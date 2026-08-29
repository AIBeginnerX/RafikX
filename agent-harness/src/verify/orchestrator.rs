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
    // 워크 메타(루트의 *.json·*.jsonl·PROGRESS.md)는 커밋에서 제외한다 — 태스크
    // 산출물과 메타가 한 커밋에 섞이면 revert 롤백이 메타와 충돌한다(실측).
    let _ = Command::new("git")
        .args([
            "add",
            "-A",
            "--",
            ".",
            ":(exclude)*.json",
            ":(exclude)*.jsonl",
            ":(exclude)PROGRESS.md",
        ])
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
    // 체크포인트 재검증 (M6) — 재개 직전에 마지막 Done 태스크의 검증을 재실행해
    // 상태 파일과 실제 코드의 불일치를 감지한다 (설계 §6.9).
    if start > 0 {
        let prev_rel = &work.plan.tasks[start - 1];
        let prev_path = work
            .plan_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(prev_rel);
        if let Ok(prev) = TaskDoc::load_trusting_state(&prev_path) {
            let outcome = run_task_verification(&prev, workspace).await;
            if let TaskOutcome::Rework(reason) = outcome {
                let msg = format!(
                    "체크포인트 불일치: {} 재검증 실패 — {reason}",
                    prev.id
                );
                crate::applog::error(&msg);
                reports.push(super::orchestrator::TaskReport {
                    id: prev.id.clone(),
                    state: TaskState::Escalated,
                    attempts: 0,
                    reason: Some(msg),
                });
                return reports_with_stop(reports);
            }
            crate::applog::info(&format!(
                "[run-plan] 체크포인트 재검증 통과: {}",
                prev.id
            ));
        }
    }
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
            let _ = append_ledger(
                ledger_path,
                &serde_json::json!({
                    "event": "verification",
                    "task_id": task.id,
                    "attempt": attempts,
                    "ok": matches!(outcome, TaskOutcome::Done(_))
                }),
            );
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
    write_progress(work, &reports);
    reports
}

/// 워크 디렉터 PROGRESS.md — 사람이 읽는 요약을 시스템이 갱신한다 (설계 §6.9 매핑).
fn write_progress(work: &WorkRun, reports: &[TaskReport]) {
    let Some(dir) = work.plan_path.parent() else { return };
    let mut out = format!(
        "# run-plan {} — {}

| 태스크 | 상태 | 시도 | 사유 |\n|---|---|---|---|\n",
        work.plan.id,
        work.plan.title
    );
    for r in reports {
        let reason = r.reason.as_deref().unwrap_or("");
        out.push_str(&format!(
            "| {} | {:?} | {}회 | {reason} |\n",
            r.id, r.state, r.attempts
        ));
    }
    let done = work.done_count();
    out.push_str(&format!(
        "\n진행: {done}/{} 태스크 완료 (모두 시스템 검증 통과)\n",
        work.plan.task_docs().len()
    ));
    let _ = std::fs::write(dir.join("PROGRESS.md"), out);
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

// ---------------------------------------------------------------------------
// 태스크 롤백 (G15 완성) — 체크포인트 커밋을 revert 로 되돌린다.
// ---------------------------------------------------------------------------

/// 태스크 체크포인트를 되돌린다: (1) git revert 로 산출물 복원(이력 보존),
/// (2) 태스크 문서를 PENDING 으로 초기화해 재실행 대기.
/// revert 충돌 시 그대로 오류를 돌려준다 — 자동 해결은 하지 않는다(§5 안전).
pub async fn rollback_task(
    work: &mut WorkRun,
    task_id: &str,
    workspace: &Path,
    ledger_path: &Path,
) -> Result<String> {
    use anyhow::Context;
    // 파일명 규칙이 아니라 태스크 문서의 id 로 찾는다 — 유일한 진실 원천.
    let rel = work
        .plan
        .tasks
        .iter()
        .find(|t| {
            let path = work
                .plan_path
                .parent()
                .unwrap_or(Path::new("."))
                .join(t);
            TaskDoc::load_trusting_state(&path)
                .map(|doc| doc.id == task_id)
                .unwrap_or(false)
        })
        .context("계획에서 태스크를 찾지 못했다")?;
    let task_path = work
        .plan_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(rel);
    let mut task = TaskDoc::load_trusting_state(&task_path)?;
    // 체크포인트 커밋 탐색 — checkpoint() 가 남긴 메시지 형식과 짝.
    // revert 는 깨끗한 작업 트리를 요구한다 — 워크 메타(json/md)만 더러우면
    // 먼저 정리 커밋하고, 소스 코드가 더러우면 안전하게 중단한다.
    let status = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .await
        .context("git status 실행 실패")?;
    let dirty: Vec<String> = String::from_utf8_lossy(&status.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let meta_only = dirty.iter().all(|l| {
        let path = l.split_once(' ').map(|(_, p)| p).unwrap_or(l);
        path.ends_with(".json") || path.ends_with(".md") || path.ends_with(".jsonl")
    });
    if !dirty.is_empty() {
        if !meta_only {
            anyhow::bail!(
                "커밋되지 않은 소스 변경이 있다 — 정리 후 롤백하라: {}",
                dirty.join(", ")
            );
        }
        let _ = tokio::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(workspace)
            .status()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["commit", "-qm", "chore(work): rollback 전 워크 메타 정리"])
            .current_dir(workspace)
            .status()
            .await;
    }

    let marker = format!("task({task_id}):");
    let out = tokio::process::Command::new("git")
        .args(["log", "-n", "1", "--format=%H", "--grep", &marker])
        .current_dir(workspace)
        .output()
        .await
        .context("git log 실행 실패")?;
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        anyhow::bail!("체크포인트 커밋이 없다 — git 이 아니거나 커밋 전에 실패한 태스크다");
    }
    let revert = tokio::process::Command::new("git")
        .args(["revert", "--no-edit", &sha])
        .current_dir(workspace)
        .output()
        .await
        .context("git revert 실행 실패")?;
    if !revert.status.success() {
        // 충돌 상태로 남기지 않는다 — revert 를 중단하고 원 상태로 돌려놓는다.
        let _ = tokio::process::Command::new("git")
            .args(["revert", "--abort"])
            .current_dir(workspace)
            .status()
            .await;
        anyhow::bail!(
            "revert 실패(충돌) — 작업 트리는 원 상태로 복구했다: {}",
            String::from_utf8_lossy(&revert.stderr).lines().next().unwrap_or("").trim()
        );
    }
    task.state = TaskState::Pending;
    task.save(&task_path)?;
    let _ = append_ledger(
        ledger_path,
        &serde_json::json!({"event":"state","task_id":task_id,"state":"ROLLED_BACK","revert":sha}),
    );
    Ok(format!(
        "{task_id} 되돌림 완료 — revert {sha:.7}, 태스크는 PENDING (재실행 대기)"
    ))
}
