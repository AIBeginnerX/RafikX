//! /ulw 자율 완수 루프 (F4) — 목표만 받아 완료 기준의 증거가 모일 때까지 계속한다.
//!
//! 산출물은 `.omo/ulw/<run-id>/` 에 파일로 남는다:
//! - goal.md      목표 원문 + 완료 기준
//! - plan.md      계획 (경량 계획 호출 결과)
//! - evidence.md  실행(run)별 증거 로그 — 반복·변경 파일·todo 진전·판정
//! - state.json   상태 기계 (running|blocked|done) — 파일이라 재시작 후 /ulw-resume 가능
//! - report.md    완료·중단 보고
//!
//! Todo Enforcer: 진전 없는 실행이 연속되면 재촉 메시지를 주입하고, 재촉 MAX_NUDGES 회를
//! 넘기면 blocked 로 전이한다 (조용한 유휴·무한 루프 둘 다 금지).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 재촉은 연속 최대 3회 — 초과 시 blocked.
pub const MAX_NUDGES: u32 = 3;
/// 총 실행 상한 = 초기 실행 + 재촉 3회.
pub const MAX_RUNS: u32 = 1 + MAX_NUDGES;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Criterion {
    pub text: String,
    #[serde(default = "default_pending")]
    pub status: String,
    #[serde(default)]
    pub evidence: String,
}

