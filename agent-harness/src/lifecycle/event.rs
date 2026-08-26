use serde::{Deserialize, Serialize};

use crate::run::{AgentId, RunId};

use super::{LifecycleOutcome, LifecycleState};

pub const LIFECYCLE_SCHEMA: &str = "rafikx.lifecycle.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Yes,
    No,
    Always,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum LifecycleEventData {
    Queued {
        task: Option<String>,
    },
    PlanningStarted,
    RunStarted {
        model: Option<String>,
    },
    Iteration {
        current: u32,
        max: u32,
    },
    Tokens {
        input: u32,
        output: u32,
        cached: u32,
    },
    ToolStarted {
        name: String,
    },
    ToolFinished {
        name: String,
        ok: bool,
    },
    ApprovalRequested {
        approval_id: String,
        preview: String,
    },
    ApprovalResolved {
        approval_id: String,
        decision: ApprovalDecision,
    },
    ChildStarted {
        child_run_id: RunId,
        agent_id: AgentId,
    },
    ChildFinished {
        child_run_id: RunId,
        state: LifecycleState,
        input_tokens: u32,
        output_tokens: u32,
    },
    AnswerStarted,
    CancelRequested {
        reason: String,
    },
    Finished {
        outcome: LifecycleOutcome,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleEvent {
    pub schema: String,
    pub seq: u64,
    pub timestamp_ms: u64,
    pub run_id: RunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    pub state: LifecycleState,
    pub event: LifecycleEventData,
}
