//! facts(지속 사실) 저장소 — lessons(실패 교훈)와 별개 축.
//!
//! 스코프: project_id 가 '' 이면 전역 사실, 아니면 프로젝트 사실(projects.id).
//! UNIQUE(project_id, key) 로 같은 키는 upsert 된다 (교훈처럼 쌓이지 않고 갱신).

use rusqlite::{Connection, OptionalExtension, params};

use anyhow::Result;

use super::{FactRow, FactWrite, build_fts_query, now_secs};

pub(super) const KINDS: &[&str] = &["stack", "preference", "convention", "env", "goal", "other"];

pub(super) fn normalize_kind(raw: &str) -> &'static str {
    let k = raw.trim().to_ascii_lowercase();
    KINDS.iter().copied().find(|k2| *k2 == k).unwrap_or("other")
}

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<FactRow> {
    Ok(FactRow {
        id: r.get(0)?,
        project_id: r.get(1)?,
        kind: r.get(2)?,
        key: r.get(3)?,
        value: r.get(4)?,
        source: r.get(5)?,
        updated_at: r.get(6)?,
        hits: r.get(7)?,
    })
}

const SELECT_COLS: &str = "id, project_id, kind, key, value, source, updated_at, hits";

fn refresh_fts(connection: &Connection, id: i64, key: &str, value: &str) -> Result<()> {
    connection.execute("DELETE FROM facts_fts WHERE fact_id=?1", params![id.to_string()])?;
    connection.execute(
        "INSERT INTO facts_fts (key, value, fact_id) VALUES (?1, ?2, ?3)",
        params![key, value, id.to_string()],
    )?;
    Ok(())
}

pub(super) fn upsert(
    connection: &Connection,
    project_id: &str,
    kind: &str,
    key: &str,
    value: &str,
    source: &str,
) -> Result<FactWrite> {
    let now = now_secs();
    let existing: Option<i64> = connection
        .query_row(
            "SELECT id FROM facts WHERE project_id=?1 AND key=?2",
            params![project_id, key],
            |r| r.get(0),
        )
        .optional()?;
    let id = match existing {
        Some(id) => {
            connection.execute(
                "UPDATE facts SET value=?1, kind=?2, source=?3, updated_at=?4 WHERE id=?5",
                params![value, kind, source, now, id],
            )?;
            id
        }
        None => {
            connection.execute(
                "INSERT INTO facts (project_id, kind, key, value, source, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![project_id, kind, key, value, source, now],
            )?;
            connection.last_insert_rowid()
        }
    };
    refresh_fts(connection, id, key, value)?;
    Ok(if existing.is_some() { FactWrite::Updated { id } } else { FactWrite::Inserted { id } })
}

