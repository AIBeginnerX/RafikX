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
            CREATE TABLE IF NOT EXISTS sessions (
              id TEXT PRIMARY KEY,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL,
              title TEXT,
              messages_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS lessons (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              created_at INTEGER NOT NULL,
              last_hit INTEGER NOT NULL,
              trigger TEXT NOT NULL,
              keywords TEXT NOT NULL,
              lesson TEXT NOT NULL,
              weight INTEGER NOT NULL DEFAULT 1
            );
            "#,
        )?;
        let db = Self { conn };
        if db.notes_fts_sql()?.is_none() {
            db.create_notes_fts(&fts_tokenize_clause("unicode61"))?;
        }
        db.ensure_lessons_fts()?;
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

    fn ensure_lessons_fts(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS lessons_fts USING fts5(
              keywords, lesson, lesson_id UNINDEXED
            );
            "#,
        )?;
        Ok(())
    }

    pub fn save_session(
        &self,
        id: Option<&str>,
        title: &str,
        messages_json: &str,
    ) -> Result<String> {
        let now = now_secs();
        if let Some(id) = id {
            let n = self.conn.execute(
                "UPDATE sessions SET updated_at=?1, title=?2, messages_json=?3 WHERE id=?4",
                rusqlite::params![now, title, messages_json, id],
            )?;
            if n > 0 {
                return Ok(id.to_string());
            }
        }
        let id = Self::new_id();
        self.conn.execute(
            "INSERT INTO sessions (id, created_at, updated_at, title, messages_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, now, now, title, messages_json],
        )?;
        Ok(id)
    }

    pub fn load_session(&self, id: &str) -> Result<Option<SessionRow>> {
        self.conn
            .query_row(
                "SELECT id, created_at, updated_at, title, messages_json FROM sessions WHERE id=?1",
                [id],
                |r| {
                    Ok(SessionRow {
                        id: r.get(0)?,
                        created_at: r.get(1)?,
                        updated_at: r.get(2)?,
                        title: r.get(3)?,
                        messages_json: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, updated_at, title, messages_json FROM sessions ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(SessionRow {
                id: r.get(0)?,
                created_at: r.get(1)?,
                updated_at: r.get(2)?,
                title: r.get(3)?,
                messages_json: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn add_lesson(
        &self,
        trigger: &str,
        keywords: &str,
        lesson: &str,
        max_lessons: u32,
    ) -> Result<LessonWrite> {
        if let Some(id) = self.find_similar_lesson(lesson, keywords)? {
            let now = now_secs();
            self.conn.execute(
                "UPDATE lessons SET weight = weight + 1, last_hit = ?1 WHERE id = ?2",
                rusqlite::params![now, id],
            )?;
            return Ok(LessonWrite::Bumped { id });
        }
        let now = now_secs();
        self.conn.execute(
            "INSERT INTO lessons (created_at, last_hit, trigger, keywords, lesson, weight) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            rusqlite::params![now, now, trigger, keywords, lesson],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute(
            "INSERT INTO lessons_fts (keywords, lesson, lesson_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![keywords, lesson, id.to_string()],
        )?;
        self.prune_lessons(max_lessons)?;
        Ok(LessonWrite::Inserted { id })
    }

    fn find_similar_lesson(&self, lesson: &str, keywords: &str) -> Result<Option<i64>> {
        let q = build_fts_query(&format!("{keywords} {lesson}"));
        if q.is_empty() {
            return Ok(None);
        }
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT lesson_id FROM lessons_fts WHERE lessons_fts MATCH ?1 LIMIT 1",
                [q],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.and_then(|s| s.parse().ok()))
    }

    fn prune_lessons(&self, max_lessons: u32) -> Result<()> {
        if max_lessons == 0 {
            return Ok(());
        }
        loop {
            let count: i64 = self
                .conn
                .query_row("SELECT COUNT(*) FROM lessons", [], |r| r.get(0))?;
            if count <= max_lessons as i64 {
                break;
            }
            let id: i64 = self.conn.query_row(
                "SELECT id FROM lessons ORDER BY weight ASC, last_hit ASC LIMIT 1",
                [],
                |r| r.get(0),
            )?;
            self.delete_lesson(id)?;
        }
        Ok(())
    }

    pub fn list_lessons(&self) -> Result<Vec<LessonRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, last_hit, trigger, keywords, lesson, weight FROM lessons ORDER BY weight DESC, last_hit DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(LessonRow {
                id: r.get(0)?,
                created_at: r.get(1)?,
                last_hit: r.get(2)?,
                trigger: r.get(3)?,
                keywords: r.get(4)?,
                lesson: r.get(5)?,
                weight: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn delete_lesson(&self, id: i64) -> Result<bool> {
        let n = self.conn.execute("DELETE FROM lessons WHERE id=?1", [id])?;
        self.conn
            .execute("DELETE FROM lessons_fts WHERE lesson_id=?1", [id.to_string()])?;
        Ok(n > 0)
    }

    pub fn clear_lessons(&self) -> Result<usize> {
        let n = self.conn.execute("DELETE FROM lessons", [])?;
        self.conn.execute("DELETE FROM lessons_fts", [])?;
        Ok(n)
    }

    pub fn lessons_for_inject(
        &self,
        task: &str,
        fts_limit: usize,
        weight_limit: usize,
    ) -> Result<Vec<LessonRow>> {
        let mut by_id: std::collections::HashMap<i64, LessonRow> = std::collections::HashMap::new();
        let q = build_fts_query(task);
        if !q.is_empty() {
            let mut stmt = self.conn.prepare(
                "SELECT lessons.id, lessons.created_at, lessons.last_hit, lessons.trigger, lessons.keywords, lessons.lesson, lessons.weight \
                 FROM lessons_fts JOIN lessons ON lessons.id = CAST(lessons_fts.lesson_id AS INTEGER) \
                 WHERE lessons_fts MATCH ?1 LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![q, fts_limit as i64], |r| {
                Ok(LessonRow {
                    id: r.get(0)?,
                    created_at: r.get(1)?,
                    last_hit: r.get(2)?,
                    trigger: r.get(3)?,
                    keywords: r.get(4)?,
                    lesson: r.get(5)?,
                    weight: r.get(6)?,
                })
            })?;
            for row in rows {
                let row = row?;
                by_id.insert(row.id, row);
            }
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, last_hit, trigger, keywords, lesson, weight FROM lessons ORDER BY weight DESC, last_hit DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([weight_limit as i64], |r| {
            Ok(LessonRow {
                id: r.get(0)?,
                created_at: r.get(1)?,
                last_hit: r.get(2)?,
                trigger: r.get(3)?,
                keywords: r.get(4)?,
                lesson: r.get(5)?,
                weight: r.get(6)?,
            })
        })?;
        for row in rows {
            let row = row?;
            by_id.entry(row.id).or_insert(row);
        }
        let mut out: Vec<LessonRow> = by_id.into_values().collect();
        out.sort_by(|a, b| b.weight.cmp(&a.weight).then(b.last_hit.cmp(&a.last_hit)));
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: String,
    #[allow(dead_code)]
    pub created_at: i64,
    #[allow(dead_code)]
    pub updated_at: i64,
    pub title: Option<String>,
    pub messages_json: String,
}

#[derive(Debug, Clone)]
pub struct LessonRow {
    pub id: i64,
    #[allow(dead_code)]
    pub created_at: i64,
    pub last_hit: i64,
    pub trigger: String,
    #[allow(dead_code)]
    pub keywords: String,
    pub lesson: String,
    pub weight: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LessonWrite {
    Inserted { id: i64 },
    Bumped { id: i64 },
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

    #[test]
    fn lesson_add_and_inject() {
        let dir = std::env::temp_dir().join(format!(
            "agent-harness-lesson-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("data.db")).unwrap();
        let w = db
            .add_lesson("manual", "read_file edit", "수정 전 원문을 읽는다", 500)
            .unwrap();
        assert!(matches!(w, LessonWrite::Inserted { .. }));
        let w2 = db
            .add_lesson("manual", "read_file edit", "수정 전 원문을 읽는다", 500)
            .unwrap();
        assert!(matches!(w2, LessonWrite::Bumped { .. }));
        let rows = db.lessons_for_inject("edit_file 원문", 5, 2).unwrap();
        assert!(!rows.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
