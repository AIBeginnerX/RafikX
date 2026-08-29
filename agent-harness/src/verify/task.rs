//! 태스크 문서 스키마와 봉인된 상태 전이.
//!
//! `CmdResults`·`VerifierVerdict` 는 pub 필드·pub 생성자가 없다 — 둘 다
//! 시스템(verification 실행기·검증자)만 만들 수 있고, 모델 출력 파서는
//! 이 타입을 만들 수 없으므로 `TaskOutcome::Done` 을 거쳐 DONE 에 갈 수 없다.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskState {
    Pending,
    InProgress,
    Verifying,
    Done,
    Rework,
    Escalated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyCmd {
    pub cmd: String,
    #[serde(default = "default_expect_exit")]
    pub expect_exit: i32,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_expect_exit() -> i32 {
    0
}

fn default_timeout() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraints {
    #[serde(default = "default_max_diff")]
    pub max_diff_lines: usize,
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
}

fn default_max_diff() -> usize {
    300
}

impl Default for Constraints {
    fn default() -> Self {
        Self {
            max_diff_lines: default_max_diff(),
            forbidden_paths: Vec::new(),
        }
    }
}

/// 시스템이 기록하는 증거 한 항목 — 모델 출력이 채우지 않는다.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub cmd: String,
    pub exit_code: Option<i32>,
    pub expect_exit: i32,
    pub passed: bool,
    pub duration_ms: u128,
    pub output_tail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<Vec<String>>,
    /// 검증 출력에서 파싱한 통과 테스트 수 — 래칫(후퇴 감지)의 원천.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u32>,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDoc {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub spec_refs: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub context_files: Vec<String>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub constraints: Constraints,
    pub verification: Vec<VerifyCmd>,
    /// diff 가 전혀 없으면 실패 처리 — "수정 없이 완료" 차단 (레드팀 시나리오 1).
    #[serde(default = "default_true")]
    pub require_diff: bool,
    /// 시스템 전용 — 증거 원장의 태스크 로컬 사본.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    pub state: TaskState,
}

fn default_true() -> bool {
    true
}

/// 시스템이 verification 명령을 직접 실행한 결과 — runner 만 만든다.
#[derive(Debug, Clone)]
pub struct CmdResults {
    pub results: Vec<Evidence>,
    pub all_passed: bool,
}

impl CmdResults {
    pub(crate) fn new(results: Vec<Evidence>) -> Self {
        let all_passed = results.iter().all(|e| e.passed);
        Self { results, all_passed }
    }
}

/// diff 정보 — runner 가 수집한다.
#[derive(Debug, Clone, Default)]
pub struct DiffInfo {
    pub stat: Option<String>,
    pub diff_text: Option<String>,
}

/// 검증자 판정 — 검증자(지금은 CLI 흐름의 자동 판정 + 게이트)만 만든다.
#[derive(Debug, Clone)]
pub enum VerifierVerdict {
    Pass,
    Fail(String),
}

/// 시스템이 VERIFYING 에서 나올 때만 존재하는 보고서.
#[derive(Debug, Clone)]
pub struct VerifiedReport {
    pub results: CmdResults,
    pub diff: DiffInfo,
}

pub enum TaskOutcome {
    Done(VerifiedReport),
    Rework(String),
    Escalated(String),
}

impl TaskDoc {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        Self::load_with(path, true)
    }

    /// 계획 재개용 — 저장된 DONE 을 신뢰한다(이미 검증 통과한 체크포인트).
    /// 단독 재검증(verify-task)은 load 를 써야 한다: DONE 도 다시 검증된다.
    pub fn load_trusting_state(path: &std::path::Path) -> anyhow::Result<Self> {
        Self::load_with(path, false)
    }

    fn load_with(path: &std::path::Path, reset_done: bool) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let mut doc: TaskDoc = serde_json::from_str(&raw)?;
        // 파일의 state 를 신뢰하지 않으면(단독 재검증) DONE 도 다시 돌려진다.
        if reset_done && doc.state == TaskState::Done {
            doc.state = TaskState::Pending;
        }
        Ok(doc)
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// 상태 전이의 유일한 진입점 — Done 은 VerifiedReport 없이 만들어지지 않는다.
    pub fn apply(&mut self, outcome: TaskOutcome) -> &TaskState {
        match outcome {
            TaskOutcome::Done(report) => {
                self.evidence.extend(report.results.results.clone());
                self.state = TaskState::Done;
            }
            TaskOutcome::Rework(reason) => {
                self.state = TaskState::Rework;
                self.evidence.push(Evidence {
                    cmd: "[rework]".into(),
                    exit_code: Some(1),
                    expect_exit: 0,
                    passed: false,
                    duration_ms: 0,
                    output_tail: reason.chars().take(500).collect(),
                    diff_stat: None,
                    guard: None,
                    tests_passed: None,
                    recorded_at: now_secs(),
                });
            }
            TaskOutcome::Escalated(reason) => {
                self.state = TaskState::Escalated;
                self.evidence.push(Evidence {
                    cmd: "[escalated]".into(),
                    exit_code: None,
                    expect_exit: 0,
                    passed: false,
                    duration_ms: 0,
                    output_tail: reason.chars().take(500).collect(),
                    diff_stat: None,
                    guard: None,
                    tests_passed: None,
                    recorded_at: now_secs(),
                });
            }
        }
        &self.state
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task() -> TaskDoc {
        serde_json::from_str(
            r#"{
                "id": "T-001",
                "title": "샘플",
                "verification": [{"cmd": "true"}],
                "state": "PENDING"
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn defaults_are_applied_from_schema() {
        let t = sample_task();
        assert!(t.require_diff, "diff 요구가 기본값 — 수정 없는 완료 차단");
        assert_eq!(t.verification[0].expect_exit, 0);
        assert_eq!(t.constraints.max_diff_lines, 300);
        assert_eq!(t.state, TaskState::Pending);
    }

    #[test]
    fn done_requires_verified_report() {
        let mut t = sample_task();
        let results = CmdResults::new(vec![Evidence {
            cmd: "true".into(),
            exit_code: Some(0),
            expect_exit: 0,
            passed: true,
            duration_ms: 1,
            output_tail: String::new(),
            diff_stat: Some("1 file changed".into()),
            guard: None,
            tests_passed: Some(12),
            recorded_at: 0,
        }]);
        let report = VerifiedReport {
            results,
            diff: DiffInfo::default(),
        };
        let state = t.apply(TaskOutcome::Done(report));
        assert_eq!(*state, TaskState::Done);
        assert_eq!(t.evidence.len(), 1);
    }

    #[test]
    fn done_file_reloads_as_pending_so_verification_reruns() {
        let mut t = sample_task();
        t.state = TaskState::Done;
        let dir = std::env::temp_dir().join(format!("rk-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.json");
        t.save(&path).unwrap();
        let reloaded = TaskDoc::load(&path).unwrap();
        assert_eq!(reloaded.state, TaskState::Pending, "저장된 DONE 은 신뢰하지 않는다");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
