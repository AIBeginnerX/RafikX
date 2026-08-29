//! verification 실행기 (M1) — 시스템이 태스크의 검증 명령을 직접 실행하고
//! exit code·diff·무결성 가드 결과를 증거로 수집한다. 모델 출력은 어디에도
//! 판정 입력으로 쓰이지 않는다.

use std::process::Stdio;
use std::time::Instant;

use anyhow::Result;
use tokio::process::Command;

use super::guard;
use super::task::{CmdResults, DiffInfo, Evidence, TaskDoc, TaskOutcome, VerifiedReport};
use crate::verify::task::VerifierVerdict;

/// 한 명령을 직접 실행해 증거로 변환한다. 모델이 본 텍스트를 넘겨도 무의미하다 —
/// 판정 근거는 이 함수가 반환하는 exit code 뿐이다 (레드팀 시나리오 2).
async fn run_cmd(cmd: &str, timeout_secs: u64) -> Evidence {
    let started = Instant::now();
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await;
    let duration_ms = started.elapsed().as_millis();
    match output {
        Err(_) => Evidence {
            cmd: cmd.to_string(),
            exit_code: None,
            expect_exit: 0,
            passed: false,
            duration_ms,
            output_tail: format!("시간 초과 ({timeout_secs}초)"),
            diff_stat: None,
            guard: None,
            tests_passed: None,
            recorded_at: now_secs(),
        },
        Ok(Err(e)) => Evidence {
            cmd: cmd.to_string(),
            exit_code: None,
            expect_exit: 0,
            passed: false,
            duration_ms,
            output_tail: format!("실행 실패: {e}"),
            diff_stat: None,
            guard: None,
            tests_passed: None,
            recorded_at: now_secs(),
        },
        Ok(Ok(out)) => {
            let exit_code = out.status.code();
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let tail: String = {
                let lines: Vec<&str> = combined.lines().collect();
                let skip = lines.len().saturating_sub(20);
                lines[skip..].join("\n").chars().take(2000).collect()
            };
            Evidence {
                cmd: cmd.to_string(),
                exit_code,
                expect_exit: 0,
                passed: exit_code == Some(0),
                duration_ms,
                output_tail: tail.clone(),
                diff_stat: None,
                guard: None,
                recorded_at: now_secs(),
                tests_passed: parse_tests_passed(&tail),
            }
        }
    }
}

/// cargo/go 스타일 출력에서 통과 테스트 수를 뽑는다 ("test result: ok. 12 passed…").
pub(crate) fn parse_tests_passed(output: &str) -> Option<u32> {
    for line in output.lines() {
        let Some(idx) = line.find(" passed") else {
            continue;
        };
        let before = &line[..idx];
        let num: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if let Ok(n) = num.parse::<u32>() {
            return Some(n);
        }
    }
    None
}

/// 품질 래칫 — LEDGER 원장의 과거 최고 통과 테스트 수와 비교해 후퇴를 잡는다 (G17).
pub fn ratchet_check(ledger_path: &std::path::Path, current: u32) -> Option<String> {
    let raw = std::fs::read_to_string(ledger_path).ok()?;
    let mut best: Option<u32> = None;
    for line in raw.lines() {
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(n) = ev.get("tests_passed").and_then(|v| v.as_u64()) {
            best = Some(best.map_or(n as u32, |b: u32| b.max(n as u32)));
        }
    }
    let peak = best?;
    if current < peak {
        Some(format!(
            "테스트 수 래칫 위반: 과거 최고 {peak}회에서 {current}회로 후퇴했다"
        ))
    } else {
        None
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 워크스페이스의 변경 요약을 수집한다 (git 있으면 diff, 없으면 None).
async fn collect_diff(workspace: &std::path::Path) -> DiffInfo {
    let stat = Command::new("git")
        .args(["diff", "--stat", "HEAD"])
        .current_dir(workspace)
        .output()
        .await
        .ok();
    let diff = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(workspace)
        .output()
        .await
        .ok();
    let read = |out: &Option<std::process::Output>| {
        out.as_ref()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    };
    DiffInfo {
        stat: read(&stat),
        diff_text: read(&diff),
    }
}

/// 태스크 검증 전체 흐름 — 명령 실행 → diff 수집 → 무결성 가드 → 판정.
/// 반환값만이 DONE 의 근거이고, 호출부는 이를 TaskOutcome 으로 적용한다.
pub async fn run_task_verification(
    task: &TaskDoc,
    workspace: &std::path::Path,
) -> TaskOutcome {
    // 1) 명령 실행 — 시스템이 직접.
    let mut evidences = Vec::new();
    for vc in &task.verification {
        let mut ev = run_cmd(&vc.cmd, vc.timeout_secs).await;
        ev.expect_exit = vc.expect_exit;
        ev.passed = ev.exit_code == Some(vc.expect_exit);
        evidences.push(ev);
    }
    let results = CmdResults::new(evidences);

    // 2) diff 수집 — "수정 없는 완료" 차단의 근거.
    let diff = collect_diff(workspace).await;
    let diff_empty = diff
        .stat
        .as_deref()
        .map(str::trim)
        .map(str::is_empty)
        .unwrap_or(true);

    // 3) 테스트 무결성 가드 + 인수 테스트 불변.
    let mut violations = guard::check_test_integrity(diff.diff_text.as_deref().unwrap_or(""));
    violations.extend(guard::check_acceptance_immutable(
        diff.diff_text.as_deref().unwrap_or(""),
    ));

    // 4) 판정 — 시스템 규칙만 적용한다.
    if !results.all_passed {
        let failed: Vec<String> = results
            .results
            .iter()
            .filter(|e| !e.passed)
            .map(|e| {
                format!(
                    "{} → exit {:?} (기대 {})",
                    e.cmd, e.exit_code, e.expect_exit
                )
            })
            .collect();
        return TaskOutcome::Rework(format!("검증 실패: {}", failed.join("; ")));
    }
    if task.require_diff && diff_empty {
        return TaskOutcome::Rework(
            "변경된 파일이 없다 — 수정 없이 완료될 수 없다 (diff 부재)".into(),
        );
    }
    if !violations.is_empty() {
        return TaskOutcome::Rework(violations.join("; "));
    }
    TaskOutcome::Done(VerifiedReport { results, diff })
}

/// LEDGER.jsonl 한 줄 추가 — 증거 원장(설계 §6.9).
pub fn append_ledger(
    ledger_path: &std::path::Path,
    event: &serde_json::Value,
) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = ledger_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger_path)?;
    writeln!(f, "{event}")?;
    Ok(())
}

