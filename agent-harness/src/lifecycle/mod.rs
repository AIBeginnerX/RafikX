mod event;
mod reducer;
mod runtime;
mod state;
mod store;

use anyhow::{Result, anyhow};

pub use event::{ApprovalDecision, LIFECYCLE_SCHEMA, LifecycleEvent, LifecycleEventData};
pub use reducer::{IllegalTransition, LifecycleReducer};
pub(crate) use runtime::LifecycleRuntime;
pub use state::{LifecycleOutcome, LifecycleState};
pub(crate) use store::LifecycleStore;

pub fn replay(events: &[LifecycleEvent]) -> Result<LifecycleState> {
    let mut reducer = LifecycleReducer::default();
    let mut sequence = 0;
    let run_id = events.first().map(|event| event.run_id.clone());
    for event in events {
        if event.seq <= sequence {
            return Err(anyhow!("lifecycle event sequence is not strictly ordered"));
        }
        if run_id.as_ref() != Some(&event.run_id) {
            return Err(anyhow!("lifecycle replay mixes run identities"));
        }
        let state = reducer.apply(&event.event)?;
        if state != event.state {
            return Err(anyhow!("lifecycle event state does not match reducer"));
        }
        sequence = event.seq;
    }
    reducer
        .state()
        .ok_or_else(|| anyhow!("lifecycle replay is empty"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{AgentId, RunId};

    #[test]
    fn lifecycle_reducer_rejects_success_after_cancel_request() {
        let mut reducer = LifecycleReducer::default();
        reducer
            .apply(&LifecycleEventData::Queued { task: None })
            .expect("queue");
        reducer
            .apply(&LifecycleEventData::RunStarted { model: None })
            .expect("start");
        reducer
            .apply(&LifecycleEventData::CancelRequested {
                reason: "user".into(),
            })
            .expect("request cancel");
        let error = reducer
            .apply(&LifecycleEventData::Finished {
                outcome: LifecycleOutcome::Succeeded,
                error: None,
            })
            .expect_err("cancelled run cannot succeed");
        assert_eq!(error.from, Some(LifecycleState::CancelRequested));
        assert_eq!(reducer.state(), Some(LifecycleState::CancelRequested));
    }

    #[test]
    fn lifecycle_event_roundtrip_is_tagged_and_lossless() {
        let event = LifecycleEvent {
            schema: LIFECYCLE_SCHEMA.into(),
            seq: 4,
            timestamp_ms: 10,
            run_id: RunId::new("run-1"),
            parent_run_id: Some(RunId::new("parent")),
            agent_id: Some(AgentId::new("agent-1")),
            state: LifecycleState::Delegating,
            event: LifecycleEventData::ChildStarted {
                child_run_id: RunId::new("child"),
                agent_id: AgentId::new("agent-1"),
            },
        };
        let value = serde_json::to_value(&event).expect("serialize lifecycle event");
        assert_eq!(value["event"]["type"], "child_started");
        assert_eq!(
            serde_json::from_value::<LifecycleEvent>(value).expect("deserialize lifecycle event"),
            event
        );
    }
}
