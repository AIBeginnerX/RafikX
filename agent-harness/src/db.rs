use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

mod lifecycle;
mod migrations;
mod project_memory;
mod session_stream;

pub use session_stream::{SessionEventRow, SessionSnapshotRow};

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
            CREATE TABLE IF NOT EXISTS reports (
              id TEXT PRIMARY KEY,
              created_at INTEGER NOT NULL,
              summary TEXT NOT NULL,
              body_path TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS graph_events (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              run_id TEXT NOT NULL,
              seq INTEGER NOT NULL,
              kind TEXT NOT NULL,
              label TEXT NOT NULL,
              detail TEXT NOT NULL DEFAULT '',
              parent TEXT,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_graph_run ON graph_events(run_id, seq);
            CREATE TABLE IF NOT EXISTS goals (
              id TEXT PRIMARY KEY,
              objective TEXT NOT NULL,
              status TEXT NOT NULL,
              completed INTEGER NOT NULL DEFAULT 0,
              total INTEGER NOT NULL DEFAULT 0,
              continuations INTEGER NOT NULL DEFAULT 0,
              messages_json TEXT NOT NULL DEFAULT '[]',
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_goals_status ON goals(status, updated_at);
            -- Self-Harness (arXiv:2606.09498): 에피소드 기록·실패 클러스터·후보 수정
            CREATE TABLE IF NOT EXISTS sh_episodes (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              created_at INTEGER NOT NULL,
              harness_version INTEGER NOT NULL,
              trial_id INTEGER,
              success INTEGER NOT NULL,
              signature TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_sh_episodes ON sh_episodes(harness_version, trial_id, created_at);
            CREATE TABLE IF NOT EXISTS sh_evidence (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL,
              signature TEXT NOT NULL UNIQUE,
              cause TEXT NOT NULL,
              causal TEXT NOT NULL,
              mechanism TEXT NOT NULL,
              support INTEGER NOT NULL DEFAULT 1,
              sample_task TEXT NOT NULL DEFAULT '',
              sample_detail TEXT NOT NULL DEFAULT '',
              addressed INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS sh_candidates (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              created_at INTEGER NOT NULL,
              evidence_id INTEGER NOT NULL,
              surface TEXT NOT NULL,
              new_value TEXT NOT NULL,
              audit_json TEXT NOT NULL DEFAULT '{}',
              state TEXT NOT NULL DEFAULT 'proposed',
              base_version INTEGER NOT NULL,
              target_signature TEXT NOT NULL DEFAULT '',
              baseline_success REAL NOT NULL DEFAULT 0,
              baseline_target INTEGER NOT NULL DEFAULT 0,
              trial_started_at INTEGER,
              decided_at INTEGER,
              decision_note TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_sh_candidates ON sh_candidates(state, base_version, id);
            "#,
        )?;
        migrations::apply(&conn)?;
        let db = Self { conn };
        if db.notes_fts_sql()?.is_none() {
            db.create_notes_fts(&fts_tokenize_clause("unicode61"))?;
        }
        db.ensure_lessons_fts()?;
        Ok(db)
    }

    pub fn schema_version(&self) -> Result<i64> {
        migrations::current_version(&self.conn)
    }

    pub fn ensure_project(&self, workspace: &Path) -> Result<String> {
        project_memory::ensure_project(&self.conn, workspace)
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
        let sql: Option<String> = self
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='notes_fts'",
                [],
                |r| r.get(0),
            )
            .optional()?;
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
            .query_row("SELECT mtime FROM notes WHERE path=?1", [path], |r| {
                r.get(0)
            })
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
        self.conn
            .execute("DELETE FROM notes WHERE path=?1", [path])?;
        self.conn
            .execute("DELETE FROM notes_fts WHERE path=?1", [path])?;
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
        let n = self
            .conn
            .execute("DELETE FROM notes WHERE path=?1", [path])?;
        self.conn
            .execute("DELETE FROM notes_fts WHERE path=?1", [path])?;
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
            .query_row("SELECT content FROM notes_fts WHERE path=?1", [path], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Into::into)
    }

    pub fn backlinks(
        &self,
        targets: &[String],
        exclude_path: &str,
        limit: usize,
    ) -> Result<Vec<NoteRow>> {
        if targets.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut stmt = self
            .conn
            .prepare("SELECT path, title, tags, links, mtime FROM notes WHERE path != ?1")?;
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
            if targets
                .iter()
                .any(|t| !t.is_empty() && hay.contains(&format!(",{t},")))
            {
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
        session_stream::save(&self.conn, id, title, messages_json)
    }

    pub fn load_session(&self, id: &str) -> Result<Option<SessionRow>> {
        session_stream::load(&self.conn, id)
    }

    pub fn save_session_compaction(
        &self,
        id: Option<&str>,
        title: &str,
        source_json: &str,
        compacted_json: &str,
    ) -> Result<String> {
        session_stream::save_compaction(&self.conn, id, title, source_json, compacted_json)
    }

    pub fn session_events(&self, id: &str) -> Result<Vec<SessionEventRow>> {
        session_stream::events(&self.conn, id)
    }

    pub fn session_snapshots(&self, id: &str) -> Result<Vec<SessionSnapshotRow>> {
        session_stream::snapshots(&self.conn, id)
    }

    pub fn restore_session_snapshot(&self, id: &str, seq: i64) -> Result<String> {
        session_stream::restore(&self.conn, id, seq)
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

    /// 오늘(로컬 자정 이후) 실행 통계 — rafikx status /chat /status 용.
    pub fn usage_today(&self) -> Result<(i64, i64, i64)> {
        let now = now_secs();
        // KST/로컬 자정 근사: UTC 기준 당일 00:00 (운영 참고용 정확도로 충분)
        let midnight = now - (now % 86_400);
        let (count, tin, tout): (i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0) \
             FROM runs WHERE started_at >= ?1",
            [midnight],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok((count, tin, tout))
    }

    /// 세션 내용(제목·메시지) 통검색 — omo 의 session_search 에서 착안한 경량 구현.
    /// 개인 규모의 세션 수에서는 LIKE 로 충분하고 FTS 마이그레이션이 필요 없다.
    pub fn search_sessions(&self, query: &str, limit: usize) -> Result<Vec<SessionRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let pat = format!("%{}%", query.replace(['%', '_'], ""));
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, updated_at, title, messages_json FROM sessions \
             WHERE title LIKE ?1 OR messages_json LIKE ?1 ORDER BY updated_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![pat, limit as i64], |r| {
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

    pub fn latest_run_id(&self) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT id FROM runs ORDER BY started_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn push_graph_event(
        &self,
        run_id: &str,
        kind: &str,
        label: &str,
        detail: &str,
        parent: Option<&str>,
    ) -> Result<()> {
        let seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM graph_events WHERE run_id = ?1",
            [run_id],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO graph_events (run_id, seq, kind, label, detail, parent, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![run_id, seq, kind, label, detail, parent, now_secs()],
        )?;
        Ok(())
    }

    pub fn graph_events(&self, run_id: &str) -> Result<Vec<GraphEventRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, kind, label, detail, parent FROM graph_events WHERE run_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([run_id], |r| {
            Ok(GraphEventRow {
                seq: r.get(0)?,
                kind: r.get(1)?,
                label: r.get(2)?,
                detail: r.get(3)?,
                parent: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn save_goal(&self, goal: &GoalRow) -> Result<()> {
        if goal.status == "active" {
            self.conn.execute(
                "UPDATE goals SET status = 'superseded', updated_at = ?1 WHERE status = 'active' AND id <> ?2",
                rusqlite::params![now_secs(), goal.id],
            )?;
        }
        self.conn.execute(
            "INSERT INTO goals (id, objective, status, completed, total, continuations, messages_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
               objective = excluded.objective,
               status = excluded.status,
               completed = excluded.completed,
               total = excluded.total,
               continuations = excluded.continuations,
               messages_json = excluded.messages_json,
               updated_at = excluded.updated_at",
            rusqlite::params![
                goal.id,
                goal.objective,
                goal.status,
                goal.completed as i64,
                goal.total as i64,
                i64::from(goal.continuations),
                goal.messages_json,
                now_secs()
            ],
        )?;
        Ok(())
    }

    /// 활성 목표 해제 — /goal clear. Esc 중단으로 active 가 고착된 경우의 탈출구.
    pub fn clear_active_goal(&self) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE goals SET status = 'cleared', updated_at = ?1 WHERE status = 'active'",
            [now_secs()],
        )?;
        Ok(n > 0)
    }

    pub fn active_goal(&self) -> Result<Option<GoalRow>> {
        self.conn
            .query_row(
                "SELECT id, objective, status, completed, total, continuations, messages_json
                 FROM goals WHERE status = 'active' ORDER BY updated_at DESC LIMIT 1",
                [],
                |row| {
                    Ok(GoalRow {
                        id: row.get(0)?,
                        objective: row.get(1)?,
                        status: row.get(2)?,
                        completed: row.get::<_, i64>(3)? as usize,
                        total: row.get::<_, i64>(4)? as usize,
                        continuations: row.get::<_, i64>(5)? as u8,
                        messages_json: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
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

    pub fn add_project_lesson(
        &self,
        workspace: &Path,
        trigger: &str,
        keywords: &str,
        lesson: &str,
        max_lessons: u32,
    ) -> Result<LessonWrite> {
        let project_id = self.ensure_project(workspace)?;
        let write = self.add_lesson(trigger, keywords, lesson, max_lessons)?;
        let lesson_id = match write {
            LessonWrite::Inserted { id } | LessonWrite::Bumped { id } => id,
        };
        project_memory::link_lesson(&self.conn, &project_id, lesson_id)?;
        Ok(write)
    }

    pub fn list_project_lessons(&self, workspace: &Path) -> Result<Vec<LessonRow>> {
        let project_id = self.ensure_project(workspace)?;
        project_memory::list(&self.conn, &project_id)
    }

    pub fn project_lessons_for_inject(
        &self,
        workspace: &Path,
        task: &str,
        fts_limit: usize,
        weight_limit: usize,
    ) -> Result<Vec<LessonRow>> {
        let project_id = self.ensure_project(workspace)?;
        project_memory::for_inject(&self.conn, &project_id, task, fts_limit, weight_limit)
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
        self.conn
            .execute("DELETE FROM project_lessons WHERE lesson_id=?1", [id])?;
        let n = self.conn.execute("DELETE FROM lessons WHERE id=?1", [id])?;
        self.conn.execute(
            "DELETE FROM lessons_fts WHERE lesson_id=?1",
            [id.to_string()],
        )?;
        Ok(n > 0)
    }

    pub fn clear_lessons(&self) -> Result<usize> {
        self.conn.execute("DELETE FROM project_lessons", [])?;
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

    pub fn recent_runs(&self, limit: usize) -> Result<Vec<RunRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, mode, class, subagent, provider, model, task, iterations, input_tokens, output_tokens, status, error \
             FROM runs ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(RunRow {
                id: r.get(0)?,
                started_at: r.get(1)?,
                mode: r.get(2)?,
                class: r.get(3)?,
                subagent: r.get(4)?,
                provider: r.get(5)?,
                model: r.get(6)?,
                task: r.get(7)?,
                iterations: r.get(8)?,
                input_tokens: r.get(9)?,
                output_tokens: r.get(10)?,
                status: r.get(11)?,
                error: r.get(12)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    #[allow(dead_code)]
    pub fn tokens_since(&self, since: i64) -> Result<(i64, i64)> {
        self.conn
            .query_row(
                "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0) FROM runs WHERE started_at >= ?1",
                [since],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
    }

    pub fn save_report(&self, id: &str, summary: &str, body_path: &str) -> Result<()> {
        let now = now_secs();
        self.conn.execute(
            "INSERT INTO reports (id, created_at, summary, body_path) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, now, summary, body_path],
        )?;
        Ok(())
    }

    pub fn last_report(&self) -> Result<Option<ReportRow>> {
        self.conn
            .query_row(
                "SELECT id, created_at, summary, body_path FROM reports ORDER BY created_at DESC LIMIT 1",
                [],
                |r| {
                    Ok(ReportRow {
                        id: r.get(0)?,
                        created_at: r.get(1)?,
                        summary: r.get(2)?,
                        body_path: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Self-Harness — 에피소드·증거 클러스터·후보 (self_harness.rs 전용)
    // -----------------------------------------------------------------------

    pub fn sh_add_episode(
        &self,
        harness_version: i64,
        trial_id: Option<i64>,
        success: bool,
        signature: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sh_episodes (created_at, harness_version, trial_id, success, signature) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![now_secs(), harness_version, trial_id, success as i64, signature],
        )?;
        Ok(())
    }

    /// 시그니처 정확 일치 클러스터링 — 같은 φ=(c,q,m) 은 support 만 올린다.
    pub fn sh_upsert_evidence(
        &self,
        signature: &str,
        cause: &str,
        causal: &str,
        mechanism: &str,
        sample_task: &str,
        sample_detail: &str,
    ) -> Result<()> {
        let now = now_secs();
        let task: String = sample_task.chars().take(300).collect();
        let detail: String = sample_detail.chars().take(300).collect();
        self.conn.execute(
            "INSERT INTO sh_evidence (created_at, updated_at, signature, cause, causal, mechanism, support, sample_task, sample_detail)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, 1, ?6, ?7)
             ON CONFLICT(signature) DO UPDATE SET
               support = support + 1,
               updated_at = ?1,
               sample_task = excluded.sample_task,
               sample_detail = excluded.sample_detail",
            rusqlite::params![now, signature, cause, causal, mechanism, task, detail],
        )?;
        Ok(())
    }

    /// 아직 다뤄지지 않은 클러스터 중 지지도 최상위 — 논문 3.2 의 정렬(지지도 순).
    pub fn sh_top_unaddressed(&self, min_support: i64) -> Result<Option<ShEvidenceRow>> {
        self.conn
            .query_row(
                "SELECT id, signature, cause, causal, mechanism, support, sample_task, sample_detail
                 FROM sh_evidence WHERE addressed = 0 AND support >= ?1
                 ORDER BY support DESC, updated_at DESC LIMIT 1",
                [min_support],
                |r| {
                    Ok(ShEvidenceRow {
                        id: r.get(0)?,
                        signature: r.get(1)?,
                        cause: r.get(2)?,
                        causal: r.get(3)?,
                        mechanism: r.get(4)?,
                        support: r.get(5)?,
                        sample_task: r.get(6)?,
                        sample_detail: r.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 이 증거에 대해 이번 Harness 세대에서 이미 후보를 만들었는지.
    pub fn sh_has_candidates_for(&self, evidence_id: i64, base_version: i64) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sh_candidates WHERE evidence_id = ?1 AND base_version = ?2",
            rusqlite::params![evidence_id, base_version],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// 기준선 — trial 이 아닌 최근 window 개 에피소드의
    /// (성공률, 타깃 원인 발생 수, 표본 수).
    /// 버전 경계로 자르지 않는다: Harness가 한 번 승격될 때마다 기준선이 0에서 다시
    /// 시작하면 표본이 늘 부족해 수락 판정이 신뢰를 잃는다(설계 §15.4).
    pub fn sh_baseline(&self, target_signature: &str, window: i64) -> Result<(f64, i64, u32)> {
        let mut stmt = self.conn.prepare(
            "SELECT success, signature FROM sh_episodes
             WHERE trial_id IS NULL
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![window.max(1)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        // 재발은 (q, m) 클러스터가 달라도 같은 원인이면 재발이다 — cause 프리픽스로 센다.
        let prefix = cause_prefix(target_signature);
        let mut n = 0i64;
        let mut ok = 0i64;
        let mut target = 0i64;
        for row in rows {
            let (s, sig) = row?;
            n += 1;
            ok += s;
            if sig.starts_with(&prefix) {
                target += 1;
            }
        }
        if n == 0 {
            return Ok((0.0, 0, 0));
        }
        Ok((ok as f64 / n as f64, target, n as u32))
    }

    /// 과거 시도 요약 — proposer 컨텍스트의 "이전에 시도한 수정" (논문 3.3).
    pub fn sh_attempts_summary(&self, evidence_id: i64, limit: usize) -> Result<String> {
        let mut stmt = self.conn.prepare(
            "SELECT surface, state, COALESCE(decision_note, '') FROM sh_candidates
             WHERE evidence_id = ?1 AND state IN ('accepted','rejected','stale')
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![evidence_id, limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (surface, state, note) = row?;
            out.push(format!("- {surface}: {state} {note}"));
        }
        Ok(out.join("\n"))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn sh_add_candidate(
        &self,
        evidence_id: i64,
        surface: &str,
        new_value: &str,
        audit_json: &str,
        base_version: i64,
        target_signature: &str,
        baseline_success: f64,
        baseline_target: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO sh_candidates (created_at, evidence_id, surface, new_value, audit_json, state, base_version, target_signature, baseline_success, baseline_target)
             VALUES (?1, ?2, ?3, ?4, ?5, 'proposed', ?6, ?7, ?8, ?9)",
            rusqlite::params![
                now_secs(),
                evidence_id,
                surface,
                new_value,
                audit_json,
                base_version,
                target_signature,
                baseline_success,
                baseline_target
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn sh_candidate(&self, id: i64) -> Result<Option<ShCandidateRow>> {
        self.conn
            .query_row(
                "SELECT id, evidence_id, surface, new_value, audit_json, state, base_version, target_signature, baseline_success, baseline_target
                 FROM sh_candidates WHERE id = ?1",
                [id],
                Self::map_sh_candidate,
            )
            .optional()
            .map_err(Into::into)
    }

    fn map_sh_candidate(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShCandidateRow> {
        Ok(ShCandidateRow {
            id: r.get(0)?,
            evidence_id: r.get(1)?,
            surface: r.get(2)?,
            new_value: r.get(3)?,
            audit_json: r.get(4)?,
            state: r.get(5)?,
            base_version: r.get(6)?,
            target_signature: r.get(7)?,
            baseline_success: r.get(8)?,
            baseline_target: r.get(9)?,
        })
    }

    pub fn sh_start_trial(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sh_candidates SET state = 'trial', trial_started_at = ?1 WHERE id = ?2",
            rusqlite::params![now_secs(), id],
        )?;
        Ok(())
    }

    /// trial 에 배정된 에피소드 통계 — (에피소드 수, 성공 수, 타깃 시그니처 재발 수).
    pub fn sh_trial_stats(
        &self,
        candidate_id: i64,
        target_signature: &str,
    ) -> Result<ShTrialStats> {
        // 재발 판정은 시그니처 완전일치가 아니라 cause 프리픽스 일치다(설계 §15.4):
        // 같은 원인이 다른 (q, m) 클러스터로 기록되면 완전일치는 재발을 놓친다.
        let prefix = cause_prefix(target_signature);
        self.conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(success),0),
                        COALESCE(SUM(CASE WHEN substr(signature, 1, length(?2)) = ?2 THEN 1 ELSE 0 END),0)
                 FROM sh_episodes WHERE trial_id = ?1",
                rusqlite::params![candidate_id, prefix],
                |r| {
                    Ok(ShTrialStats {
                        episodes: r.get(0)?,
                        successes: r.get(1)?,
                        target_recurrences: r.get(2)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    pub fn sh_decide_candidate(&self, id: i64, state: &str, note: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sh_candidates SET state = ?1, decided_at = ?2, decision_note = ?3 WHERE id = ?4",
            rusqlite::params![state, now_secs(), note.chars().take(300).collect::<String>(), id],
        )?;
        Ok(())
    }

    pub fn sh_mark_addressed(&self, evidence_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sh_evidence SET addressed = 1, updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now_secs(), evidence_id],
        )?;
        Ok(())
    }

    /// 같은 세대(base_version)에서 아직 시도하지 않은 다음 후보.
    pub fn sh_next_proposed(&self, base_version: i64) -> Result<Option<ShCandidateRow>> {
        self.conn
            .query_row(
                "SELECT id, evidence_id, surface, new_value, audit_json, state, base_version, target_signature, baseline_success, baseline_target
                 FROM sh_candidates WHERE state = 'proposed' AND base_version = ?1
                 ORDER BY id ASC LIMIT 1",
                [base_version],
                Self::map_sh_candidate,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Harness 승격으로 세대가 지난 후보 폐기 — 병렬 후보 평가의 순차 번역.
    pub fn sh_stale_proposed(&self, current_version: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sh_candidates SET state = 'stale', decided_at = ?1,
             decision_note = 'base_version 경과 (Harness 승격)'
             WHERE state = 'proposed' AND base_version < ?2",
            rusqlite::params![now_secs(), current_version],
        )?;
        Ok(())
    }

    /// /engine 상태 표시용 — 이번 버전의 (에피소드 수, 실패 수).
    pub fn sh_episode_counts(&self, harness_version: i64) -> Result<(i64, i64)> {
        self.conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END),0)
                 FROM sh_episodes WHERE harness_version = ?1",
                [harness_version],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
    }

    pub fn sh_open_cluster_count(&self) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM sh_evidence WHERE addressed = 0",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
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
pub struct GraphEventRow {
    pub seq: i64,
    pub kind: String,
    pub label: String,
    pub detail: String,
    pub parent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GoalRow {
    pub id: String,
    pub objective: String,
    pub status: String,
    pub completed: usize,
    pub total: usize,
    pub continuations: u8,
    pub messages_json: String,
}

#[derive(Debug, Clone)]
pub struct RunRow {
    #[allow(dead_code)]
    pub id: String,
    #[allow(dead_code)]
    pub started_at: i64,
    #[allow(dead_code)]
    pub mode: String,
    pub class: Option<String>,
    #[allow(dead_code)]
    pub subagent: Option<String>,
    pub provider: Option<String>,
    #[allow(dead_code)]
    pub model: Option<String>,
    #[allow(dead_code)]
    pub task: String,
    pub iterations: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReportRow {
    pub id: String,
    #[allow(dead_code)]
    pub created_at: i64,
    pub summary: String,
    pub body_path: String,
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

/// Self-Harness 실패 클러스터 — 시그니처 φ=(cause, causal, mechanism) 단위.
#[derive(Debug, Clone)]
pub struct ShEvidenceRow {
    pub id: i64,
    pub signature: String,
    pub cause: String,
    pub causal: String,
    pub mechanism: String,
    pub support: i64,
    pub sample_task: String,
    pub sample_detail: String,
}

/// Self-Harness 후보 수정 Δ_j — audit 기록과 기준선을 함께 보관한다.
#[derive(Debug, Clone)]
pub struct ShCandidateRow {
    pub id: i64,
    pub evidence_id: i64,
    pub surface: String,
    pub new_value: String,
    pub audit_json: String,
    #[allow(dead_code)]
    pub state: String,
    #[allow(dead_code)]
    pub base_version: i64,
    pub target_signature: String,
    pub baseline_success: f64,
    #[allow(dead_code)]
    pub baseline_target: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct ShTrialStats {
    pub episodes: i64,
    pub successes: i64,
    pub target_recurrences: i64,
}

/// 시그니처(`cause|q|m`)의 원인 프리픽스 — `cause|` 까지. 구분자가 없으면 전체.
/// 재발 판정과 기준선 집계가 같은 기준을 쓴다 (설계 §15.4).
pub fn cause_prefix(signature: &str) -> String {
    match signature.find('|') {
        Some(i) => signature[..=i].to_string(),
        None => signature.to_string(),
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

    #[test]
    fn migrations_are_versioned_and_idempotent() {
        let dir = std::env::temp_dir().join(format!("rafikx-migration-{}", Db::new_id()));
        std::fs::create_dir_all(&dir).expect("create migration directory");
        let path = dir.join("data.db");
        let first = Db::open(&path).expect("first database open");
        assert_eq!(first.schema_version().expect("first schema version"), 4);
        drop(first);
        let second = Db::open(&path).expect("second database open");
        assert_eq!(second.schema_version().expect("second schema version"), 4);
        let rows: i64 = second
            .conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("migration row count");
        assert_eq!(rows, 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn session_stream_recovers_corrupt_legacy_snapshot() {
        let dir = std::env::temp_dir().join(format!("rafikx-session-recover-{}", Db::new_id()));
        std::fs::create_dir_all(&dir).expect("create session directory");
        let db = Db::open(&dir.join("data.db")).expect("open database");
        let messages = r#"[{"role":"user","content":[]}]"#;
        let id = db
            .save_session(None, "recovery", messages)
            .expect("save session");
        db.conn
            .execute(
                "UPDATE sessions SET messages_json='corrupt' WHERE id=?1",
                [&id],
            )
            .expect("corrupt legacy snapshot");
        let recovered = db
            .load_session(&id)
            .expect("load session")
            .expect("session exists");
        assert_eq!(recovered.messages_json, messages);
        assert_eq!(db.session_events(&id).expect("events").len(), 1);
        assert_eq!(db.session_snapshots(&id).expect("snapshots").len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn compaction_keeps_source_snapshot_restorable() {
        let dir = std::env::temp_dir().join(format!("rafikx-session-compact-{}", Db::new_id()));
        std::fs::create_dir_all(&dir).expect("create compaction directory");
        let db = Db::open(&dir.join("data.db")).expect("open database");
        let source = r#"[{"role":"user","content":[{"type":"text","text":"original"}]}]"#;
        let compacted = r#"[{"role":"user","content":[{"type":"text","text":"summary"}]}]"#;
        let id = db
            .save_session_compaction(None, "compact", source, compacted)
            .expect("save compaction");
        let snapshots = db.session_snapshots(&id).expect("compaction snapshots");
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].reason, "compaction_source");
        assert_eq!(
            db.load_session(&id)
                .expect("load compacted")
                .expect("session")
                .messages_json,
            compacted
        );
        let restored = db
            .restore_session_snapshot(&id, snapshots[0].seq)
            .expect("restore source");
        assert_eq!(restored, source);
        assert_eq!(
            db.load_session(&id)
                .expect("load restored")
                .expect("session")
                .messages_json,
            source
        );
        assert_eq!(db.session_events(&id).expect("events").len(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn project_memory_does_not_leak_between_workspaces() {
        let dir = std::env::temp_dir().join(format!("rafikx-project-memory-{}", Db::new_id()));
        let first = dir.join("first");
        let second = dir.join("second");
        std::fs::create_dir_all(&first).expect("create first workspace");
        std::fs::create_dir_all(&second).expect("create second workspace");
        let db = Db::open(&dir.join("data.db")).expect("open database");
        db.add_project_lesson(
            &first,
            "manual",
            "cargo test",
            "첫 프로젝트에서는 cargo test를 실행한다",
            100,
        )
        .expect("add first project lesson");
        assert_eq!(
            db.list_project_lessons(&first)
                .expect("first lessons")
                .len(),
            1
        );
        assert!(
            db.list_project_lessons(&second)
                .expect("second lessons")
                .is_empty()
        );
        assert_eq!(
            db.project_lessons_for_inject(&first, "cargo test", 5, 2)
                .expect("first inject")
                .len(),
            1
        );
        assert!(
            db.project_lessons_for_inject(&second, "cargo test", 5, 2)
                .expect("second inject")
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lesson_add_and_inject() {
        let dir = std::env::temp_dir().join(format!(
            "rafikx-lesson-{}",
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

    #[test]
    fn self_harness_mining_baseline_and_trial_flow() {
        let dir = std::env::temp_dir().join(format!(
            "rafikx-sh-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("data.db")).unwrap();
        let sig = "verify_fail|direct|missing_artifact";

        // 시그니처 정확 일치 클러스터링 — 같은 φ 는 support 만 오른다.
        db.sh_upsert_evidence(
            sig,
            "verify_fail",
            "direct",
            "missing_artifact",
            "작업 A",
            "노트",
        )
        .unwrap();
        db.sh_upsert_evidence(
            sig,
            "verify_fail",
            "direct",
            "missing_artifact",
            "작업 B",
            "노트2",
        )
        .unwrap();
        assert!(db.sh_top_unaddressed(3).unwrap().is_none()); // 임계 미달
        let ev = db.sh_top_unaddressed(2).unwrap().expect("클러스터");
        assert_eq!(ev.support, 2);
        assert_eq!(ev.signature, sig);

        // 기준선: 성공 2 · 타깃 실패 2 → 성공률 0.5, 타깃 2건.
        db.sh_add_episode(0, None, true, "").unwrap();
        db.sh_add_episode(0, None, false, sig).unwrap();
        db.sh_add_episode(0, None, true, "").unwrap();
        db.sh_add_episode(0, None, false, sig).unwrap();
        let (rate, target, n) = db.sh_baseline(sig, 20).unwrap();
        assert_eq!(n, 4);
        assert_eq!(target, 2);
        assert!((rate - 0.5).abs() < 1e-9);
        // 기준선은 Harness 버전 경계로 잘리지 않는다 (§15.4) — 다른 버전 에피소드도 표본이다.
        db.sh_add_episode(3, None, false, sig).unwrap();
        let (_, target_v3, n_v3) = db.sh_baseline(sig, 20).unwrap();
        assert_eq!((n_v3, target_v3), (5, 3));
        // 원인이 같으면 (q, m) 클러스터가 달라도 같은 타깃으로 센다.
        db.sh_add_episode(0, None, false, "verify_fail|indirect|other")
            .unwrap();
        let (_, target_cause, _) = db.sh_baseline(sig, 20).unwrap();
        assert_eq!(target_cause, 4);

        // 후보 등록 → trial → trial 에피소드 통계 → 판정 기록.
        let cid = db
            .sh_add_candidate(
                ev.id,
                "verification_instruction",
                "새 검증 지시",
                "{}",
                0,
                sig,
                rate,
                target,
            )
            .unwrap();
        assert!(!db.sh_has_candidates_for(ev.id, 1).unwrap());
        assert!(db.sh_has_candidates_for(ev.id, 0).unwrap());
        db.sh_start_trial(cid).unwrap();
        db.sh_add_episode(0, Some(cid), true, "").unwrap();
        db.sh_add_episode(0, Some(cid), true, "").unwrap();
        let other_sig = "tool_loop|direct|blind_retry";
        db.sh_upsert_evidence(
            other_sig,
            "tool_loop",
            "direct",
            "blind_retry",
            "작업 C",
            "",
        )
        .unwrap();
        db.sh_add_episode(0, Some(cid), false, other_sig).unwrap();
        let stats = db.sh_trial_stats(cid, sig).unwrap();
        assert_eq!(stats.episodes, 3);
        assert_eq!(stats.successes, 2);
        assert_eq!(stats.target_recurrences, 0); // 다른 원인의 실패는 재발이 아니다
        // 재발 판정은 cause 프리픽스 일치다 (§15.4) — 같은 원인이 다른 (q, m) 클러스터로
        // 기록돼도 재발로 센다. 완전일치였다면 이 실패를 놓쳤다.
        db.sh_add_episode(0, Some(cid), false, "verify_fail|indirect|other")
            .unwrap();
        let stats = db.sh_trial_stats(cid, sig).unwrap();
        assert_eq!(stats.episodes, 4);
        assert_eq!(stats.target_recurrences, 1);

        db.sh_decide_candidate(cid, "accepted", "재발 0").unwrap();
        db.sh_mark_addressed(ev.id).unwrap();
        assert!(db.sh_top_unaddressed(1).unwrap().map(|e| e.signature) != Some(sig.into()));
        assert_eq!(db.sh_open_cluster_count().unwrap(), 1); // tool_loop 클러스터만 미해결로 남는다

        // 세대가 지난 proposed 후보는 stale 처리된다.
        let old = db
            .sh_add_candidate(
                ev.id,
                "execution_instruction",
                "다른 지시",
                "{}",
                0,
                sig,
                rate,
                target,
            )
            .unwrap();
        db.sh_stale_proposed(1).unwrap();
        assert!(db.sh_next_proposed(0).unwrap().is_none());
        assert_eq!(db.sh_candidate(old).unwrap().unwrap().state, "stale");

        let (episodes, failures) = db.sh_episode_counts(0).unwrap();
        assert_eq!((episodes, failures), (9, 5));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn active_goal_persists_continuation_state() {
        let dir = std::env::temp_dir().join(format!(
            "rafikx-goal-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("data.db")).unwrap();
        db.save_goal(&GoalRow {
            id: "run-1".into(),
            objective: "기능 완성".into(),
            status: "active".into(),
            completed: 2,
            total: 5,
            continuations: 3,
            messages_json: "[]".into(),
        })
        .unwrap();
        let goal = db.active_goal().unwrap().expect("active goal");
        assert_eq!(goal.objective, "기능 완성");
        assert_eq!((goal.completed, goal.total, goal.continuations), (2, 5, 3));
        db.save_goal(&GoalRow {
            id: "run-1".into(),
            objective: "기능 완성".into(),
            status: "complete".into(),
            completed: 5,
            total: 5,
            continuations: 3,
            messages_json: "[]".into(),
        })
        .unwrap();
        assert!(db.active_goal().unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