/// query 가 비어 있으면 스코프 전체를 최신순으로, 아니면 FTS 검색.
/// 조회된 행은 hits/last_hit 를 갱신한다 (자주 쓰는 사실이 위로).
pub(super) fn recall(
    connection: &Connection,
    project_id: &str,
    query: &str,
    kind: Option<&str>,
    limit: usize,
) -> Result<Vec<FactRow>> {
    let kind_filter = kind.map(normalize_kind);
    let mut out = Vec::new();
    let fts = build_fts_query(query);
    // trigram 은 3글자 미만 토큰을 매칭하지 못한다 (예: "언어" 2글자) — 짧은
    // 검색어는 LIKE 부분일치로 폴 fallback 한다.
    let short_tokens = !query.trim().is_empty()
        && query.split_whitespace().any(|tok| tok.chars().count() < 3);
    if short_tokens {
        let like = format!("%{}%", query.trim().replace('%', ""));
        let mut stmt = connection.prepare(&format!(
            "SELECT {SELECT_COLS} FROM facts WHERE project_id IN ('', ?1) AND (?2 IS NULL OR kind=?2) AND (key LIKE ?3 OR value LIKE ?3) ORDER BY updated_at DESC LIMIT ?4"
        ))?;
        let rows = stmt.query_map(params![project_id, kind_filter, like, limit as i64], row_from)?;
        for r in rows {
            out.push(r?);
        }
    } else if fts.is_empty() {
        let mut stmt = connection.prepare(&format!(
            "SELECT {SELECT_COLS} FROM facts WHERE project_id IN ('', ?1) AND (?2 IS NULL OR kind=?2) ORDER BY updated_at DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![project_id, kind_filter, limit as i64], row_from)?;
        for r in rows {
            out.push(r?);
        }
    } else {
        let qualified = SELECT_COLS
            .split(',')
            .map(|c| format!("facts.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = connection.prepare(&format!(
            "SELECT {qualified} FROM facts_fts JOIN facts ON facts.id=CAST(facts_fts.fact_id AS INTEGER) WHERE facts_fts MATCH ?1 AND facts.project_id IN ('', ?2) AND (?3 IS NULL OR facts.kind=?3) LIMIT ?4"
        ))?;
        let rows = stmt.query_map(params![fts, project_id, kind_filter, limit as i64], row_from)?;
        for r in rows {
            out.push(r?);
        }
    }
    let now = now_secs();
    for row in &out {
        connection.execute(
            "UPDATE facts SET hits=hits+1, last_hit=?1 WHERE id=?2",
            params![now, row.id],
        )?;
    }
    Ok(out)
}

pub(super) fn forget(connection: &Connection, project_id: &str, key: &str) -> Result<Option<FactRow>> {
    let found: Option<FactRow> = connection
        .query_row(
            &format!("SELECT {SELECT_COLS} FROM facts WHERE project_id=?1 AND key=?2"),
            params![project_id, key],
            row_from,
        )
        .optional()?;
    if let Some(row) = &found {
        connection.execute("DELETE FROM facts_fts WHERE fact_id=?1", params![row.id.to_string()])?;
        connection.execute("DELETE FROM facts WHERE id=?1", params![row.id])?;
    }
    Ok(found)
}

pub(super) fn list(connection: &Connection, project_id: &str) -> Result<Vec<FactRow>> {
    let mut stmt = connection.prepare(&format!(
        "SELECT {SELECT_COLS} FROM facts WHERE project_id IN ('', ?1) ORDER BY kind, updated_at DESC"
    ))?;
    let rows = stmt.query_map(params![project_id], row_from)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn open_tmp(tag: &str) -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("rafikx-facts-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db")).unwrap();
        let conn = db.into_conn_for_tests();
        (dir, conn)
    }

    fn cleanup(dir: std::path::PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn upsert_updates_same_key() {
        let (d, c) = open_tmp("upsert_updates_same_key");
        upsert(&c, "", "stack", "package-manager", "npm", "user").unwrap();
        let w = upsert(&c, "", "stack", "package-manager", "pnpm", "user").unwrap();
        assert!(matches!(w, FactWrite::Updated { .. }));
        let rows = list(&c, "").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value, "pnpm");
        cleanup(d);
    }

    #[test]
    fn scopes_are_separated() {
        let (d, c) = open_tmp("scopes_are_separated");
        upsert(&c, "", "env", "os", "macos", "user").unwrap();
        upsert(&c, "p1", "stack", "db", "sqlite", "agent").unwrap();
        // 전역('') 조회는 전역+프로젝트 p1이 아니라 전역만+p1... 정책: list는 IN ('', ?1)
        // 프로젝트 스코프에서 조회하면 전역+그 프로젝트 둘 다 보인다.
        let proj_view = list(&c, "p1").unwrap();
        assert_eq!(proj_view.len(), 2);
        let other_view = list(&c, "p2").unwrap();
        assert_eq!(other_view.len(), 1); // 전역만
        assert_eq!(other_view[0].key, "os");
        cleanup(d);
    }

    #[test]
    fn recall_fts_and_hits() {
        let (d, c) = open_tmp("recall_fts_and_hits");
        upsert(&c, "", "preference", "언어", "한국어로 답변", "user").unwrap();
        let rows = recall(&c, "", "언어", None, 5).unwrap();
        assert_eq!(rows.len(), 1);
        let again = recall(&c, "", "한국어", None, 5).unwrap();
        assert_eq!(again.len(), 1);
        assert!(again[0].hits >= 1);
        cleanup(d);
    }

    #[test]
    fn forget_returns_removed_value() {
        let (d, c) = open_tmp("forget_returns_removed_value");
        upsert(&c, "", "stack", "db", "sqlite", "agent").unwrap();
        let removed = forget(&c, "", "db").unwrap().unwrap();
        assert_eq!(removed.value, "sqlite");
        assert!(forget(&c, "", "db").unwrap().is_none());
        assert!(recall(&c, "", "sqlite", None, 5).unwrap().is_empty());
        cleanup(d);
    }

    #[test]
    fn unknown_kind_falls_back_to_other() {
        assert_eq!(normalize_kind("STACK"), "stack");
        assert_eq!(normalize_kind("weird"), "other");
    }
}