/// 검증자 판정 진입점 — M1 에서는 자동 규칙(가드+명령)이 판정하고,
/// M2 에서 독립 검증자(리뷰어)의 VerifierVerdict 가 여기에 합류한다.
pub fn auto_verdict(outcome_verified: bool, reason: &str) -> VerifierVerdict {
    if outcome_verified {
        VerifierVerdict::Pass
    } else {
        VerifierVerdict::Fail(reason.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::task::{TaskDoc, TaskState};

    fn task_with(cmds: &[&str], require_diff: bool) -> TaskDoc {
        let verification: Vec<serde_json::Value> = cmds
            .iter()
            .map(|c| serde_json::json!({"cmd": c}))
            .collect();
        serde_json::from_value(serde_json::json!({
            "id": "T-T",
            "title": "테스트",
            "require_diff": require_diff,
            "verification": verification,
            "state": "PENDING",
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn failing_command_blocks_done() {
        let t = task_with(&["exit 3"], false);
        let outcome = run_task_verification(&t, std::env::temp_dir().as_path()).await;
        let mut t = t;
        let state = t.apply(outcome);
        assert_eq!(*state, TaskState::Rework);
        assert!(t.evidence.last().unwrap().output_tail.contains("exit Some(3)"));
    }

    #[tokio::test]
    async fn passing_commands_without_diff_fail_when_required() {
        let t = task_with(&["true"], true);
        // 임시 디렉터는 git 이 아니므로 diff 수집 실패 → diff 없음 취급.
        let dir = std::env::temp_dir().join(format!("rk-verify-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let outcome = run_task_verification(&t, &dir).await;
        let mut t = t;
        let state = t.apply(outcome);
        assert_eq!(*state, TaskState::Rework, "diff 부재는 Rework");
        assert!(t.evidence.last().unwrap().output_tail.contains("diff 부재"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod ratchet_tests {
    use super::*;

    #[test]
    fn parses_cargo_test_counts() {
        let out = "running 5 tests\ntest a ... ok\ntest b ... ok\n\ntest result: ok. 12 passed; 0 failed; 0 ignored\n";
        assert_eq!(parse_tests_passed(out), Some(12));
        assert_eq!(parse_tests_passed("no test output"), None);
    }

    #[test]
    fn ratchet_blocks_regression_but_allows_progress() {
        let dir = std::env::temp_dir().join(format!("rk-ratchet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join("LEDGER.jsonl");
        std::fs::write(
            &ledger,
            "{\"event\":\"metric\",\"tests_passed\":20}\n{\"event\":\"other\"}\n",
        )
        .unwrap();
        assert!(ratchet_check(&ledger, 19).is_some(), "후퇴 감지");
        assert!(ratchet_check(&ledger, 20).is_none(), "동점은 통과");
        assert!(ratchet_check(&ledger, 21).is_none(), "진행은 통과");
        assert!(ratchet_check(&dir.join("none.jsonl"), 1).is_none(), "원장 없음 = 래칫 없음");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
