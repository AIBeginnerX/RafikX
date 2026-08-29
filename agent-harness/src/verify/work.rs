//! 실행 워크런 (M4) — SPEC·계획·태스크·원장을 한 디렉터로 묶고 재개 지점을 정의한다.
//! 근거: docs/agent-upgrade/04_DESIGN.md §6.9.
//!
//! 역할 분리(§6.1)의 실행 형태:
//! - Executor 는 서브프로세스(새 컨텍스트)로 태스크 문서의 instructions 만 받는다 —
//!   전체 대화 이력과 물리적으로 격리된다.
//! - Verifier 는 시스템(verification 실행기)이며 입력은 diff + 명령 결과뿐이다.
//! - 오케스트레이터(run-plan)는 상태 전이만 지배한다.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::plan::PlanDoc;
use super::spec::SpecDoc;
use super::task::{TaskDoc, TaskState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkConfig {
    /// Executor 실행 명령 템플릿 — {instructions} 자리에 태스크 지시가 들어간다.
    /// 기본값은 자기 자신(rafikx ask) — 새 프로세스이므로 컨텍스트가 완전히 분리된다.
    #[serde(default = "default_executor")]
    pub executor: String,
    /// 통과 후 git 체크포인트 커밋을 만들지 (G15).
    #[serde(default = "default_true")]
    pub checkpoint_commits: bool,
    /// Executor 재시도 상한 — 사다리 1단계(§6.8).
    #[serde(default = "default_retries")]
    pub executor_retries: u32,
}

fn default_executor() -> String {
    "rafikx ask {instructions}".into()
}

fn default_true() -> bool {
    true
}

fn default_retries() -> u32 {
    1
}

impl Default for WorkConfig {
    fn default() -> Self {
        Self {
            executor: default_executor(),
            checkpoint_commits: default_true(),
            executor_retries: default_retries(),
        }
    }
}

/// 실행 디렉터의 한 워크런 — plan.json 을 문으로 로드한다.
pub struct WorkRun {
    pub plan: PlanDoc,
    pub plan_path: std::path::PathBuf,
    pub spec: Option<SpecDoc>,
    pub config: WorkConfig,
}

impl WorkRun {
    pub fn load(plan_path: &std::path::Path) -> Result<Self> {
        let plan = PlanDoc::load(plan_path)?;
        let base = plan_path.parent().unwrap_or(std::path::Path::new("."));
        let spec_path = base.join("spec.json");
        let spec = if spec_path.exists() {
            Some(
                SpecDoc::load(&spec_path)
                    .with_context(|| format!("SPEC 로드 실패: {}", spec_path.display()))?,
            )
        } else {
            None
        };
        let config_path = base.join("work.json");
        let config = if config_path.exists() {
            let raw = std::fs::read_to_string(&config_path)?;
            serde_json::from_str(&raw)
                .with_context(|| format!("work.json 형식 오류: {}", config_path.display()))?
        } else {
            WorkConfig::default()
        };
        Ok(Self {
            plan,
            plan_path: plan_path.to_path_buf(),
            spec,
            config,
        })
    }

    /// SPEC 이 있고 미동결이면 실행을 거부한다 — 승인 전 실행 금지 (§6.3-4).
    pub fn spec_gate(&self) -> Result<()> {
        if let Some(spec) = &self.spec
            && !spec.frozen
        {
            anyhow::bail!(
                "SPEC {} 이 아직 동결되지 않았다 — rafikx spec-freeze 로 승인·동결 후 실행하라",
                spec.id
            );
        }
        Ok(())
    }

    /// 재개 지점 — Done 이 아닌 첫 태스크. 없으면 전부 완료다.
    pub fn resume_index(&self) -> Option<usize> {
        self.plan
            .task_docs()
            .iter()
            .position(|t| t.state != TaskState::Done)
    }

    /// 완료 개수 — 진행 요약에 쓴다.
    pub fn done_count(&self) -> usize {
        self.plan
            .task_docs()
            .iter()
            .filter(|t| t.state == TaskState::Done)
            .count()
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
    fn resume_starts_at_first_not_done() {
        let dir = std::env::temp_dir().join(format!("rk-work-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir,
            "t1.json",
            r#"{"id":"T-1","title":"a","spec_refs":["AC-1"],"verification":[{"cmd":"true"}],"state":"DONE"}"#,
        );
        write(
            &dir,
            "t2.json",
            r#"{"id":"T-2","title":"b","spec_refs":["AC-1"],"verification":[{"cmd":"true"}],"state":"PENDING"}"#,
        );
        write(
            &dir,
            "t3.json",
            r#"{"id":"T-3","title":"c","spec_refs":["AC-1"],"verification":[{"cmd":"true"}],"state":"PENDING"}"#,
        );
        write(
            &dir,
            "plan.json",
            r#"{"id":"P","acceptance":[{"id":"AC-1","description":"d"}],"tasks":["t1.json","t2.json","t3.json"]}"#,
        );
        let run = WorkRun::load(&dir.join("plan.json")).unwrap();
        assert_eq!(run.resume_index(), Some(1), "Done 이 아닌 첫 태스크에서 재개");
        assert_eq!(run.done_count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spec_gate_blocks_unfrozen_spec() {
        let dir = std::env::temp_dir().join(format!("rk-work2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir,
            "t1.json",
            r#"{"id":"T-1","title":"a","spec_refs":["AC-1"],"verification":[{"cmd":"true"}],"state":"PENDING"}"#,
        );
        write(
            &dir,
            "spec.json",
            r#"{"id":"S-1","title":"s","goal":"g","acceptance":[{"id":"AC-1","description":"d","verification":"v"}],"assumptions":["a"],"frozen":false}"#,
        );
        write(
            &dir,
            "plan.json",
            r#"{"id":"P","acceptance":[{"id":"AC-1","description":"d"}],"tasks":["t1.json"]}"#,
        );
        let run = WorkRun::load(&dir.join("plan.json")).unwrap();
        assert!(run.spec_gate().is_err(), "미동결 SPEC 은 실행 거부");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_done_has_no_resume_point() {
        let dir = std::env::temp_dir().join(format!("rk-work3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir,
            "t1.json",
            r#"{"id":"T-1","title":"a","spec_refs":["AC-1"],"verification":[{"cmd":"true"}],"state":"DONE"}"#,
        );
        write(
            &dir,
            "plan.json",
            r#"{"id":"P","acceptance":[{"id":"AC-1","description":"d"}],"tasks":["t1.json"]}"#,
        );
        let run = WorkRun::load(&dir.join("plan.json")).unwrap();
        assert_eq!(run.resume_index(), None, "전부 완료면 재개 지점 없음");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
