use std::fs;

use rafikx::db::Db;
use rafikx::lifecycle::{ApprovalDecision, LifecycleEventData, LifecycleState, replay};
use rafikx::run::{FinishResult, RunContext, RunId, TerminalState};

#[test]
fn lifecycle_replay_matches_live_reducer() {
    let workspace = std::env::temp_dir().join(format!("rafikx-life-{}", Db::new_id()));
    fs::create_dir_all(&workspace).expect("create lifecycle workspace");
    let run = RunContext::isolated(RunId::new("run-life"), workspace.clone());
    run.transition_lifecycle(LifecycleEventData::PlanningStarted)
        .expect("planning");
    run.transition_lifecycle(LifecycleEventData::RunStarted {
        model: Some("test-model".into()),
    })
    .expect("running");
    run.transition_lifecycle(LifecycleEventData::ApprovalRequested {
        approval_id: "approval-1".into(),
        preview: "write file".into(),
    })
    .expect("waiting approval");
    run.transition_lifecycle(LifecycleEventData::ApprovalResolved {
        approval_id: "approval-1".into(),
        decision: ApprovalDecision::Yes,
    })
    .expect("resume running");
    run.transition_lifecycle(LifecycleEventData::AnswerStarted)
        .expect("answering");
    assert_eq!(
        run.finish(TerminalState::Succeeded),
        FinishResult::Finished(TerminalState::Succeeded)
    );

    let live = run.lifecycle_events();
    assert_eq!(
        replay(&live).expect("replay live events"),
        LifecycleState::Succeeded
    );
    let db = Db::open(&workspace.join("data.db")).expect("open lifecycle database");
    for event in &live {
        db.append_lifecycle_event(event)
            .expect("persist lifecycle event");
    }
    let stored = db
        .lifecycle_events(&RunId::new("run-life"))
        .expect("load lifecycle events");
    assert_eq!(stored, live);
    assert_eq!(
        replay(&stored).expect("replay stored events"),
        LifecycleState::Succeeded
    );
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn lifecycle_cancelled_cannot_succeed() {
    let run = RunContext::isolated(
        RunId::new("run-cancel"),
        std::env::temp_dir().join("rafikx-life-cancel"),
    );
    run.transition_lifecycle(LifecycleEventData::RunStarted { model: None })
        .expect("running");
    assert!(run.cancel("user requested"));
    assert_eq!(
        run.finish(TerminalState::Succeeded),
        FinishResult::Finished(TerminalState::Cancelled)
    );
    assert_eq!(run.lifecycle_state(), Some(LifecycleState::Cancelled));
    assert_eq!(
        run.finish(TerminalState::Succeeded),
        FinishResult::AlreadyFinished(TerminalState::Cancelled)
    );
}
