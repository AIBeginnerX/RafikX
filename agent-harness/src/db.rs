use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::Connection;

static SEQ: AtomicU32 = AtomicU32::new(1);

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("{} 폴더를 만들 수 없습니다", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("{} 를 열 수 없습니다", path.display()))?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS runs (
              id TEXT PRIMARY KEY,
              started_at INTEGER NOT NULL,
              finished_at INTEGER,
              mode TEXT NOT NULL,
              class TEXT,
              subagent TEXT,
              provider TEXT,
              model TEXT,
              task TEXT,
              iterations INTEGER DEFAULT 0,
              input_tokens INTEGER DEFAULT 0,
              output_tokens INTEGER DEFAULT 0,
              status TEXT NOT NULL,
              error TEXT
            );
            "#,
        )?;
        Ok(Self { conn })
    }

    pub fn db_path() -> Result<PathBuf> {
        Ok(crate::config::Config::data_dir()?.join("data.db"))
    }

    pub fn new_id() -> String {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        format!("{millis}-{seq}")
    }

    pub fn start_run(
        &self,
        mode: &str,
        task: &str,
        class: Option<&str>,
        subagent: Option<&str>,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<String> {
        let id = Self::new_id();
        let started = now_secs();
        let task: String = task.chars().take(500).collect();
        self.conn.execute(
            "INSERT INTO runs (id, started_at, mode, class, subagent, task, provider, model, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'ok')",
            rusqlite::params![id, started, mode, class, subagent, task, provider, model],
        )?;
        Ok(id)
    }

    pub fn finish_run(
        &self,
        id: &str,
        status: &str,
        iterations: i64,
        input_tokens: i64,
        output_tokens: i64,
        error: Option<&str>,
    ) -> Result<()> {
        let finished = now_secs();
        let err = error.map(|e| e.chars().take(500).collect::<String>());
        self.conn.execute(
            "UPDATE runs SET finished_at=?1, status=?2, iterations=?3, input_tokens=?4, output_tokens=?5, error=?6 WHERE id=?7",
            rusqlite::params![finished, status, iterations, input_tokens, output_tokens, err, id],
        )?;
        Ok(())
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
