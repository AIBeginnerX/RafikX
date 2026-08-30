use std::collections::HashSet;
use std::fmt;

use super::{LifecycleEventData, LifecycleState};
use crate::run::RunId;

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
    active_children: HashSet<RunId>,
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
            LifecycleEventData::ChildStarted { child_run_id, .. }
                if matches!(
                    current,
                    Some(LifecycleState::Running | LifecycleState::Delegating)
                ) && !self.active_children.contains(child_run_id) =>
            {
                self.active_children.insert(child_run_id.clone());
                LifecycleState::Delegating
            }
            LifecycleEventData::ChildFinished { child_run_id, .. }
                if current == Some(LifecycleState::Delegating)
                    && self.active_children.contains(child_run_id) =>
            {
                self.active_children.remove(child_run_id);
                if self.active_children.is_empty() {
                    LifecycleState::Running
                } else {
                    LifecycleState::Delegating
                }
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
                    && current != Some(LifecycleState::Answering)
                    && current != Some(LifecycleState::CancelRequested) =>
            {
                LifecycleState::CancelRequested
            }
            LifecycleEventData::Finished { outcome, .. }
                if current.is_some_and(|state| !state.is_terminal())
                    && (current != Some(LifecycleState::CancelRequested)
                        || outcome.state() == LifecycleState::Cancelled) =>
            {
                self.active_children.clear();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{AgentId, RunId};

    fn child_started(id: &str) -> LifecycleEventData {
        LifecycleEventData::ChildStarted {
            child_run_id: RunId::new(id),
            agent_id: AgentId::new(format!("agent-{id}")),
        }
    }

    fn child_finished(id: &str) -> LifecycleEventData {
        LifecycleEventData::ChildFinished {
            child_run_id: RunId::new(id),
            state: LifecycleState::Succeeded,
            input_tokens: 1,
            output_tokens: 1,
        }
    }

    #[test]
    fn overlapping_children_keep_parent_delegating_until_the_last_child_finishes() {
        let mut reducer = LifecycleReducer::default();
        reducer
            .apply(&LifecycleEventData::Queued { task: None })
            .expect("queue");
        reducer
            .apply(&LifecycleEventData::RunStarted { model: None })
            .expect("run");

        assert_eq!(
            reducer.apply(&child_started("one")).expect("first child"),
            LifecycleState::Delegating
        );
        assert_eq!(
            reducer.apply(&child_started("two")).expect("second child"),
            LifecycleState::Delegating
        );
        assert_eq!(
            reducer.apply(&child_finished("one")).expect("first finish"),
            LifecycleState::Delegating
        );
        assert_eq!(
            reducer.apply(&child_finished("two")).expect("last finish"),
            LifecycleState::Running
        );
    }

    #[test]
    fn unknown_child_finish_is_rejected_without_changing_delegating_state() {
        let mut reducer = LifecycleReducer::default();
        reducer
            .apply(&LifecycleEventData::Queued { task: None })
            .expect("queue");
        reducer
            .apply(&LifecycleEventData::RunStarted { model: None })
            .expect("run");
        reducer.apply(&child_started("known")).expect("child");

        reducer
            .apply(&child_started("known"))
            .expect_err("duplicate child must not be counted twice");
        assert_eq!(reducer.state(), Some(LifecycleState::Delegating));

        let error = reducer
            .apply(&child_finished("unknown"))
            .expect_err("unknown child must not complete delegation");
        assert_eq!(error.from, Some(LifecycleState::Delegating));
        assert_eq!(reducer.state(), Some(LifecycleState::Delegating));
    }

    #[test]
    fn answer_started_is_the_commit_point_for_late_cancellation() {
        let mut reducer = LifecycleReducer::default();
        reducer
            .apply(&LifecycleEventData::Queued { task: None })
            .expect("queue");
        reducer
            .apply(&LifecycleEventData::RunStarted { model: None })
            .expect("run");
        reducer
            .apply(&LifecycleEventData::AnswerStarted)
            .expect("commit answer");

        reducer
            .apply(&LifecycleEventData::CancelRequested {
                reason: "too late".into(),
            })
            .expect_err("a committed answer cannot be cancelled");
        assert_eq!(reducer.state(), Some(LifecycleState::Answering));
    }
}
