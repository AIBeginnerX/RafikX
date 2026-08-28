use anyhow::{Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "run_context_metadata",
        sql: r#"
        CREATE TABLE IF NOT EXISTS run_metadata (
          run_id TEXT PRIMARY KEY,
          parent_run_id TEXT,
          agent_id TEXT,
          schema TEXT NOT NULL DEFAULT 'rafikx.run.v1',
          updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_run_metadata_parent
          ON run_metadata(parent_run_id, updated_at);
    "#,
    },
    Migration {
        version: 2,
        name: "append_only_session_streams",
        sql: r#"
        CREATE TABLE IF NOT EXISTS session_events (
          session_id TEXT NOT NULL,
          seq INTEGER NOT NULL,
          event_id TEXT NOT NULL UNIQUE,
          kind TEXT NOT NULL,
          payload_json TEXT NOT NULL DEFAULT '{}',
          created_at INTEGER NOT NULL,
          PRIMARY KEY(session_id, seq)
        );
        CREATE TABLE IF NOT EXISTS session_snapshots (
          session_id TEXT NOT NULL,
          seq INTEGER NOT NULL,
          reason TEXT NOT NULL,
          messages_json TEXT NOT NULL,
          checksum TEXT NOT NULL,
          created_at INTEGER NOT NULL,
          PRIMARY KEY(session_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_session_events_created
          ON session_events(session_id, created_at, seq);
        CREATE INDEX IF NOT EXISTS idx_session_snapshots_reason
          ON session_snapshots(session_id, reason, seq);
        "#,
    },
    Migration {
        version: 3,
        name: "project_scoped_memory",
        sql: r#"
        CREATE TABLE IF NOT EXISTS projects (
          id TEXT PRIMARY KEY,
          canonical_path TEXT NOT NULL UNIQUE,
          created_at INTEGER NOT NULL,
          updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS project_lessons (
          project_id TEXT NOT NULL,
          lesson_id INTEGER NOT NULL,
          created_at INTEGER NOT NULL,
          PRIMARY KEY(project_id, lesson_id)
        );
        CREATE INDEX IF NOT EXISTS idx_project_lessons_lesson
          ON project_lessons(lesson_id, project_id);
        "#,
    },
    Migration {
        version: 4,
        name: "typed_lifecycle_events",
        sql: r#"
        CREATE TABLE IF NOT EXISTS lifecycle_events (
          run_id TEXT NOT NULL,
          seq INTEGER NOT NULL,
          schema TEXT NOT NULL,
          timestamp_ms INTEGER NOT NULL,
          parent_run_id TEXT,
          agent_id TEXT,
          state TEXT NOT NULL,
          event_json TEXT NOT NULL,
          PRIMARY KEY(run_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_lifecycle_parent
          ON lifecycle_events(parent_run_id, timestamp_ms, seq);
        "#,
    },
    Migration {
        version: 5,
        name: "facts_memory",
        sql: r#"
        CREATE TABLE IF NOT EXISTS facts (
          id INTEGER PRIMARY KEY,
          project_id TEXT NOT NULL DEFAULT '',
          kind TEXT NOT NULL,
          key TEXT NOT NULL,
          value TEXT NOT NULL,
          source TEXT NOT NULL,
          created_at INTEGER NOT NULL,
          updated_at INTEGER NOT NULL,
          last_hit INTEGER NOT NULL DEFAULT 0,
          hits INTEGER NOT NULL DEFAULT 0,
          UNIQUE(project_id, key)
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
          key, value, fact_id UNINDEXED
        );
        "#,
    },
    Migration {
        version: 6,
        name: "facts_fts_trigram",
        sql: r#"
        DROP TABLE IF EXISTS facts_fts;
        CREATE VIRTUAL TABLE facts_fts USING fts5(
          key, value, fact_id UNINDEXED, tokenize='trigram'
        );
        INSERT INTO facts_fts (key, value, fact_id)
          SELECT key, value, id FROM facts;
        "#,
    },
];

pub(super) fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_migrations (
          version INTEGER PRIMARY KEY,
          name TEXT NOT NULL,
          checksum TEXT NOT NULL,
          applied_at INTEGER NOT NULL
        );
        "#,
    )?;
    for migration in MIGRATIONS {
        apply_one(connection, migration)?;
    }
    Ok(())
}

pub(super) fn current_version(connection: &Connection) -> Result<i64> {
    connection
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn apply_one(connection: &Connection, migration: &Migration) -> Result<()> {
    let checksum = checksum(migration.sql);
    let existing = connection
        .query_row(
            "SELECT checksum FROM schema_migrations WHERE version=?1",
            [migration.version],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != checksum {
            return Err(anyhow!(
                "database migration {} checksum mismatch",
                migration.version
            ));
        }
        return Ok(());
    }

    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(migration.sql)?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, name, checksum, applied_at) VALUES (?1, ?2, ?3, unixepoch())",
        params![migration.version, migration.name, checksum],
    )?;
    transaction.commit()?;
    Ok(())
}

fn checksum(sql: &str) -> String {
    let digest = Sha256::digest(sql.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
