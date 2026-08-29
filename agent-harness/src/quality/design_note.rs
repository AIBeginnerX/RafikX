//! 알고리즘 설계 노트 (S1) — 사소하지 않은 로직의 구현 전 의무 산출물.
//! 근거: docs/agent-upgrade/07_QUALITY.md §4. 후보 비교·복잡도 상한·불변식·
//! 정확성 논증·property 테스트 계획이 없으면 구현을 시작할 수 없다.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub name: String,
    pub time_complexity: String,
    pub space_complexity: String,
    #[serde(default)]
    pub tradeoff: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesignNote {
    pub id: String,
    /// 무엇을 푸는가 — 입력/출력/제약의 수학적 기술.
    pub problem: String,
    pub candidates: Vec<Candidate>,
    /// 선택한 후보 — candidates 중 하나여야 한다.
    pub chosen: String,
    /// 예상 데이터 규모 기준 선택 근거.
    pub rationale: String,
    /// 복잡도 상한 — S6 성능 게이트의 검증 기준이 된다.
    pub complexity_budget: String,
    /// 불변식·사전/사후조건 — debug_assert·타입·명시적 검증으로 인코딩할 것.
    #[serde(default)]
    pub invariants: Vec<String>,
    /// 정확성 논증 — 루프 불변식·종료 조건 기준 2~5줄.
    pub correctness_argument: String,
    /// property-based 테스트로 검증할 수학적 성질 (멱등·가역·순서 불변·경계 안전).
    #[serde(default)]
    pub properties: Vec<String>,
}

impl DesignNote {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("설계 노트를 읽을 수 없습니다: {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("설계 노트 형식 오류: {}", path.display()))
    }

    /// 확정 전 검증 — 문제를 모두 수집해 한 번에 보고한다.
    pub fn validation_errors(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.problem.trim().is_empty() {
            v.push("문제 형식화가 비어 있다".into());
        }
        if self.candidates.len() < 2 {
            v.push("후보가 2개 미만이다 — 비교 없는 선택은 근거가 아니다".into());
        }
        if !self
            .candidates
            .iter()
            .any(|c| c.name == self.chosen)
        {
            v.push(format!(
                "선택({})이 후보 목록에 없다",
                self.chosen
            ));
        }
        if self
            .candidates
            .iter()
            .any(|c| c.time_complexity.trim().is_empty())
        {
            v.push("후보의 시간 복잡도가 비어 있다".into());
        }
        if self.rationale.trim().is_empty() {
            v.push("선택 근거가 비어 있다".into());
        }
        if self.complexity_budget.trim().is_empty() {
            v.push("복잡도 상한이 비어 있다 — S6 검증 기준이 된다".into());
        }
        if self.invariants.is_empty() {
            v.push("불변식이 정의되지 않았다".into());
        }
        let lines = self.correctness_argument.lines().count();
        if self.correctness_argument.trim().is_empty() || lines < 2 {
            v.push("정확성 논증이 2줄 미만이다".into());
        }
        if self.properties.is_empty() {
            v.push("property 테스트로 검증할 성질이 없다".into());
        }
        v
    }

    pub fn validate(&self) -> Result<()> {
        let errors = self.validation_errors();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!("설계 노트 검증 실패:\n{}", errors.join("\n")))
        }
    }
}

use anyhow::Context;

#[cfg(test)]
mod tests {
    use super::*;

    fn good_note() -> DesignNote {
        serde_json::from_str(
            r#"{
            "id": "DN-1",
            "problem": "10만 건 로그에서 중복 제거",
            "candidates": [
                {"name": "hash-set", "time_complexity": "O(n)", "space_complexity": "O(n)", "tradeoff": "메모리 희생"},
                {"name": "sort-dedupe", "time_complexity": "O(n log n)", "space_complexity": "O(1)", "tradeoff": "순서 변경"}
            ],
            "chosen": "hash-set",
            "rationale": "n=10만이면 O(n) 유리, 순서 보존 불필요",
            "complexity_budget": "O(n) 시간, O(n) 공간",
            "invariants": ["결과 집합은 입력 집합과 동일"],
            "correctness_argument": "루프 불변식: 처리 완료 시점에 seen 은 지금까지의 입력 원소 집합과 동일.\n모든 입력을 방문하면 종료하므로 결과는 전체 집합.",
            "properties": ["멱등성: dedupe(dedupe(x)) == dedupe(x)", "순서 무관"]
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn complete_note_passes() {
        assert!(good_note().validate().is_ok());
    }

    #[test]
    fn missing_pieces_are_all_reported() {
        let mut note = good_note();
        note.chosen = "없는 후보".into();
        note.invariants.clear();
        note.properties.clear();
        note.correctness_argument = "한 줄".into();
        let errors = note.validation_errors();
        assert_eq!(errors.len(), 4, "전부 한 번에: {errors:?}");
        assert!(note.validate().is_err());
    }
}
