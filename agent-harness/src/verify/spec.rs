//! SPEC 스키마와 동결 봉인 (M3) — 모호함은 실행 전에 죽인다.
//! 근거: docs/agent-upgrade/04_DESIGN.md §6.3.
//! 동결은 `freeze` 한 곳에서만 일어나고, 동결된 문서는 변경 메서드가 없다 —
//! SPEC 변경은 새 초안 + 재승인(변경 통제)만 가능하다.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use super::plan::AcceptanceCriterion;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecDoc {
    pub id: String,
    pub title: String,
    /// 목표 요약 — 한 문단.
    pub goal: String,
    #[serde(default)]
    pub in_scope: Vec<String>,
    #[serde(default)]
    pub out_of_scope: Vec<String>,
    pub acceptance: Vec<AcceptanceCriterion>,
    /// 비기능 요구 — 성능·보안·호환성·에러 처리 수준.
    #[serde(default)]
    pub non_functional: Vec<String>,
    /// 인터뷰에서 정리한 가정 — 질문하지 않고 진행한 판단의 기록.
    #[serde(default)]
    pub assumptions: Vec<String>,
    /// 인터뷰에서 던진 질문과 답변 기록 (질문 ≤ 5개 제한은 프로토콜이 강제).
    #[serde(default)]
    pub interview: Vec<(String, String)>,
    /// 동결 상태 — 승인 후 true. 동결 후의 변경은 변경 요청 절차뿐이다.
    pub frozen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_at: Option<i64>,
}

impl SpecDoc {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("SPEC 파일을 읽을 수 없습니다: {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("SPEC 형식이 올바르지 않습니다: {}", path.display()))
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        if self.frozen {
            let existing = SpecDoc::load(path);
            if let Ok(prev) = existing
                && prev.frozen
                && prev != *self
            {
                return Err(anyhow!(
                    "SPEC 이 동결돼 있다 — 변경은 새 초안 + 재승인(변경 요청)으로만 가능하다"
                ));
            }
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// 동결 전 검증 — 거부 사유를 모두 수집해 한 번에 보고한다.
    pub fn validation_errors(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.goal.trim().is_empty() {
            v.push("목표 요약이 비어 있다".into());
        }
        if self.acceptance.is_empty() {
            v.push("acceptance criteria 가 없다 — 검증 가능한 완료 기준을 정의하라".into());
        }
        for ac in &self.acceptance {
            if ac.verification.trim().is_empty() {
                v.push(format!(
                    "{} 의 검증 방법이 없다 — AC 는 검증 가능해야 한다",
                    ac.id
                ));
            }
        }
        if self.interview.is_empty() && self.assumptions.is_empty() {
            v.push(
                "인터뷰 기록도 가정 목록도 없다 — 모호함은 질문 또는 명시적 가정으로 처리하라"
                    .into(),
            );
        }
        v
    }

    /// 동결 — 검증을 통과해야만 상태가 바뀐다 (데이터 강제).
    pub fn freeze(&mut self) -> Result<()> {
        if self.frozen {
            return Err(anyhow!("이미 동결된 SPEC 이다"));
        }
        let errors = self.validation_errors();
        if !errors.is_empty() {
            return Err(anyhow!("SPEC 검증 실패:\n{}", errors.join("\n")));
        }
        self.frozen = true;
        self.frozen_at = Some(now_secs());
        Ok(())
    }
}

impl PartialEq for SpecDoc {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(self).ok() == serde_json::to_value(other).ok()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with_acs(ids: &[&str]) -> SpecDoc {
        let acceptance: Vec<AcceptanceCriterion> = ids
            .iter()
            .map(|id| AcceptanceCriterion {
                id: id.to_string(),
                description: "설명".into(),
                verification: "cargo test".into(),
            })
            .collect();
        SpecDoc {
            id: "S-1".into(),
            title: "제목".into(),
            goal: "목표".into(),
            in_scope: vec![],
            out_of_scope: vec![],
            acceptance,
            non_functional: vec![],
            assumptions: vec!["가정: X".into()],
            interview: vec![],
            frozen: false,
            frozen_at: None,
        }
    }

    #[test]
    fn freeze_requires_verification_per_ac() {
        let mut spec = spec_with_acs(&["AC-1"]);
        spec.acceptance[0].verification = String::new();
        assert!(spec.freeze().is_err(), "검증 방법 없는 AC 는 동결 거부");
        spec.acceptance[0].verification = "cargo test".into();
        assert!(spec.freeze().is_ok());
        assert!(spec.frozen);
        assert!(spec.frozen_at.is_some());
    }

    #[test]
    fn frozen_spec_cannot_be_overwritten() {
        let dir = std::env::temp_dir().join(format!("rk-spec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spec.json");
        let mut spec = spec_with_acs(&["AC-1"]);
        spec.freeze().unwrap();
        spec.save(&path).unwrap();
        // 동결 후 무단 변경 시도 — 다른 내용이면 거부.
        spec.goal = "몰래 바꾼 목표".into();
        assert!(spec.save(&path).is_err(), "동결 산출물 덮어쓰기 거부");
        // 동일 내용 재저장은 허용(멱등).
        let mut same = spec_with_acs(&["AC-1"]);
        same.freeze().unwrap();
        assert!(same.save(&path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validation_reports_all_problems_at_once() {
        let mut spec = spec_with_acs(&["AC-1"]);
        spec.goal = String::new();
        spec.assumptions.clear();
        spec.acceptance[0].verification = String::new();
        let errors = spec.validation_errors();
        assert_eq!(errors.len(), 3, "모든 문제를 한 번에: {errors:?}");
    }
}
