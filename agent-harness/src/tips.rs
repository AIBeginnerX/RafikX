//! 팁 시스템 (F9) — 기능 발견 가능성. data/tips.json 의 팁을 시작 화면에 1줄 띄우고,
//! /tips 목록·/tip <id> 상세(실제 구현 코드 발췌 포함)로 셀프 문서화한다.
//!
//! 팁은 데이터이지 코드 생성물이 아니다. excerpt 는 설치 환경에 레포가 없어도
//! 동작하도록 tips.json 에 함께 담는다 (경로는 출처 표기용).

use serde::Deserialize;

const BUNDLED: &str = include_str!("../data/tips.json");

#[derive(Debug, Clone, Deserialize)]
pub struct Tip {
    pub id: String,
    pub tip: String,
    pub detail: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub excerpt: String,
    #[serde(default)]
    pub since: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

pub fn all() -> Vec<Tip> {
    serde_json::from_str(BUNDLED).expect("data/tips.json 은 항상 올바른 JSON 이어야 한다")
}

/// 세션당 1개 — 호출 시각으로 고르는 의사 난수 (추가 의존성 없이).
pub fn pick_random() -> Option<Tip> {
    let tips = all();
    if tips.is_empty() {
        return None;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize ^ (d.as_secs() as usize / 97))
        .unwrap_or(0);
    let n = tips.len();
    tips.into_iter().nth(nanos % n)
}

pub fn find(id: &str) -> Option<Tip> {
    let wanted = id.trim().to_ascii_lowercase();
    all().into_iter().find(|t| t.id == wanted)
}

pub fn list_lines() -> Vec<String> {
    let mut lines = vec!["[팁 목록] /tip <id> 로 자세히".to_string()];
    for t in all() {
        lines.push(format!("- {} — {}", t.id, t.tip));
    }
    lines
}

pub fn detail_lines(tip: &Tip) -> Vec<String> {
    let mut lines = vec![
        format!("[팁] {} ({}~)", tip.tip, tip.since),
        tip.detail.clone(),
    ];
    if !tip.code.is_empty() {
        lines.push(format!("구현: {}", tip.code));
    }
    if !tip.excerpt.is_empty() {
        lines.push("```".into());
        lines.push(tip.excerpt.clone());
        lines.push("```".into());
    }
    if !tip.tags.is_empty() {
        lines.push(format!("태그: {}", tip.tags.join(", ")));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_json_parses_with_required_fields() {
        let tips = all();
        assert!(tips.len() >= 12, "팁은 12개 이상: {}", tips.len());
        for t in &tips {
            assert!(!t.id.is_empty(), "id 없음");
            assert!(!t.tip.is_empty(), "{}: tip 없음", t.id);
            assert!(!t.detail.is_empty(), "{}: detail 없음", t.id);
            assert!(!t.code.is_empty(), "{}: code 없음", t.id);
        }
    }

    #[test]
    fn ids_are_unique() {
        let tips = all();
        let mut ids: Vec<&str> = tips.iter().map(|t| t.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), tips.len(), "중복 id 있음");
    }

    #[test]
    fn find_is_case_insensitive() {
        assert!(find("ulw").is_some());
        assert!(find("ULW").is_some());
        assert!(find("없는-id").is_none());
    }

    #[test]
    fn pick_random_returns_a_tip() {
        assert!(pick_random().is_some());
    }

    #[test]
    fn detail_includes_code_and_excerpt() {
        let tip = find("hashline").unwrap();
        let lines = detail_lines(&tip);
        assert!(lines.iter().any(|l| l.contains("hashline.rs")));
        assert!(lines.iter().any(|l| l.contains("verify_span")));
    }
}
