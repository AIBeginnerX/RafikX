//! run-plan 오케스트레이터 (M4) — 계획을 순서대로 실행하고 검증·체크포인트·재개를 지배한다.
//! 근거: docs/agent-upgrade/04_DESIGN.md §6.5·§6.8·§6.9.
//!
//! 역할 분리의 실행 형태:
//! - Executor = 서브프로세스. 태스크 문서의 instructions 만 환경변수로 전달받는다 —
//!   부모의 대화 이력과 물리적으로 격리된다(컨텍스트 격리의 강한 형태).
//! - Verifier = 시스템(verification 실행기). 입력은 diff + 명령 exit code 뿐이다.
//! - 오케스트레이터는 상태 전이·체크포인트·재개만 담당한다.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use tokio::process::Command;

use super::runner::{append_ledger, run_task_verification};
use super::task::{TaskDoc, TaskOutcome, TaskState};
use super::work::WorkRun;

#[derive(Debug, Clone, Serialize)]
pub struct TaskReport {
    pub id: String,
    pub state: TaskState,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Executor 서브프로세스 1회 — 새 컨텍스트로 태스크 지시만 수행한다.
async fn run_executor(
    template: &str,
    instructions: &str,
    feedback: Option<&str>,
    workspace: &Path,
    yes: bool,
) -> Result<()> {
    let filled = template.replace("{instructions}", instructions);
    let filled = match feedback {
        Some(f) => format!(
            "{filled}\n[재시도 지시] 이전 실행은 아래 검증 실패로 끝났다. 같은 접근을 반복하지 말고 다른 접근을 시도하라.\n{f}"
        ),
        None => filled,
    };
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&filled)
        .current_dir(workspace)
        .env("RAFIKX_TASK_INSTRUCTIONS", instructions);
    if let Some(f) = feedback {
        cmd.env("RAFIKX_VERIFY_FEEDBACK", f);
    }
    if yes && template.starts_with("rafikx ask") {
        // 기본 템플릿일 때만 승인 플래그를 넘긴다 — 사용자 템플릿은 자기 책임.
        cmd.arg("--yes");
    }
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(1800),
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await;
    match out {
        Err(_) => Err(anyhow::anyhow!("executor 시간 초과 (1800초)")),
        Ok(Err(e)) => Err(anyhow::anyhow!("executor 실행 실패: {e}")),
        Ok(Ok(out)) if out.status.success() => Ok(()),
        Ok(Ok(out)) => {
            let tail: String = String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("")
                .to_string();
            Err(anyhow::anyhow!(
                "executor 종료 코드 {:?} {tail}",
                out.status.code()
            ))
        }
    }
}

/// git 체크포인트 — 통과한 태스크를 커밋으로 남긴다 (G15). 저장소가 아니면 조용히 생략.
async fn checkpoint(task: &TaskDoc, workspace: &Path) {
    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(workspace)
        .status()
        .await;
    let _ = Command::new("git")
        .args([
            "commit",
            "-qm",
            &format!("task({}): {}", task.id, task.title),
        ])
        .current_dir(workspace)
        .status()
        .await;
}

