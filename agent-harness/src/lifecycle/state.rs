use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Queued,
    Planning,
    Running,
    WaitingApproval,
    Delegating,
    Answering,
    Succeeded,
    Limited,
    Failed,
    CancelRequested,
    Cancelled,
}

impl LifecycleState {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Limited | Self::Failed | Self::Cancelled
        )
    }

    pub const fn legacy_status(self) -> &'static str {
        match self {
            Self::Queued
            | Self::Planning
            | Self::Running
            | Self::WaitingApproval
            | Self::Delegating
            | Self::Answering => "running",
            Self::Succeeded => "ok",
            Self::Limited => "limit",
            Self::Failed => "fail",
            Self::CancelRequested => "cancelling",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleOutcome {
    Succeeded,
    Limited,
    Failed,
    Cancelled,
}

impl LifecycleOutcome {
    pub const fn state(self) -> LifecycleState {
        match self {
            Self::Succeeded => LifecycleState::Succeeded,
            Self::Limited => LifecycleState::Limited,
            Self::Failed => LifecycleState::Failed,
            Self::Cancelled => LifecycleState::Cancelled,
        }
    }
}
