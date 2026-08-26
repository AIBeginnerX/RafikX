use serde_json::json;

use super::RunContext;
use crate::lifecycle::{
    IllegalTransition, LifecycleEvent, LifecycleEventData, LifecycleOutcome, LifecycleState,
};
use crate::run::{FinishResult, RunEventKind, TerminalState};

impl RunContext {
    pub fn cancel(&self, reason: impl Into<String>) -> bool {
        let reason = reason.into();
        let cancelled = self.control.cancel(reason.clone());
        if cancelled {
            let _ = self
                .lifecycle
                .transition(LifecycleEventData::CancelRequested {
                    reason: reason.clone(),
                });
            self.emit(RunEventKind::Cancel, json!({"reason": reason}));
        }
        cancelled
    }

    pub fn is_cancelled(&self) -> bool {
        self.control.is_cancelled()
    }

    pub async fn cancelled_reason(&self) -> String {
        self.control.cancelled_reason().await
    }

    pub fn finish(&self, state: TerminalState) -> FinishResult {
        self.finish_with_error(state, None)
    }

    pub fn finish_with_error(&self, state: TerminalState, error: Option<String>) -> FinishResult {
        let state = if self.lifecycle.state() == Some(LifecycleState::CancelRequested) {
            TerminalState::Cancelled
        } else {
            state
        };
        let result = self.control.finish(state);
        if matches!(result, FinishResult::Finished(_)) {
            let outcome = match state {
                TerminalState::Succeeded => LifecycleOutcome::Succeeded,
                TerminalState::Limited => LifecycleOutcome::Limited,
                TerminalState::Failed => LifecycleOutcome::Failed,
                TerminalState::Cancelled => LifecycleOutcome::Cancelled,
            };
            let _ = self
                .lifecycle
                .transition(LifecycleEventData::Finished { outcome, error });
            self.emit(
                RunEventKind::Finish,
                json!({"state": format!("{state:?}").to_ascii_lowercase()}),
            );
        }
        result
    }

    pub fn transition_lifecycle(
        &self,
        event: LifecycleEventData,
    ) -> Result<LifecycleEvent, IllegalTransition> {
        self.lifecycle.transition(event)
    }

    pub fn lifecycle_state(&self) -> Option<LifecycleState> {
        self.lifecycle.state()
    }

    pub fn lifecycle_events(&self) -> Vec<LifecycleEvent> {
        self.lifecycle.history()
    }

    pub fn stored_lifecycle_events(&self) -> Vec<LifecycleEvent> {
        self.lifecycle.stored_events()
    }

    pub fn subscribe_lifecycle(&self) -> tokio::sync::broadcast::Receiver<LifecycleEvent> {
        self.lifecycle.subscribe()
    }

    pub fn lifecycle_persistence_error(&self) -> Option<String> {
        self.lifecycle.persistence_error()
    }

    pub fn terminal_state(&self) -> Option<TerminalState> {
        self.control.terminal_state()
    }
}