/// 계획 전체 실행 — 재개 지점부터, 검증 통과분만 다음으로 진행한다.
/// 하나가 Escalated 되면 의존성 안전을 위해 루프를 멈춘다.
pub async fn run_plan(
    work: &mut WorkRun,
    workspace: &Path,
    yes: bool,
    ledger_path: &Path,
) -> Vec<TaskReport> {
    let mut reports = Vec::new();
    let Some(start) = work.resume_index() else {
        return reports;
    };
    let total = work.plan.task_docs().len();
    for index in start..total {
        let task_path = {
            // plan.tasks 상대경로를 다시 resolve 한다 — save 대상 파일.
            let rel = &work.plan.tasks[index];
            work.plan_path
                .parent()
                .unwrap_or(Path::new("."))
                .join(rel)
        };
        let mut task = match TaskDoc::load(&task_path) {
            Ok(t) => t,
            Err(e) => {
                reports.push(TaskReport {
                    id: format!("index-{index}"),
                    state: TaskState::Escalated,
                    attempts: 0,
                    reason: Some(format!("태스크 문서 로드 실패: {e:#}")),
                });
                break;
            }
        };
        let mut attempts = 0u32;
        let max_attempts = 1 + work.config.executor_retries;
        let mut feedback: Option<String> = None;
        loop {
            attempts += 1;
            crate::applog::info(&format!(
                "[run-plan] {} 실행 ({attempts}/{max_attempts})",
                task.id
            ));
            if let Err(e) = run_executor(
                &work.config.executor,
                &task.instructions,
                feedback.as_deref(),
                workspace,
                yes,
            )
            .await
            {
                // executor 자체 실패는 사다리 1단계 재시도 대상.
                feedback = Some(format!("{e}"));
                if attempts >= max_attempts {
                    let reason = format!("executor 실패: {e}");
                    let _ = append_ledger(
                        ledger_path,
                        &serde_json::json!({"event":"state","task_id":task.id,"state":"ESCALATED","reason":reason}),
                    );
                    let state = *task.apply(TaskOutcome::Escalated(reason.clone()));
                    let _ = task.save(&task_path);
                    reports.push(TaskReport {
                        id: task.id,
                        state,
                        attempts,
                        reason: Some(reason),
                    });
                    return reports_with_stop(reports);
                }
                continue;
            }
            // Verifier — 시스템이 직접 판정한다.
            let outcome = run_task_verification(&task, workspace).await;
            match outcome {
                TaskOutcome::Done(report) => {
                    let passed: u32 = report
                        .results
                        .results
                        .iter()
                        .filter_map(|e| e.tests_passed)
                        .max()
                        .unwrap_or(0);
                    let state = *task.apply(TaskOutcome::Done(report));
                    let _ = task.save(&task_path);
                    let _ = append_ledger(
                        ledger_path,
                        &serde_json::json!({"event":"state","task_id":task.id,"state":"DONE"}),
                    );
                    if passed > 0 {
                        let _ = append_ledger(
                            ledger_path,
                            &serde_json::json!({"event":"metric","task_id":task.id,"tests_passed":passed}),
                        );
                    }
                    if work.config.checkpoint_commits {
                        checkpoint(&task, workspace).await;
                    }
                    reports.push(TaskReport {
                        id: task.id,
                        state,
                        attempts,
                        reason: None,
                    });
                    break;
                }
                TaskOutcome::Rework(reason) => {
                    if attempts >= max_attempts {
                        let _ = append_ledger(
                            ledger_path,
                            &serde_json::json!({"event":"state","task_id":task.id,"state":"ESCALATED","reason":reason}),
                        );
                        let state =
                            *task.apply(TaskOutcome::Escalated(reason.clone()));
                        let _ = task.save(&task_path);
                        reports.push(TaskReport {
                            id: task.id,
                            state,
                            attempts,
                            reason: Some(reason),
                        });
                        // 의존성 안전 — 막힌 태스크 뒤는 진행하지 않는다.
                        return reports_with_stop(reports);
                    }
                    feedback = Some(reason);
                }
                TaskOutcome::Escalated(reason) => {
                    let state = *task.apply(TaskOutcome::Escalated(reason.clone()));
                    let _ = task.save(&task_path);
                    reports.push(TaskReport {
                        id: task.id,
                        state,
                        attempts,
                        reason: Some(reason),
                    });
                    return reports_with_stop(reports);
                }
            }
        }
    }
    reports
}

/// 중단 시 이후 태스크들을 보고에 남긴다 — 멈춘 지점이 명확히 보이게.
fn reports_with_stop(mut reports: Vec<TaskReport>) -> Vec<TaskReport> {
    reports.push(TaskReport {
        id: "[이후]".into(),
        state: TaskState::Pending,
        attempts: 0,
        reason: Some("앞선 태스크가 막혀 의존성 안전을 위해 중단했다".into()),
    });
    reports
}
