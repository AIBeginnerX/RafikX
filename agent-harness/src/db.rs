use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

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
            CREATE TABLE IF NOT EXISTS notes (
              path TEXT PRIMARY KEY,
              title TEXT NOT NULL,
              tags TEXT NOT NULL DEFAULT '',
              links TEXT NOT NULL DEFAULT '',
              mtime INTEGER NOT NULL
            );
            "#,
        )?;
        let db = Self { conn };
        if db.notes_fts_sql()?.is_none() {
            db.create_notes_fts(&fts_tokenize_clause("unicode61"))?;
        }
        Ok(db)
    }

    pub fn has_fts5(&self) -> Result<bool> {
        let mut stmt = self.conn.prepare("PRAGMA compile_options")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            if row?.to_ascii_uppercase().contains("FTS5") {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// FTS tokenizer가 config와 다르면 notes_fts를 재생성한다. true면 전체 재인덱스가 필요하다.
    pub fn ensure_notes_fts(&self, tokenizer: &str) -> Result<bool> {
        let wanted = fts_tokenize_clause(tokenizer);
        let existing = self.notes_fts_sql()?;
        match existing {
            Some(sql) if sql.contains(&wanted) => Ok(false),
            Some(_) => {
                self.conn.execute_batch("DROP TABLE IF EXISTS notes_fts;")?;
                self.conn.execute_batch("DELETE FROM notes;")?;
                self.create_notes_fts(&wanted)?;
                Ok(true)
            }
            None => {
                self.create_notes_fts(&wanted)?;
                Ok(false)
            }
        }
    }

    fn notes_fts_sql(&self) -> Result<Option<String>> {
        let sql: Option<String> = self.conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='notes_fts'",
            [],
            |r| r.get(0),
        ).optional()?;
        Ok(sql)
    }

    fn create_notes_fts(&self, tokenize: &str) -> Result<()> {
        self.conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(\n  \
             title, content, tags, path UNINDEXED,\n  \
             tokenize = '{tokenize}'\n);"
        ))?;
        Ok(())
    }

    pub fn note_mtime(&self, path: &str) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT mtime FROM notes WHERE path=?1",
                [path],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert_note(
        &self,
        path: &str,
        title: &str,
        tags: &str,
        links: &str,
        content: &str,
        mtime: i64,
    ) -> Result<()> {
        self.conn.execute("DELETE FROM notes WHERE path=?1", [path])?;
        self.conn.execute("DELETE FROM notes_fts WHERE path=?1", [path])?;
        self.conn.execute(
            "INSERT INTO notes (path, title, tags, links, mtime) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![path, title, tags, links, mtime],
        )?;
        self.conn.execute(
            "INSERT INTO notes_fts (title, content, tags, path) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![title, content, tags, path],
        )?;
        Ok(())
    }

    pub fn delete_note(&self, path: &str) -> Result<bool> {
        let n = self.conn.execute("DELETE FROM notes WHERE path=?1", [path])?;
        self.conn.execute("DELETE FROM notes_fts WHERE path=?1", [path])?;
        Ok(n > 0)
    }

    pub fn all_note_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM notes")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn search_notes(&self, query: &str, limit: usize) -> Result<Vec<NoteHit>> {
        let fts = build_fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT notes_fts.path, notes.title, notes.tags, notes.links,\n             \
             snippet(notes_fts, 1, '', '', '…', 24)\n             \
             FROM notes_fts JOIN notes ON notes.path = notes_fts.path\n             \
             WHERE notes_fts MATCH ?1\n             \
             ORDER BY rank\n             \
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![fts, limit as i64], |r| {
            Ok(NoteHit {
                path: r.get(0)?,
                title: r.get(1)?,
                tags: r.get(2)?,
                links: r.get(3)?,
                excerpt: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    #[allow(dead_code)]
    pub fn note_by_path(&self, path: &str) -> Result<Option<NoteRow>> {
        self.conn
            .query_row(
                "SELECT path, title, tags, links, mtime FROM notes WHERE path=?1",
                [path],
                |r| {
                    Ok(NoteRow {
                        path: r.get(0)?,
                        title: r.get(1)?,
                        tags: r.get(2)?,
                        links: r.get(3)?,
                        mtime: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn note_content(&self, path: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT content FROM notes_fts WHERE path=?1",
                [path],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn backlinks(&self, targets: &[String], exclude_path: &str, limit: usize) -> Result<Vec<NoteRow>> {
        if targets.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT path, title, tags, links, mtime FROM notes WHERE path != ?1",
        )?;
        let rows = stmt.query_map([exclude_path], |r| {
            Ok(NoteRow {
                path: r.get(0)?,
                title: r.get(1)?,
                tags: r.get(2)?,
                links: r.get(3)?,
                mtime: r.get(4)?,
            })
        })?;
        for row in rows {
            let row = row?;
            let hay = format!(",{},", row.links);
            if targets.iter().any(|t| {
                !t.is_empty() && hay.contains(&format!(",{t},"))
            }) {
                out.push(row);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
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

#[derive(Debug, Clone)]
pub struct NoteHit {
    pub path: String,
    pub title: String,
    pub tags: String,
    #[allow(dead_code)]
    pub links: String,
    pub excerpt: String,
}

#[derive(Debug, Clone)]
pub struct NoteRow {
    pub path: String,
    pub title: String,
    #[allow(dead_code)]
    pub tags: String,
    pub links: String,
    #[allow(dead_code)]
    pub mtime: i64,
}

pub fn fts_tokenize_clause(tokenizer: &str) -> String {
    match tokenizer.trim() {
        "trigram" => "trigram".into(),
        _ => "unicode61 remove_diacritics 2".into(),
    }
}

/// 5.9 FTS 쿼리 이스케이프 — 사용자 입력을 MATCH에 직접 넣지 말 것
pub fn build_fts_query(user: &str) -> String {
    user.split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_quotes_and_escapes() {
        assert_eq!(build_fts_query("프로젝트 계획"), "\"프로젝트\" \"계획\"");
        assert_eq!(build_fts_query(r#"say "hi""#), "\"say\" \"\"\"hi\"\"\"");
        assert_eq!(build_fts_query("   "), "");
    }
}
