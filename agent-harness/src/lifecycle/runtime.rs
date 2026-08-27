use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast;

use crate::run::{AgentId, RunId};

use super::store::LifecycleStore;
use super::{
    IllegalTransition, LIFECYCLE_SCHEMA, LifecycleEvent, LifecycleEventData, LifecycleReducer,
    LifecycleState,
};

const EVENT_CAPACITY: usize = 256;

#[derive(Clone)]
struct LifecycleBus {
    sender: broadcast::Sender<LifecycleEvent>,
    history: Arc<Mutex<Vec<LifecycleEvent>>>,
}

impl LifecycleBus {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            sender,
            history: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn publish(&self, event: LifecycleEvent) {
        if let Ok(mut history) = self.history.lock() {
            history.push(event.clone());
        }
        let _ = self.sender.send(event);
    }

    fn history(&self) -> Vec<LifecycleEvent> {
        self.history
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default()
    }
}

pub(crate) struct LifecycleRuntime {
    run_id: RunId,
    parent_run_id: Option<RunId>,
    agent_id: Option<AgentId>,
    reducer: Mutex<LifecycleReducer>,
    sequence: AtomicU64,
    bus: LifecycleBus,
    store: Option<LifecycleStore>,
    persistence_error: Mutex<Option<String>>,
}

impl LifecycleRuntime {
    pub(crate) fn root(run_id: RunId, store: Option<LifecycleStore>) -> Arc<Self> {
        Self::new(run_id, None, None, LifecycleBus::new(), store)
    }

    pub(crate) fn child(&self, run_id: RunId, agent_id: AgentId) -> Arc<Self> {
        Self::new(
            run_id,
            Some(self.run_id.clone()),
            Some(agent_id),
            self.bus.clone(),
            self.store.clone(),
        )
    }

    fn new(
        run_id: RunId,
        parent_run_id: Option<RunId>,
        agent_id: Option<AgentId>,
        bus: LifecycleBus,
        store: Option<LifecycleStore>,
    ) -> Arc<Self> {
        let runtime = Arc::new(Self {
            run_id,
            parent_run_id,
            agent_id,
            reducer: Mutex::new(LifecycleReducer::default()),
            sequence: AtomicU64::new(1),
            bus,
            store,
            persistence_error: Mutex::new(None),
        });
        let _ = runtime.transition(LifecycleEventData::Queued { task: None });
        runtime
    }

    pub(crate) fn transition(
        &self,
        data: LifecycleEventData,
    ) -> Result<LifecycleEvent, IllegalTransition> {
        let mut reducer = self.reducer.lock().map_err(|_| IllegalTransition {
            from: self.state(),
            event: data.name(),
        })?;
        let state = reducer.apply(&data)?;
        let event = LifecycleEvent {
            schema: LIFECYCLE_SCHEMA.into(),
            seq: self.sequence.fetch_add(1, Ordering::Relaxed),
            timestamp_ms: now_ms(),
            run_id: self.run_id.clone(),
            parent_run_id: self.parent_run_id.clone(),
            agent_id: self.agent_id.clone(),
            state,
            event: data,
        };
        if let Some(store) = &self.store
            && let Err(error) = store.append(&event)
            && let Ok(mut slot) = self.persistence_error.lock()
        {
            *slot = Some(error.to_string());
        }
        self.bus.publish(event.clone());
        Ok(event)
    }

    pub(crate) fn state(&self) -> Option<LifecycleState> {
        self.reducer.lock().ok().and_then(|reducer| reducer.state())
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<LifecycleEvent> {
        self.bus.sender.subscribe()
    }

    pub(crate) fn history(&self) -> Vec<LifecycleEvent> {
        self.bus.history()
    }

    pub(crate) fn persistence_error(&self) -> Option<String> {
        self.persistence_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    pub(crate) fn stored_events(&self) -> Vec<LifecycleEvent> {
        self.store
            .as_ref()
            .and_then(|store| store.load(&self.run_id).ok())
            .unwrap_or_default()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}
