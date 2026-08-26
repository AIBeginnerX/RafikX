use anyhow::Result;

use crate::lifecycle::{LifecycleEvent, LifecycleState};
use crate::run::RunId;

use super::Db;

impl Db {
    pub fn append_lifecycle_event(&self, event: &LifecycleEvent) -> Result<()> {
        self.conn.execute(
            "INSERT INTO lifecycle_events (
               run_id, seq, schema, timestamp_ms, parent_run_id, agent_id, state, event_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                event.run_id.as_str(),
                event.seq as i64,
                event.schema,
                event.timestamp_ms as i64,
                event.parent_run_id.as_ref().map(RunId::as_str),
                event.agent_id.as_ref().map(|id| id.as_str()),
                serde_json::to_value(event.state)?
                    .as_str()
                    .unwrap_or("failed"),
                serde_json::to_string(event)?,
            ],
        )?;
        Ok(())
    }

    pub fn lifecycle_events(&self, run_id: &RunId) -> Result<Vec<LifecycleEvent>> {
        let mut statement = self
            .conn
            .prepare("SELECT event_json FROM lifecycle_events WHERE run_id=?1 ORDER BY seq ASC")?;
        let rows = statement.query_map([run_id.as_str()], |row| row.get::<_, String>(0))?;
        let mut events = Vec::new();
        for row in rows {
            events.push(serde_json::from_str(&row?)?);
        }
        Ok(events)
    }

    pub fn lifecycle_state(&self, run_id: &RunId) -> Result<Option<LifecycleState>> {
        let event = self.lifecycle_events(run_id)?.into_iter().next_back();
        Ok(event.map(|event| event.state))
    }
}
