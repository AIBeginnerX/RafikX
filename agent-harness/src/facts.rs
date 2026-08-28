//! facts(지속 사실) 주입 블록 — 세션 컨텍스트 조립 시 "[기억하는 사실]" 섹션.
//!
//! lessons 의 "[과거 교훈]" 과 별개 섹션이다: 교훈은 실패에서 배운 경고,
//! 사실은 스택·선호·관습 같은 중립 정보. 전역 facts + 프로젝트 facts 를
//! kind 순으로 최대 20건, limit_chars 이내로 주입한다.

use crate::db::{Db, FactRow};

/// 주입 상한 — lessons 와 별도 예산. SPEC 성능표의 주입 오버헤드 목표 < 5ms.
pub const MAX_ITEMS: usize = 20;

pub fn inject_block(db: &Db, workspace: &std::path::Path, limit_chars: usize) -> String {
    if limit_chars == 0 {
        return String::new();
    }
    let Ok(rows) = db.list_facts(Some(workspace)) else {
        return String::new();
    };
    assemble_block(&rows, limit_chars)
}

pub fn assemble_block(rows: &[FactRow], limit_chars: usize) -> String {
    if rows.is_empty() || limit_chars == 0 {
        return String::new();
    }
    let mut body = String::from("[기억하는 사실]\n");
    let mut count = 0usize;
    for row in rows {
        if count >= MAX_ITEMS {
            break;
        }
        let scope = if row.project_id.is_empty() { "" } else { "·프로젝트" };
        let line = format!("- ({}{}) {}: {}\n", row.kind, scope, row.key.trim(), row.value.trim());
        if body.chars().count() + line.chars().count() > limit_chars {
            break;
        }
        body.push_str(&line);
        count += 1;
    }
    if body.lines().count() <= 1 {
        return String::new();
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, key: &str, value: &str, project: &str) -> FactRow {
        FactRow {
            id: 1,
            project_id: project.into(),
            kind: kind.into(),
            key: key.into(),
            value: value.into(),
            source: "user".into(),
            updated_at: 0,
            hits: 0,
        }
    }

    #[test]
    fn block_groups_kinds_and_marks_project_scope() {
        let rows = vec![
            row("stack", "패키지 매니저", "pnpm", "p1"),
            row("preference", "답변 언어", "한국어", ""),
        ];
        let block = assemble_block(&rows, 800);
        assert!(block.starts_with("[기억하는 사실]"));
        assert!(block.contains("(stack·프로젝트) 패키지 매니저: pnpm"));
        assert!(block.contains("(preference) 답변 언어: 한국어"));
    }

    #[test]
    fn respects_char_limit() {
        let rows = vec![row("stack", "k", &"v".repeat(500), "")];
        assert!(assemble_block(&rows, 50).is_empty());
        assert!(!assemble_block(&rows, 800).is_empty());
    }

    #[test]
    fn caps_at_max_items() {
        let rows: Vec<_> = (0..30).map(|i| row("other", &format!("k{i}"), "v", "")).collect();
        let block = assemble_block(&rows, 10_000);
        assert_eq!(block.lines().count() - 1, MAX_ITEMS);
    }

    #[test]
    fn empty_rows_yield_empty_block() {
        assert!(assemble_block(&[], 800).is_empty());
    }
}
