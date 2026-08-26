use std::sync::Arc;

use serde_json::json;

use crate::chat::RunObserver;
use crate::run::RunContext;

use super::protocol::notification;
use super::state::RpcState;
use super::writer::Outbound;

pub fn create(state: RpcState, outbound: Outbound) -> RunObserver {
    Arc::new(move |run: RunContext| {
        let run_id = run.run_id().to_string();
        if let Ok(mut runs) = state.runs.lock() {
            runs.insert(run_id.clone(), run.clone());
        }

        let mut lifecycle = run.subscribe_lifecycle();
        let mut events = run.subscribe();
        let lifecycle_out = outbound.clone();
        tokio::spawn(async move {
            while let Ok(event) = lifecycle.recv().await {
                if lifecycle_out
                    .send(notification("run.lifecycle", json!(event)))
                    .await
                    .is_err()
                {
                    break;
                }
                if event.state.is_terminal() && event.run_id.as_str() == run_id {
                    break;
                }
            }
        });

        let event_out = outbound.clone();
        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                let terminal = matches!(event.kind, crate::run::RunEventKind::Finish)
                    && event.run_id == *run.run_id();
                if event_out
                    .send(notification("run.event", json!(event)))
                    .await
                    .is_err()
                    || terminal
                {
                    break;
                }
            }
        });
    })
}
