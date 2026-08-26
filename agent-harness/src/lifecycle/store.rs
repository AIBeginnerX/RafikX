use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};

use crate::db::Db;
use crate::run::RunId;

use super::LifecycleEvent;

#[derive(Clone)]
pub(crate) struct LifecycleStore {
    db: Arc<Mutex<Db>>,
}

impl LifecycleStore {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            db: Arc::new(Mutex::new(Db::open(path)?)),
        })
    }

    pub(crate) fn append(&self, event: &LifecycleEvent) -> Result<()> {
        self.db
            .lock()
            .map_err(|_| anyhow!("lifecycle store lock is poisoned"))?
            .append_lifecycle_event(event)
    }

    pub(crate) fn load(&self, run_id: &RunId) -> Result<Vec<LifecycleEvent>> {
        self.db
            .lock()
            .map_err(|_| anyhow!("lifecycle store lock is poisoned"))?
            .lifecycle_events(run_id)
    }
}
