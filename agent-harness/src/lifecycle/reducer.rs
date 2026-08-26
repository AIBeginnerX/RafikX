use std::fmt;

use super::{LifecycleEventData, LifecycleState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: Option<LifecycleState>,
    pub event: &'static str,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "illegal lifecycle transition from {:?} via {}",
            self.from, self.event
        )
    }
}

impl std::error::Error for IllegalTransition {}

#[derive(Debug, Clone, Default)]
pub struct LifecycleReducer {
    state: Option<LifecycleState>,
}

impl LifecycleReducer {
    pub fn state(&self) -> Option<LifecycleState> {
        self.state
    }

    pub fn apply(
        &mut self,
        event: &LifecycleEventData,
    ) -> Result<LifecycleState, IllegalTransition> {
        let current = self.state;
        let next = match event {
            LifecycleEventData::Queued { .. } if current.is_none() => LifecycleState::Queued,
            LifecycleEventData::PlanningStarted if current == Some(LifecycleState::Queued) => {
                LifecycleState::Planning
            }
            LifecycleEventData::RunStarted { .. }
                if matches!(
                    current,
                    Some(
                        LifecycleState::Queued
                            | LifecycleState::Planning
                            | LifecycleState::WaitingApproval
                            | LifecycleState::Delegating
                    )
                ) =>
            {
                LifecycleState::Running
            }
            LifecycleEventData::Iteration { .. }
            | LifecycleEventData::Tokens { .. }
            | LifecycleEventData::ToolStarted { .. }
            | LifecycleEventData::ToolFinished { .. }
                if current == Some(LifecycleState::Running) =>
            {
                LifecycleState::Running
            }
            LifecycleEventData::ApprovalRequested { .. }
                if current == Some(LifecycleState::Running) =>
            {
                LifecycleState::WaitingApproval
            }
            LifecycleEventData::ApprovalResolved { .. }
                if current == Some(LifecycleState::WaitingApproval) =>
            {
                LifecycleState::Running
            }
            LifecycleEventData::ChildStarted { .. } if current == Some(LifecycleState::Running) => {
                LifecycleState::Delegating
            }
            LifecycleEventData::ChildFinished { .. }
                if current == Some(LifecycleState::Delegating) =>
            {
                LifecycleState::Running
            }
            LifecycleEventData::AnswerStarted
                if matches!(
                    current,
                    Some(LifecycleState::Running | LifecycleState::Planning)
                ) =>
            {
                LifecycleState::Answering
            }
            LifecycleEventData::CancelRequested { .. }
                if current.is_some_and(|state| !state.is_terminal())
                    && current != Some(LifecycleState::CancelRequested) =>
            {
                LifecycleState::CancelRequested
            }
            LifecycleEventData::Finished { outcome, .. }
                if current.is_some_and(|state| !state.is_terminal())
                    && (current != Some(LifecycleState::CancelRequested)
                        || outcome.state() == LifecycleState::Cancelled) =>
            {
                outcome.state()
            }
            LifecycleEventData::Queued { .. }
            | LifecycleEventData::PlanningStarted
            | LifecycleEventData::RunStarted { .. }
            | LifecycleEventData::Iteration { .. }
            | LifecycleEventData::Tokens { .. }
            | LifecycleEventData::ToolStarted { .. }
            | LifecycleEventData::ToolFinished { .. }
            | LifecycleEventData::ApprovalRequested { .. }
            | LifecycleEventData::ApprovalResolved { .. }
            | LifecycleEventData::ChildStarted { .. }
            | LifecycleEventData::ChildFinished { .. }
            | LifecycleEventData::AnswerStarted
            | LifecycleEventData::CancelRequested { .. }
            | LifecycleEventData::Finished { .. } => {
                return Err(IllegalTransition {
                    from: current,
                    event: event.name(),
                });
            }
        };
        self.state = Some(next);
        Ok(next)
    }
}

impl LifecycleEventData {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Queued { .. } => "queued",
            Self::PlanningStarted => "planning_started",
            Self::RunStarted { .. } => "run_started",
            Self::Iteration { .. } => "iteration",
            Self::Tokens { .. } => "tokens",
            Self::ToolStarted { .. } => "tool_started",
            Self::ToolFinished { .. } => "tool_finished",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalResolved { .. } => "approval_resolved",
            Self::ChildStarted { .. } => "child_started",
            Self::ChildFinished { .. } => "child_finished",
            Self::AnswerStarted => "answer_started",
            Self::CancelRequested { .. } => "cancel_requested",
            Self::Finished { .. } => "finished",
        }
    }
}