fn default_pending() -> String {
    "pending".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UlwState {
    pub run_id: String,
    pub goal: String,
    pub status: String,
    #[serde(default)]
    pub criteria: Vec<Criterion>,
    #[serde(default)]
    pub runs: u32,
    #[serde(default)]
    pub nudges: u32,
    #[serde(default)]
    pub last_completed: usize,
    #[serde(default)]
    pub blocked_reason: String,
    /// 직전 검증 실패 시그니처 — 같은 실패의 연속 횟수를 센다 (디버그 루프 탈출 조건).
    #[serde(default)]
    pub last_failure_sig: String,
    #[serde(default)]
    pub failure_streak: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 한 번의 실행(턴)에서 뽑는 판정 재료 — chat::CompletionSummary 에서 채운다.
#[derive(Debug, Clone, Default)]
pub struct RunSummaryLite {
    pub changed_files: Vec<String>,
    pub completed_todos: usize,
    pub total_todos: usize,
    pub iterations: u32,
    pub tool_errors: usize,
    pub answer_tail: String,
    /// ulw 가 에이전트 주장과 독립으로 직접 실행한 검증 (F4b 품질 게이트).
    /// verify_ran=false 면 검증 불필요(코드 변경 없음·감지된 명령 없음)로 본다.
    pub verify_ran: bool,
    pub verify_ok: bool,
    pub verify_tail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UlwVerdict {
    /// 모든 완료 기준이 충족됨.
    Done,
    /// 미완료 기준 남음 — 재촉 메시지로 계속.
    Continue,
    /// 재촉 상한 초과 또는 실행 상한 — 중단.
    Blocked(String),
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl UlwState {
    pub fn dir(workspace: &Path, run_id: &str) -> PathBuf {
        workspace.join(".omo").join("ulw").join(run_id)
    }

    /// 새 루프 시작 — 디렉터리·goal.md·state.json 생성.
    pub fn start(workspace: &Path, goal: &str) -> Result<Self> {
        let run_id = format!("ulw-{}", crate::db::Db::new_id());
        let now = now_secs();
        let state = Self {
            run_id: run_id.clone(),
            goal: goal.trim().to_string(),
            status: "running".into(),
            criteria: Vec::new(),
            runs: 0,
            nudges: 0,
            last_completed: 0,
            blocked_reason: String::new(),
            last_failure_sig: String::new(),
            failure_streak: 0,
            created_at: now,
            updated_at: now,
        };
        let dir = Self::dir(workspace, &run_id);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("{} 를 만들 수 없습니다", dir.display()))?;
        std::fs::write(dir.join("goal.md"), format!("# 목표\n\n{}\n", state.goal))?;
        state.save(workspace)?;
        Ok(state)
    }

    pub fn load(workspace: &Path, run_id: &str) -> Result<Self> {
        let path = Self::dir(workspace, run_id).join("state.json");
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("ulw 실행을 찾을 수 없습니다: {run_id} ({})", path.display()))?;
        let state: Self = serde_json::from_str(&body)
            .with_context(|| format!("state.json 이 손상됐습니다: {}", path.display()))?;
        Ok(state)
    }

    /// id 가 없으면 가장 최근 실행을 고른다 (/ulw-resume 인자 생략).
    pub fn latest_id(workspace: &Path) -> Option<String> {
        let root = workspace.join(".omo").join("ulw");
        let mut best: Option<(i64, String)> = None;
        for entry in std::fs::read_dir(root).ok()? {
            let entry = entry.ok()?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if let Ok(state) = Self::load(workspace, &id) {
                let score = state.updated_at;
                if best.as_ref().is_none_or(|(s, _)| score > *s) {
                    best = Some((score, id));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    pub fn save(&self, workspace: &Path) -> Result<()> {
        let dir = Self::dir(workspace, &self.run_id);
        std::fs::create_dir_all(&dir)?;
        let mut state = self.clone();
        state.updated_at = now_secs();
        let body = serde_json::to_string_pretty(&state)?;
        std::fs::write(dir.join("state.json"), body)?;
        Ok(())
    }

    /// 경량 계획 호출 결과에서 완료 기준을 추출해 저장한다.
    /// `- `, `- [ ]`, `N. ` 목록 줄만 인식한다.
    pub fn parse_criteria_lines(plan_text: &str) -> Vec<String> {
        plan_text
            .lines()
            .filter_map(|line| {
                let t = line.trim();
                let item = t
                    .strip_prefix("- [ ]")
                    .or_else(|| t.strip_prefix("- [x]"))
                    .or_else(|| t.strip_prefix("- "))
                    .or_else(|| {
                        t.split_once(". ")
                            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                            .map(|(_, rest)| rest)
                    })?;
                let item = item.trim();
                if item.is_empty() { None } else { Some(item.to_string()) }
            })
            .collect()
    }

    pub fn set_criteria(&mut self, workspace: &Path, plan_text: &str, items: Vec<String>) -> Result<()> {
        self.criteria = items
            .into_iter()
            .map(|text| Criterion {
                text,
                status: "pending".into(),
                evidence: String::new(),
            })
            .collect();
        let dir = Self::dir(workspace, &self.run_id);
        std::fs::write(dir.join("plan.md"), plan_text)?;
        let mut ev = String::from("# 증거 로그\n\n| 기준 | 상태 | 증거 |\n|---|---|---|\n");
        for c in &self.criteria {
            ev.push_str(&format!("| {} | 대기 | — |\n", c.text));
        }
        ev.push_str("\n## 실행 기록\n");
        std::fs::write(dir.join("evidence.md"), ev)?;
        self.save(workspace)
    }

    pub fn unmet(&self) -> Vec<&Criterion> {
        self.criteria.iter().filter(|c| c.status != "met").collect()
    }

    /// 실행 결과를 반영하고 다음 행동을 결정한다 — 이 함수가 루프의 유일한 판정기다.
    pub fn record_run(&mut self, workspace: &Path, summary: &RunSummaryLite) -> Result<UlwVerdict> {
        self.runs += 1;
        let progress = summary.completed_todos > self.last_completed;
        self.last_completed = self.last_completed.max(summary.completed_todos);

        // evidence.md 실행 기록 추가
        let dir = Self::dir(workspace, &self.run_id);
        let files = if summary.changed_files.is_empty() {
            "—".into()
        } else {
            summary.changed_files.join(", ")
        };
        let verify_line = if !summary.verify_ran {
            "검증: 생략(변경 없음 또는 명령 미감지)".to_string()
        } else if summary.verify_ok {
            "검증: 통과".to_string()
        } else {
            format!("검증: 실패 — {}", summary.verify_tail.chars().take(200).collect::<String>())
        };
        let entry = format!(
            "\n### 실행 #{} — todo {}/{} · 반복 {} · 오류 {} · 진전 {}\n- 변경 파일: {}\n- {}\n- 답변 끝: {}\n",
            self.runs,
            summary.completed_todos,
            summary.total_todos,
            summary.iterations,
            summary.tool_errors,
            if progress { "있음" } else { "없음" },
            files,
            verify_line,
            summary.answer_tail.chars().take(200).collect::<String>()
        );
        let evidence_path = dir.join("evidence.md");
        let mut ev = std::fs::read_to_string(&evidence_path).unwrap_or_default();
        ev.push_str(&entry);
        std::fs::write(&evidence_path, ev)?;

        // 품질 게이트 (F4b): todo 완료만으로는 부족 — 코드 변경이 있었으면 ulw 가 직접
        // 실행한 검증(빌드·테스트)도 통과해야 한다. 에이전트의 "다 됐습니다"는 증거가 아니다.
        let verify_passed = !summary.verify_ran || summary.verify_ok;
        let verify_failed = summary.verify_ran && !summary.verify_ok;
        if verify_failed {
            let sig: String = summary.verify_tail.chars().take(120).collect();
            if sig == self.last_failure_sig && !sig.is_empty() {
                self.failure_streak += 1;
            } else {
                self.failure_streak = 1;
            }
            self.last_failure_sig = sig;
            if self.failure_streak >= 3 {
                self.status = "blocked".into();
                self.blocked_reason = format!(
                    "같은 검증 실패가 {}회 연속 — 독립 리뷰가 필요합니다: {}",
                    self.failure_streak,
                    summary.verify_tail.chars().take(120).collect::<String>()
                );
                self.save(workspace)?;
                return Ok(UlwVerdict::Blocked(self.blocked_reason.clone()));
            }
        }
        let all_done =
            summary.total_todos > 0 && summary.completed_todos == summary.total_todos && verify_passed;
        if all_done {
            for c in &mut self.criteria {
                if c.status != "met" {
                    c.status = "met".into();
                    c.evidence = format!("실행 #{}: todo 전부 완료 + {}", self.runs, files);
                }
            }
            self.status = "done".into();
            self.save(workspace)?;
            return Ok(UlwVerdict::Done);
        }

        if progress && !verify_failed {
            self.nudges = 0;
        } else {
            self.nudges += 1;
        }
        if self.nudges > MAX_NUDGES || self.runs >= MAX_RUNS {
            self.status = "blocked".into();
            self.blocked_reason = format!(
                "재촉 {}회·실행 {}회에도 미완료 기준 {}개 남음",
                self.nudges,
                self.runs,
                self.unmet().len()
            );
            self.save(workspace)?;
            return Ok(UlwVerdict::Blocked(self.blocked_reason.clone()));
        }
        self.save(workspace)?;
        Ok(UlwVerdict::Continue)
    }

    /// Todo Enforcer 가 주입하는 재촉 메시지 — 직전 검증 실패 로그가 있으면 함께 준다
    /// (디버그 루프: 에이전트는 추측이 아니라 실제 실패 출력을 보고 고친다).
    pub fn nudge_message(&self) -> String {
        let unmet: Vec<String> = self.unmet().iter().map(|c| format!("- {}", c.text)).collect();
        let failure = if self.last_failure_sig.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n직전 검증 실패 로그 (연속 {}회):\n```\n{}\n```\n이 출력의 원인을 먼저 고쳐라. 같은 방식의 재시도는 금지한다.",
                self.failure_streak, self.last_failure_sig
            )
        };
        format!(
            "[ulw 재촉 {}/{}] 아직 완료 기준이 {}개 남았습니다:\n{}\n계속하세요. 각 기준의 증거(명령 출력·테스트 결과·파일 경로)를 모으기 전에는 완료를 선언하지 마세요.{}",
            self.nudges,
            MAX_NUDGES,
            unmet.len(),
            unmet.join("\n"),
            failure
        )
    }

    /// 첫 실행에 붙이는 규칙 블록.
    pub fn kickoff_task(&self) -> String {
        let criteria: Vec<String> = self.criteria.iter().map(|c| format!("- [ ] {}", c.text)).collect();
        format!(
            "{}\n\n[ulw 실행 규칙]\n완료 기준:\n{}\n- 위 기준을 todo_write 로 등록한 뒤 시작하라.\n- 각 기준은 증거(명령 출력·테스트 결과·파일 경로)가 있어야 완료로 표시한다.\n- 코드 변경이 있는 기준은 테스트를 추가·실행한다. 테스트가 없는 기준은 evidence.md 에 '수동 확인' 사유를 반드시 적는다 — 조용한 생략은 실패로 본다.\n- 모든 기준이 충족될 때까지 멈추지 마라. 산출물 근거는 .omo/ulw/{}/ 에 기록된다.",
            self.goal,
            criteria.join("\n"),
            self.run_id
        )
    }

    pub fn write_report(&self, workspace: &Path, extra: &str) -> Result<()> {
        let dir = Self::dir(workspace, &self.run_id);
        let mut body = format!(
            "# ulw 보고 — {}\n\n- 목표: {}\n- 상태: {}\n- 실행 {}회 · 재촉 {}회\n",
            self.run_id, self.goal, self.status, self.runs, self.nudges
        );
        if !self.blocked_reason.is_empty() {
            body.push_str(&format!("- 중단 사유: {}\n", self.blocked_reason));
        }
        body.push_str("\n## 완료 기준\n");
        for c in &self.criteria {
            let mark = if c.status == "met" { "x" } else { " " };
            body.push_str(&format!("- [{mark}] {} — {}\n", c.text, c.evidence));
        }
        if !extra.is_empty() {
            body.push_str(&format!("\n## 비고\n{extra}\n"));
        }
        std::fs::write(dir.join("report.md"), body)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(tag: &str) -> (PathBuf, UlwState) {
        let dir = std::env::temp_dir().join(format!("rafikx-ulw-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = UlwState::start(&dir, "로그인 API에 rate limit 추가").unwrap();
        (dir, state)
    }

    fn summary(completed: usize, total: usize) -> RunSummaryLite {
        RunSummaryLite {
            changed_files: vec!["src/api.rs".into()],
            completed_todos: completed,
            total_todos: total,
            iterations: 3,
            tool_errors: 0,
            answer_tail: "완료했습니다".into(),
            verify_ran: false,
            verify_ok: false,
            verify_tail: String::new(),
        }
    }

    #[test]
    fn start_creates_artifacts() {
        let (dir, state) = setup("start");
        assert!(UlwState::dir(&dir, &state.run_id).join("goal.md").is_file());
        assert!(UlwState::dir(&dir, &state.run_id).join("state.json").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_criteria_from_plan_text() {
        let plan = "계획:\n- 빌드 통과\n- [ ] 테스트 추가\n1. 문서 갱신\n텍스트 줄\n";
        let items = UlwState::parse_criteria_lines(plan);
        assert_eq!(items, vec!["빌드 통과", "테스트 추가", "문서 갱신"]);
    }

    #[test]
    fn done_when_all_todos_completed() {
        let (dir, mut state) = setup("done");
        state
            .set_criteria(&dir, "plan", vec!["a".into(), "b".into()])
            .unwrap();
        let verdict = state.record_run(&dir, &summary(3, 3)).unwrap();
        assert_eq!(verdict, UlwVerdict::Done);
        assert_eq!(state.status, "done");
        assert!(state.unmet().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn continues_then_blocks_after_max_nudges() {
        let (dir, mut state) = setup("block");
        state
            .set_criteria(&dir, "plan", vec!["a".into(), "b".into()])
            .unwrap();
        // 진전 없는 실행 반복 — 재촉 3회 초과 시 blocked
        assert_eq!(state.record_run(&dir, &summary(1, 3)).unwrap(), UlwVerdict::Continue);
        assert_eq!(state.record_run(&dir, &summary(1, 3)).unwrap(), UlwVerdict::Continue);
        assert_eq!(state.record_run(&dir, &summary(1, 3)).unwrap(), UlwVerdict::Continue);
        let verdict = state.record_run(&dir, &summary(1, 3)).unwrap();
        assert!(matches!(verdict, UlwVerdict::Blocked(_)));
        assert_eq!(state.status, "blocked");
        assert!(!state.blocked_reason.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn progress_resets_nudges() {
        let (dir, mut state) = setup("reset");
        state.set_criteria(&dir, "plan", vec!["a".into()]).unwrap();
        // 진전 0(완료 todo 증가 없음)이어야 재촉이 오른다 — 첫 실행의 0→1 자체는 진전이다.
        assert_eq!(state.record_run(&dir, &summary(0, 3)).unwrap(), UlwVerdict::Continue);
        assert_eq!(state.record_run(&dir, &summary(0, 3)).unwrap(), UlwVerdict::Continue);
        assert_eq!(state.nudges, 2); // 진전 없는 실행 2회 연속
        assert_eq!(state.record_run(&dir, &summary(2, 3)).unwrap(), UlwVerdict::Continue);
        assert_eq!(state.nudges, 0); // 진전이 있으면 재촉 카운트 리셋
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn state_survives_save_load_cycle() {
        let (dir, mut state) = setup("resume");
        state
            .set_criteria(&dir, "plan", vec!["a".into(), "b".into()])
            .unwrap();
        let _ = state.record_run(&dir, &summary(1, 2));
        let loaded = UlwState::load(&dir, &state.run_id).unwrap();
        assert_eq!(loaded.goal, state.goal);
        assert_eq!(loaded.runs, 1);
        assert_eq!(loaded.criteria.len(), 2);
        assert_eq!(UlwState::latest_id(&dir), Some(state.run_id.clone()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nudge_message_lists_unmet() {
        let (dir, mut state) = setup("nudge");
        state
            .set_criteria(&dir, "plan", vec!["테스트 통과".into(), "문서화".into()])
            .unwrap();
        let msg = state.nudge_message();
        assert!(msg.contains("2개"));
        assert!(msg.contains("테스트 통과"));
        assert!(msg.contains("재촉"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn kickoff_carries_criteria_and_rules() {
        let (dir, mut state) = setup("kickoff");
        state
            .set_criteria(&dir, "plan", vec!["빌드 통과".into()])
            .unwrap();
        let task = state.kickoff_task();
        assert!(task.contains("rate limit"));
        assert!(task.contains("- [ ] 빌드 통과"));
        assert!(task.contains("todo_write"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod quality_gate_tests {
    use super::*;

    fn setup(tag: &str) -> (PathBuf, UlwState) {
        let dir = std::env::temp_dir().join(format!("rafikx-ulwq-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = UlwState::start(&dir, "기능 추가").unwrap();
        state
            .set_criteria(&dir, "plan", vec!["테스트 통과".into()])
            .unwrap();
        (dir, state)
    }

    fn summary_with_verify(completed: usize, total: usize, ran: bool, ok: bool, tail: &str) -> RunSummaryLite {
        RunSummaryLite {
            changed_files: vec!["src/lib.rs".into()],
            completed_todos: completed,
            total_todos: total,
            iterations: 2,
            tool_errors: 0,
            answer_tail: String::new(),
            verify_ran: ran,
            verify_ok: ok,
            verify_tail: tail.into(),
        }
    }

    #[test]
    fn todos_complete_but_verify_failed_is_not_done() {
        let (dir, mut state) = setup("gate");
        let verdict = state
            .record_run(&dir, &summary_with_verify(2, 2, true, false, "cargo test: FAILED"))
            .unwrap();
        assert!(matches!(verdict, UlwVerdict::Continue));
        assert_eq!(state.status, "running");
        assert!(state.criteria.iter().all(|c| c.status != "met"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn todos_complete_and_verify_passed_is_done() {
        let (dir, mut state) = setup("pass");
        let verdict = state
            .record_run(&dir, &summary_with_verify(2, 2, true, true, ""))
            .unwrap();
        assert_eq!(verdict, UlwVerdict::Done);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn same_failure_three_times_blocks_immediately() {
        let (dir, mut state) = setup("streak");
        let fail = || summary_with_verify(1, 2, true, false, "error[E0308]: mismatched types");
        assert!(matches!(state.record_run(&dir, &fail()).unwrap(), UlwVerdict::Continue));
        assert!(matches!(state.record_run(&dir, &fail()).unwrap(), UlwVerdict::Continue));
        let verdict = state.record_run(&dir, &fail()).unwrap();
        assert!(matches!(verdict, UlwVerdict::Blocked(_)));
        assert!(state.blocked_reason.contains("리뷰"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn different_failures_reset_streak() {
        let (dir, mut state) = setup("streak-reset");
        let f1 = || summary_with_verify(1, 2, true, false, "error A");
        let f2 = || summary_with_verify(1, 2, true, false, "error B");
        assert!(matches!(state.record_run(&dir, &f1()).unwrap(), UlwVerdict::Continue));
        assert!(matches!(state.record_run(&dir, &f1()).unwrap(), UlwVerdict::Continue));
        assert_eq!(state.failure_streak, 2);
        assert!(matches!(state.record_run(&dir, &f2()).unwrap(), UlwVerdict::Continue));
        assert_eq!(state.failure_streak, 1); // 다른 실패면 스트릭 리셋
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nudge_message_carries_failure_log() {
        let (dir, mut state) = setup("nudge-log");
        let _ = state.record_run(&dir, &summary_with_verify(1, 2, true, false, "cargo test FAILED: auth"));
        let msg = state.nudge_message();
        assert!(msg.contains("검증 실패"));
        assert!(msg.contains("auth"));
        assert!(msg.contains("같은 방식의 재시도는 금지"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
