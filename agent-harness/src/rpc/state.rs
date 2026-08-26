use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, oneshot};

use crate::agent::ApprovalChoice;
use crate::chat::Session;
use crate::run::RunContext;

#[derive(Clone, Default)]
pub struct RpcState {
    pub sessions: Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<Session>>>>>,
    pub runs: Arc<Mutex<HashMap<String, RunContext>>>,
    pub approvals: Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalChoice>>>>,
}

impl RpcState {
    pub fn cancel_all(&self, reason: &str) {
        if let Ok(runs) = self.runs.lock() {
            for run in runs.values() {
                run.cancel(reason);
            }
        }
        if let Ok(mut approvals) = self.approvals.lock() {
            for (_, sender) in approvals.drain() {
                let _ = sender.send(ApprovalChoice::No);
            }
        }
    }
}
