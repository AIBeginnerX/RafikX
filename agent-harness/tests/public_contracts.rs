use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rafikx::agent::{AgentOutcome, LocalAsk, RemoteApproval};
use rafikx::api::{SlashResult, TurnRequest, TurnResult};
use rafikx::config::Config;
use rafikx::graph::GraphNode;
use rafikx::harness::{Binding, run_pipeline};
use rafikx::provider::Message;
use rafikx::run::{RunContext, RunId};
use rafikx::tools::ToolRegistry;
use rafikx::tools_more::{ApplyPatch, PatchOp};
use rafikx::ui::{AgentProgress, Live};
use serde_json::{Value, json};

#[allow(dead_code)]
#[allow(clippy::too_many_arguments)] // 레거시 공개 계약 시그니처 고정 검증용
fn legacy_run_pipeline_call<'a>(
    cfg: &'a Config,
    binding: &'a Binding,
    task: &'a str,
    yes: bool,
    cli_provider: Option<&'a str>,
    resume: Option<Vec<Message>>,
    remote: Option<RemoteApproval>,
    local_ask: Option<LocalAsk>,
) -> impl Future<Output = Result<AgentOutcome>> + 'a {
    run_pipeline(
        cfg,
        binding,
        task,
        yes,
        cli_provider,
        resume,
        remote,
        local_ask,
    )
}

fn assert_required_keys(value: &Value, required: &[&str]) {
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("expected object, got {value}"));
    for key in required {
        assert!(object.contains_key(*key), "missing legacy key: {key}");
    }
}

#[test]
fn legacy_api_contracts_keep_required_json_fields() {
    let turn = TurnResult {
        run_id: "run-1".into(),
        label: "dev -> coder".into(),
        status: "ok".into(),
        tokens_in: 10,
        tokens_out: 5,
        elapsed_ms: 12,
        session_id: Some("session-1".into()),
        graph: vec![GraphNode {
            seq: 1,
            kind: "request".into(),
            label: "model".into(),
            detail: "in=10 out=5".into(),
            parent: Some("pre_step".into()),
        }],
        lifecycle_state: None,
        lifecycle: Vec::new(),
        context_sources: Vec::new(),
    };
    let turn_json = serde_json::to_value(turn).expect("TurnResult serializes");
    assert_required_keys(
        &turn_json,
        &[
            "run_id",
            "label",
            "status",
            "tokens_in",
            "tokens_out",
            "elapsed_ms",
            "session_id",
            "graph",
        ],
    );
    assert_required_keys(
        &turn_json["graph"][0],
        &["seq", "kind", "label", "detail", "parent"],
    );

    let slash = SlashResult {
        notes: "saved".into(),
        quit: false,
        agent_task: None,
        ulw_goal: None,
        ulw_resume: None,
        compact: false,
        assign: false,
        model_fetch: false,
    };
    let slash_json = serde_json::to_value(slash).expect("SlashResult serializes");
    assert_required_keys(
        &slash_json,
        &[
            "notes",
            "quit",
            "agent_task",
            "compact",
            "assign",
            "model_fetch",
        ],
    );
}

#[test]
fn legacy_turn_request_accepts_additive_fields_without_losing_meaning() {
    let request: TurnRequest = serde_json::from_value(json!({
        "prompt": "inspect",
        "provider": "openai",
        "model": "model-1",
        "class": "dev",
        "obsidian": true,
        "session_id": "session-1",
        "yes": false,
        "future_optional_field": {"schema": 2}
    }))
    .expect("additive request fields remain compatible");

    assert_eq!(request.prompt, "inspect");
    assert_eq!(request.provider.as_deref(), Some("openai"));
    assert_eq!(request.model.as_deref(), Some("model-1"));
    assert_eq!(request.class.as_deref(), Some("dev"));
    assert!(request.obsidian);
    assert_eq!(request.session_id.as_deref(), Some("session-1"));
    assert!(!request.yes);
}

#[test]
fn legacy_live_variants_and_task_schema_remain_available() {
    fn kind(event: Live) -> &'static str {
        match event {
            Live::Chunk(_) => "chunk",
            Live::Assistant(_) => "assistant",
            Live::System(_) => "system",
            Live::Warn(_) => "warn",
            Live::Status(_) => "status",
            Live::Todo(_) => "todo",
            Live::Agent(_) => "agent",
            Live::Mode(_) => "mode",
        }
    }

    assert_eq!(kind(Live::Chunk("x".into())), "chunk");
    assert_eq!(
        kind(Live::Agent(AgentProgress {
            id: "agent-1".into(),
            role: "reviewer".into(),
            model: "model-1".into(),
            activity: "완료 기준 대조".into(),
            done: false,
        })),
        "agent"
    );

    let registry = ToolRegistry::all();
    let task = registry.get("task").expect("task tool remains registered");
    let schema = task.input_schema();
    assert_eq!(schema["required"], json!(["prompt"]));
    assert!(schema["properties"].get("class").is_some());
    assert!(schema["properties"].get("role").is_some());
    assert!(schema["properties"].get("model").is_some());
}

#[test]
fn legacy_change_and_patch_methods_keep_the_v118_signatures() {
    let dry_run: fn(&Path, &[PatchOp]) -> Result<String> = ApplyPatch::dry_run;
    let _ = dry_run;
    let smoke = rafikx::quality::browser::smoke_test(Path::new("index.html"));
    drop(smoke);

    let root = std::env::temp_dir().join(format!("rafikx-public-contract-{}", std::process::id()));
    let run = RunContext::isolated(RunId::new("public-contract"), root);
    let changed = PathBuf::from("legacy.txt");
    run.record_committed_paths([changed.clone()]);
    assert_eq!(run.committed_paths(), vec![changed]);
}
