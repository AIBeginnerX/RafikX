use anyhow::{Result, anyhow};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use super::{Db, SessionRow, now_secs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEventRow {
    pub seq: i64,
    pub event_id: String,
    pub kind: String,
    pub payload_json: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshotRow {
    pub seq: i64,
    pub reason: String,
    pub messages_json: String,
    pub checksum: String,
    pub created_at: i64,
}

pub(super) fn save(
    connection: &Connection,
    requested_id: Option<&str>,
    title: &str,
    messages_json: &str,
) -> Result<String> {
    validate_messages(messages_json)?;
    let transaction = connection.unchecked_transaction()?;
    let id = resolve_id(&transaction, requested_id)?;
    upsert_session(&transaction, &id, title, messages_json)?;
    append_snapshot(&transaction, &id, "messages_saved", messages_json)?;
    transaction.commit()?;
    Ok(id)
}

pub(super) fn save_compaction(
    connection: &Connection,
    requested_id: Option<&str>,
    title: &str,
    source_json: &str,
    compacted_json: &str,
) -> Result<String> {
    validate_messages(source_json)?;
    validate_messages(compacted_json)?;
    let transaction = connection.unchecked_transaction()?;
    let id = resolve_id(&transaction, requested_id)?;
    append_snapshot(&transaction, &id, "compaction_source", source_json)?;
    upsert_session(&transaction, &id, title, compacted_json)?;
    append_snapshot(&transaction, &id, "compaction_result", compacted_json)?;
    transaction.commit()?;
    Ok(id)
}

pub(super) fn load(connection: &Connection, id: &str) -> Result<Option<SessionRow>> {
    let row = connection
        .query_row(
            "SELECT id, created_at, updated_at, title, messages_json FROM sessions WHERE id=?1",
            [id],
            |row| {
                Ok(SessionRow {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    updated_at: row.get(2)?,
                    title: row.get(3)?,
                    messages_json: row.get(4)?,
                })
            },
        )
        .optional()?;
    let Some(mut row) = row else {
        return Ok(None);
    };
    if validate_messages(&row.messages_json).is_ok() {
        return Ok(Some(row));
    }
    let snapshots = snapshots(connection, id)?;
    let recovered = snapshots
        .into_iter()
        .rev()
        .find(|snapshot| valid_snapshot(snapshot));
    let Some(recovered) = recovered else {
        return Err(anyhow!("session '{id}' has no valid recoverable snapshot"));
    };
    row.messages_json = recovered.messages_json;
    Ok(Some(row))
}

pub(super) fn events(connection: &Connection, id: &str) -> Result<Vec<SessionEventRow>> {
    let mut statement = connection.prepare(
        "SELECT seq, event_id, kind, payload_json, created_at FROM session_events WHERE session_id=?1 ORDER BY seq",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok(SessionEventRow {
            seq: row.get(0)?,
            event_id: row.get(1)?,
            kind: row.get(2)?,
            payload_json: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub(super) fn snapshots(connection: &Connection, id: &str) -> Result<Vec<SessionSnapshotRow>> {
    let mut statement = connection.prepare(
        "SELECT seq, reason, messages_json, checksum, created_at FROM session_snapshots WHERE session_id=?1 ORDER BY seq",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok(SessionSnapshotRow {
            seq: row.get(0)?,
            reason: row.get(1)?,
            messages_json: row.get(2)?,
            checksum: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub(super) fn restore(connection: &Connection, id: &str, seq: i64) -> Result<String> {
    let snapshot = connection
        .query_row(
            "SELECT seq, reason, messages_json, checksum, created_at FROM session_snapshots WHERE session_id=?1 AND seq=?2",
            params![id, seq],
            |row| {
                Ok(SessionSnapshotRow {
                    seq: row.get(0)?,
                    reason: row.get(1)?,
                    messages_json: row.get(2)?,
                    checksum: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("session snapshot not found: {id}@{seq}"))?;
    if !valid_snapshot(&snapshot) {
        return Err(anyhow!("session snapshot checksum is invalid: {id}@{seq}"));
    }
    let transaction = connection.unchecked_transaction()?;
    let changed = transaction.execute(
        "UPDATE sessions SET messages_json=?1, updated_at=?2 WHERE id=?3",
        params![snapshot.messages_json, now_secs(), id],
    )?;
    if changed == 0 {
        return Err(anyhow!("session not found: {id}"));
    }
    append_snapshot(&transaction, id, "restored", &snapshot.messages_json)?;
    transaction.commit()?;
    Ok(snapshot.messages_json)
}

fn resolve_id(transaction: &Transaction<'_>, requested: Option<&str>) -> Result<String> {
    if let Some(id) = requested {
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
            [id],
            |row| row.get::<_, bool>(0),
        )?;
        if exists {
            return Ok(id.to_string());
        }
    }
    Ok(Db::new_id())
}

fn upsert_session(
    transaction: &Transaction<'_>,
    id: &str,
    title: &str,
    messages_json: &str,
) -> Result<()> {
    let now = now_secs();
    transaction.execute(
        "INSERT INTO sessions(id, created_at, updated_at, title, messages_json) VALUES (?1, ?2, ?2, ?3, ?4) ON CONFLICT(id) DO UPDATE SET updated_at=excluded.updated_at, title=excluded.title, messages_json=excluded.messages_json",
        params![id, now, title, messages_json],
    )?;
    Ok(())
}

fn append_snapshot(
    transaction: &Transaction<'_>,
    id: &str,
    reason: &str,
    messages_json: &str,
) -> Result<i64> {
    let seq = transaction.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM session_events WHERE session_id=?1",
        [id],
        |row| row.get::<_, i64>(0),
    )?;
    let checksum = checksum(messages_json);
    let payload = serde_json::json!({
        "snapshot_seq": seq,
        "checksum": checksum,
        "bytes": messages_json.len(),
    });
    let now = now_secs();
    transaction.execute(
        "INSERT INTO session_events(session_id, seq, event_id, kind, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, seq, Db::new_id(), reason, payload.to_string(), now],
    )?;
    transaction.execute(
        "INSERT INTO session_snapshots(session_id, seq, reason, messages_json, checksum, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, seq, reason, messages_json, checksum, now],
    )?;
    Ok(seq)
}

fn valid_snapshot(snapshot: &SessionSnapshotRow) -> bool {
    checksum(&snapshot.messages_json) == snapshot.checksum
        && validate_messages(&snapshot.messages_json).is_ok()
}

fn validate_messages(messages_json: &str) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(messages_json)?;
    if !value.is_array() {
        return Err(anyhow!("session messages must be a JSON array"));
    }
    Ok(())
}

fn checksum(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
