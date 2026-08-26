use std::path::PathBuf;

use rafikx::run::{
    AgentId, ContextSourceKind, FinishResult, RunContext, RunEventKind, RunId, TerminalState,
};
use serde_json::json;

#[test]
fn run_context_interleaved_events_stay_isolated() {
    let first = RunContext::isolated(RunId::from("run-a"), PathBuf::from("/tmp/rafikx-run-a"));
    let second = RunContext::isolated(RunId::from("run-b"), PathBuf::from("/tmp/rafikx-run-b"));
    let mut first_events = first.subscribe();
    let mut second_events = second.subscribe();

    first.emit(RunEventKind::Status, json!({"label": "planning"}));
    second.emit(RunEventKind::Status, json!({"label": "running"}));
    first.emit(RunEventKind::Context, json!({"tokens": 1024}));

    let first_one = first_events.try_recv().expect("first status event");
    let first_two = first_events.try_recv().expect("first context event");
    let second_one = second_events.try_recv().expect("second status event");

    assert_eq!(first_one.run_id.as_str(), "run-a");
    assert_eq!(first_two.run_id.as_str(), "run-a");
    assert_eq!(second_one.run_id.as_str(), "run-b");
    assert!(first_events.try_recv().is_err());
    assert!(second_events.try_recv().is_err());
}

#[tokio::test]
async fn run_context_parent_cancel_reaches_child_without_sibling_leak() {
    let parent = RunContext::isolated(RunId::from("parent"), PathBuf::from("/tmp/rafikx-parent"));
    let child = parent.child(RunId::from("child"), AgentId::from("agent-1"));
    let unrelated = RunContext::isolated(
        RunId::from("unrelated"),
        PathBuf::from("/tmp/rafikx-unrelated"),
    );

    assert!(parent.cancel("user interrupted"));
    assert_eq!(child.cancelled_reason().await, "user interrupted");
    assert!(!unrelated.is_cancelled());
}

#[test]
fn run_context_cancel_then_finish_is_idempotent() {
    let context = RunContext::isolated(
        RunId::from("run-cancel"),
        PathBuf::from("/tmp/rafikx-run-cancel"),
    );

    assert!(context.cancel("escape"));
    assert_eq!(
        context.finish(TerminalState::Cancelled),
        FinishResult::Finished(TerminalState::Cancelled)
    );
    assert_eq!(
        context.finish(TerminalState::Succeeded),
        FinishResult::AlreadyFinished(TerminalState::Cancelled)
    );
    assert_eq!(context.terminal_state(), Some(TerminalState::Cancelled));
}

#[test]
fn subagent_identity_events_and_approval_scope_are_inherited() {
    let parent = RunContext::isolated(
        RunId::from("parent-run"),
        PathBuf::from("/tmp/rafikx-subagent"),
    );
    parent
        .transition_lifecycle(rafikx::lifecycle::LifecycleEventData::RunStarted {
            model: Some("parent-model".into()),
        })
        .expect("parent running");
    parent.approve_run_tree();

    let child = parent.child(RunId::from("child-run"), AgentId::from("agent-review"));
    parent
        .transition_lifecycle(rafikx::lifecycle::LifecycleEventData::ChildStarted {
            child_run_id: child.run_id().clone(),
            agent_id: child.agent_id().expect("child agent").clone(),
        })
        .expect("delegating");
    assert!(child.run_tree_approved());
    assert_eq!(child.parent_run_id(), Some(parent.run_id()));
    assert_ne!(child.run_id(), parent.run_id());

    let events = parent.lifecycle_events();
    assert!(events.iter().any(|event| {
        event.run_id == *parent.run_id()
            && matches!(
                event.event,
                rafikx::lifecycle::LifecycleEventData::ChildStarted { .. }
            )
    }));
    assert!(events.iter().any(|event| {
        event.run_id == *child.run_id()
            && event.parent_run_id.as_ref() == Some(parent.run_id())
            && event.agent_id.as_ref() == child.agent_id()
    }));
}

#[test]
fn context_source_accounting_is_ordered_and_run_local() {
    let parent = RunContext::isolated(
        RunId::from("context-parent"),
        PathBuf::from("/tmp/rafikx-context"),
    );
    let child = parent.child(RunId::from("context-child"), AgentId::from("agent-context"));
    parent.record_context_source(ContextSourceKind::System, "system", 100, 80);
    parent.record_context_source(ContextSourceKind::ProjectRules, "AGENTS.md", 40, 20);
    child.record_context_source(ContextSourceKind::Lsp, "rust-analyzer", 20, 8);

    let parent_sources = parent.context_sources();
    assert_eq!(parent_sources.len(), 2);
    assert_eq!(parent_sources[0].source_id, "system");
    assert_eq!(parent_sources[1].source_id, "AGENTS.md");
    let child_sources = child.context_sources();
    assert_eq!(child_sources.len(), 1);
    assert_eq!(child_sources[0].kind, ContextSourceKind::Lsp);
}
