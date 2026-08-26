use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use super::{AgentId, RunId};

const EVENT_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunEventKind {
    Status,
    Context,
    Graph,
    Live,
    Todo,
    Approval,
    Mutation,
    Plan,
    Provider,
    Tool,
    Child,
    Cancel,
    Finish,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEvent {
    pub schema: String,
    pub seq: u64,
    pub timestamp_ms: u64,
    pub run_id: RunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    pub kind: RunEventKind,
    pub payload: Value,
}

pub type EventReceiver = broadcast::Receiver<RunEvent>;
pub type EventTap = Arc<dyn Fn(&RunEvent) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct EventBus {
    sender: broadcast::Sender<RunEvent>,
    sequence: Arc<AtomicU64>,
    taps: Arc<Vec<EventTap>>,
}

impl EventBus {
    pub(crate) fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            sender,
            sequence: Arc::new(AtomicU64::new(1)),
            taps: Arc::new(Vec::new()),
        }
    }

    pub(crate) fn with_tap(mut self, tap: EventTap) -> Self {
        let mut taps = self.taps.as_ref().clone();
        taps.push(tap);
        self.taps = Arc::new(taps);
        self
    }

    pub(crate) fn subscribe(&self) -> EventReceiver {
        self.sender.subscribe()
    }

    pub(crate) fn emit(
        &self,
        run_id: RunId,
        parent_run_id: Option<RunId>,
        agent_id: Option<AgentId>,
        kind: RunEventKind,
        payload: Value,
    ) {
        let event = RunEvent {
            schema: "rafikx.run.v1".into(),
            seq: self.sequence.fetch_add(1, Ordering::Relaxed),
            timestamp_ms: now_ms(),
            run_id,
            parent_run_id,
            agent_id,
            kind,
            payload,
        };
        for tap in self.taps.iter() {
            tap(&event);
        }
        let _ = self.sender.send(event);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}
