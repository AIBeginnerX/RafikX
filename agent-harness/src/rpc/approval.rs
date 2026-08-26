use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::oneshot;

use crate::agent::{ApprovalChoice, LocalAsk};
use crate::db::Db;

use super::protocol::notification;
use super::state::RpcState;
use super::writer::Outbound;

pub fn broker(state: RpcState, outbound: Outbound) -> LocalAsk {
    Arc::new(move |preview: String| {
        let state = state.clone();
        let outbound = outbound.clone();
        Box::pin(async move {
            let approval_id = format!("approval-{}", Db::new_id());
            let (sender, receiver) = oneshot::channel();
            if let Ok(mut approvals) = state.approvals.lock() {
                approvals.insert(approval_id.clone(), sender);
            }
            let sent = outbound
                .send(notification(
                    "approval.requested",
                    json!({"approval_id": approval_id, "preview": preview}),
                ))
                .await
                .is_ok();
            if !sent {
                remove(&state, &approval_id);
                return ApprovalChoice::No;
            }
            let choice = match tokio::time::timeout(Duration::from_secs(300), receiver).await {
                Ok(Ok(choice)) => choice,
                _ => ApprovalChoice::No,
            };
            remove(&state, &approval_id);
            choice
        })
    })
}

fn remove(state: &RpcState, id: &str) {
    if let Ok(mut approvals) = state.approvals.lock() {
        approvals.remove(id);
    }
}
