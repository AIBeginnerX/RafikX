//! 계획 데이터 스키마와 AC 커버리지 매트릭스 (M2).
//! 근거: docs/agent-upgrade/04_DESIGN.md §6.4. 계획이 텍스트가 아니라 데이터라서
//! "모든 AC 가 ≥1 태스크에 매핑"을 코드가 강제할 수 있다.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::task::TaskDoc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    pub id: String,
    #[serde(default)]
    pub description: String,
    /// 실행 가능하면 검증 방법을 적는다 — 태스크 verification 의 근거가 된다.
    #[serde(default)]
    pub verification: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDoc {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub acceptance: Vec<AcceptanceCriterion>,
    /// 태스크 문서(JSON) 경로 — plan 파일 위치 기준 상대경로.
    pub tasks: Vec<String>,
    /// load 에서 채워지는 태스크 문서 — 직렬화 대상이 아니다.
    #[serde(skip)]
    pub task_docs: Vec<TaskDoc>,
}

impl PlanDoc {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("계획 파일을 읽을 수 없습니다: {}", path.display()))?;
        let mut plan: PlanDoc = serde_json::from_str(&raw)
            .with_context(|| format!("계획 파일 형식이 올바르지 않습니다: {}", path.display()))?;
        if plan.acceptance.is_empty() {
            anyhow::bail!("AC 가 없는 계획은 확정할 수 없습니다 — [완료 기준]을 SPEC 으로 정리하세요");
        }
        let base = path.parent().unwrap_or(std::path::Path::new("."));
        plan.task_docs = plan
            .tasks
            .iter()
            .map(|rel| {
                let p = base.join(rel);
                TaskDoc::load(&p)
                    .with_context(|| format!("태스크 문서 로드 실패: {}", p.display()))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(plan)
    }

    /// 태스크 문서들 — load 시 파일에서 읽혀 채워진다(직렬화 대상 아님).
    pub fn task_docs(&self) -> &[TaskDoc] {
        &self.task_docs
    }

    /// 커버리지 매트릭스 검사 — 문제를 모두 수집해 한 번에 보고한다.
    /// 빈 목록 = 계획 확정 가능.
    pub fn coverage_violations(&self) -> Vec<String> {
        let mut v = Vec::new();
        let task_docs = &self.task_docs;
        for ac in &self.acceptance {
            let mapped = task_docs
                .iter()
                .any(|t| t.spec_refs.iter().any(|r| r == &ac.id));
            if !mapped {
                v.push(format!(
                    "AC 커버리지: {} 는 어떤 태스크에도 매핑되지 않았다",
                    ac.id
                ));
            }
        }
        for (index, t) in task_docs.iter().enumerate() {
            if t.verification.is_empty() {
                v.push(format!(
                    "검증 불가 태스크: {} ({}번 항목) — verification 명령이 없다",
                    t.id, index
                ));
            }
            for r in &t.spec_refs {
                if !self.acceptance.iter().any(|ac| &ac.id == r) {
                    v.push(format!(
                        "미지 참조: {} 가 알 수 없는 {} 를 가리킨다",
                        t.id, r
                    ));
                }
            }
            let over = t.constraints.max_diff_lines;
            if over == 0 {
                v.push(format!("크기 기준: {} 의 max_diff_lines 가 0 이다", t.id));
            }
        }
        v
    }

    /// 사람이 읽는 커버리지 매트릭스.
    pub fn coverage_matrix(&self) -> String {
        let mut out = String::from("AC 커버리지 매트릭스:\n");
        for ac in &self.acceptance {
            let tasks: Vec<&str> = self
                .task_docs
                .iter()
                .filter(|t| t.spec_refs.iter().any(|r| r == &ac.id))
                .map(|t| t.id.as_str())
                .collect();
            let mark = if tasks.is_empty() { "✗" } else { "✓" };
            out.push_str(&format!(
                "  {mark} {} — {}\n",
                ac.id,
                if tasks.is_empty() {
                    "미매핑".to_string()
                } else {
                    tasks.join(", ")
                }
            ));
        }
        out
    }
}



#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, name: &str, json: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, json).unwrap();
        p
    }

    #[test]
    fn coverage_matrix_catches_unmapped_and_unverifiable() {
        let dir = std::env::temp_dir().join(format!("rk-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t_full = write(
            &dir,
            "t1.json",
            r#"{"id":"T-1","title":"a","spec_refs":["AC-1"],"verification":[{"cmd":"true"}],"state":"PENDING"}"#,
        );
        let t_noverify = write(
            &dir,
            "t2.json",
            r#"{"id":"T-2","title":"b","spec_refs":["AC-2"],"verification":[],"state":"PENDING"}"#,
        );
        let plan_path = write(
            &dir,
            "plan.json",
            r#"{"id":"P-1","title":"계획","acceptance":[
                {"id":"AC-1","description":"a","verification":"cargo test"},
                {"id":"AC-2","description":"b"},
                {"id":"AC-3","description":"c"}],
                "tasks":["t1.json","t2.json"]}"#,
        );
        let _ = (t_full, t_noverify);
        let plan = PlanDoc::load(&plan_path).unwrap();
        assert_eq!(plan.task_docs().len(), 2);
        let v = plan.coverage_violations();
        assert!(v.iter().any(|s| s.contains("AC-3")), "미매핑 AC 감지: {v:?}");
        assert!(
            v.iter().any(|s| s.contains("verification 명령이 없다")),
            "검증 불가 태스크 감지: {v:?}"
        );
        let matrix = plan.coverage_matrix();
        assert!(matrix.contains("✓ AC-1 — T-1"));
        assert!(matrix.contains("✗ AC-3 — 미매핑"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_without_ac_is_rejected_at_load() {
        let dir = std::env::temp_dir().join(format!("rk-plan2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = write(&dir, "plan.json", r#"{"id":"P","acceptance":[],"tasks":[]}"#);
        assert!(PlanDoc::load(&p).is_err(), "AC 없는 계획은 로드 단계에서 거부");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
