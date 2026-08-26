use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use super::{LessonRow, build_fts_query, now_secs};

pub(super) fn ensure_project(connection: &Connection, workspace: &Path) -> Result<String> {
    let canonical = workspace.canonicalize().with_context(|| {
        format!(
            "project workspace cannot be resolved: {}",
            workspace.display()
        )
    })?;
    let path = canonical.to_string_lossy().into_owned();
    let id = project_id(&path);
    let now = now_secs();
    connection.execute(
        "INSERT INTO projects(id, canonical_path, created_at, updated_at) VALUES (?1, ?2, ?3, ?3) ON CONFLICT(id) DO UPDATE SET canonical_path=excluded.canonical_path, updated_at=excluded.updated_at",
        params![id, path, now],
    )?;
    Ok(id)
}

pub(super) fn link_lesson(connection: &Connection, project_id: &str, lesson_id: i64) -> Result<()> {
    connection.execute(
        "INSERT OR IGNORE INTO project_lessons(project_id, lesson_id, created_at) VALUES (?1, ?2, ?3)",
        params![project_id, lesson_id, now_secs()],
    )?;
    Ok(())
}

pub(super) fn list(connection: &Connection, project_id: &str) -> Result<Vec<LessonRow>> {
    let mut statement = connection.prepare(
        "SELECT lessons.id, lessons.created_at, lessons.last_hit, lessons.trigger, lessons.keywords, lessons.lesson, lessons.weight FROM project_lessons JOIN lessons ON lessons.id=project_lessons.lesson_id WHERE project_lessons.project_id=?1 ORDER BY lessons.weight DESC, lessons.last_hit DESC",
    )?;
    let rows = statement.query_map([project_id], lesson_from_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub(super) fn for_inject(
    connection: &Connection,
    project_id: &str,
    task: &str,
    fts_limit: usize,
    weight_limit: usize,
) -> Result<Vec<LessonRow>> {
    let mut by_id = std::collections::HashMap::new();
    let query = build_fts_query(task);
    if !query.is_empty() {
        let mut statement = connection.prepare(
            "SELECT lessons.id, lessons.created_at, lessons.last_hit, lessons.trigger, lessons.keywords, lessons.lesson, lessons.weight FROM lessons_fts JOIN lessons ON lessons.id=CAST(lessons_fts.lesson_id AS INTEGER) JOIN project_lessons ON project_lessons.lesson_id=lessons.id WHERE project_lessons.project_id=?1 AND lessons_fts MATCH ?2 LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![project_id, query, fts_limit as i64],
            lesson_from_row,
        )?;
        for row in rows {
            let row = row?;
            by_id.insert(row.id, row);
        }
    }
    let mut statement = connection.prepare(
        "SELECT lessons.id, lessons.created_at, lessons.last_hit, lessons.trigger, lessons.keywords, lessons.lesson, lessons.weight FROM project_lessons JOIN lessons ON lessons.id=project_lessons.lesson_id WHERE project_lessons.project_id=?1 ORDER BY lessons.weight DESC, lessons.last_hit DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![project_id, weight_limit as i64], lesson_from_row)?;
    for row in rows {
        let row = row?;
        by_id.entry(row.id).or_insert(row);
    }
    let mut lessons = by_id.into_values().collect::<Vec<_>>();
    lessons.sort_by(|left, right| {
        right
            .weight
            .cmp(&left.weight)
            .then(right.last_hit.cmp(&left.last_hit))
    });
    Ok(lessons)
}

fn lesson_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LessonRow> {
    Ok(LessonRow {
        id: row.get(0)?,
        created_at: row.get(1)?,
        last_hit: row.get(2)?,
        trigger: row.get(3)?,
        keywords: row.get(4)?,
        lesson: row.get(5)?,
        weight: row.get(6)?,
    })
}

fn project_id(canonical_path: &str) -> String {
    Sha256::digest(canonical_path.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
