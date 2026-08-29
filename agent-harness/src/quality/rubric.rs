//! 완료 판정 루브릭 (S0) — 기능·비기능·디자인 기준을 데이터로.
//! 근거: docs/agent-upgrade/07_QUALITY.md §6. 기준 발견 프로토콜(6.2)에 따라
//! 기준 후보는 사용자 승인으로 동결되고, 이후 판정은 동결된 루브릭으로만 한다.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricItem {
    /// 기준 — 검증 가능한 문장으로.
    pub criterion: String,
    /// 기능/비기능/디자인.
    pub kind: RubricKind,
    /// 실행 가능한 검증 방법 (테스트 명령·수동 절차).
    pub verification: String,
    /// 확인됐음 — 게이트가 채운다 (모델 출력이 아니다).
    #[serde(default)]
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RubricKind {
    Functional,
    NonFunctional,
    Design,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rubric {
    pub id: String,
    pub items: Vec<RubricItem>,
    /// 기준 발견 프로토콜(§6.2): 유사 레퍼런스 ≥3 에서 추출한 공통 기준인지.
    #[serde(default)]
    pub derived_from_references: Vec<String>,
    /// 사용자 확정 — 동결.
    pub frozen: bool,
}

impl Rubric {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw)
            .with_context(|| format!("루브릭 형식 오류: {}", path.display()))
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// 동결 전 검증 — 기능 기준은 필수, 검증 방법 비어 있으면 거부.
    pub fn validation_errors(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.items.is_empty() {
            v.push("루브릭 항목이 없다".into());
        }
        if !self
            .items
            .iter()
            .any(|i| i.kind == RubricKind::Functional)
        {
            v.push("기능 기준이 하나도 없다".into());
        }
        for item in &self.items {
            if item.verification.trim().is_empty() {
                v.push(format!(
                    "'{}' 의 검증 방법이 비어 있다",
                    item.criterion
                ));
            }
        }
        // 기준 발견 프로토콜: 임의 취향 판정 금지 — 참조 기반 또는 사용자 합의 필수.
        if self.derived_from_references.len() < 3 {
            v.push(format!(
                "기준 발견 근거가 부족하다 — 유사 레퍼런스 3개 이상 필요 (현재 {})",
                self.derived_from_references.len()
            ));
        }
        v
    }

    pub fn freeze(&mut self) -> Result<()> {
        if self.frozen {
            return Err(anyhow!("이미 동결된 루브릭이다"));
        }
        let errors = self.validation_errors();
        if !errors.is_empty() {
            return Err(anyhow!("루브릭 검증 실패:\n{}", errors.join("\n")));
        }
        self.frozen = true;
        Ok(())
    }

    /// 완료 판정 — 전 항목 verified 여야 한다.
    pub fn unverified_items(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|i| !i.verified)
            .map(|i| format!("[{:?}] {}", i.kind, i.criterion))
            .collect()
    }
}

use anyhow::Context;

#[cfg(test)]
mod tests {
    use super::*;

    fn rubric(refs: usize) -> Rubric {
        serde_json::from_value(serde_json::json!({
            "id": "R-1",
            "items": [
                {"criterion": "기능 동작", "kind": "functional", "verification": "cargo test"},
                {"criterion": "대비 AA", "kind": "design", "verification": "스크린샷 검사"}
            ],
            "derived_from_references": (0..refs).map(|i| format!("ref{i}")).collect::<Vec<_>>(),
            "frozen": false
        }))
        .unwrap()
    }

    #[test]
    fn freeze_requires_references_and_functional() {
        assert!(rubric(2).freeze().is_err(), "참조 3 미만 거부");
        let mut ok = rubric(3);
        assert!(ok.freeze().is_ok());
        assert!(ok.frozen);
    }

    #[test]
    fn unverified_items_block_completion() {
        let mut r = rubric(3);
        r.items[0].verified = true;
        let pending = r.unverified_items();
        assert_eq!(pending.len(), 1, "미검증 항목 나열: {pending:?}");
    }
}
