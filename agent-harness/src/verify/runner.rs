//! verification 실행기 (M1) — 시스템이 태스크의 검증 명령을 직접 실행하고
//! exit code·diff·무결성 가드 결과를 증거로 수집한다. 모델 출력은 어디에도
//! 판정 입력으로 쓰이지 않는다.

use std::time::Instant;

use anyhow::Result;

use super::guard;
use super::task::{CmdResults, DiffInfo, Evidence, TaskDoc, TaskOutcome, VerifiedReport};
use crate::verify::task::VerifierVerdict;

/// 한 명령을 직접 실행해 증거로 변환한다. 모델이 본 텍스트를 넘겨도 무의미하다 —
/// 판정 근거는 이 함수가 반환하는 exit code 뿐이다 (레드팀 시나리오 2).
async fn run_cmd(cmd: &str, timeout_secs: u64, workspace: &std::path::Path) -> Evidence {
    let started = Instant::now();
    #[cfg(windows)]
    let (program, args) = (
        "cmd",
        vec![
            "/D".to_string(),
            "/S".to_string(),
            "/C".to_string(),
            cmd.to_string(),
        ],
    );
    #[cfg(not(windows))]
    let (program, args) = ("sh", vec!["-c".to_string(), cmd.to_string()]);
    let output = crate::quality::run_bounded_command(
        program,
        &args,
        workspace,
        std::time::Duration::from_secs(timeout_secs),
    )
    .await;
    let duration_ms = started.elapsed().as_millis();
    match output {
        Err(error) => Evidence {
            cmd: cmd.to_string(),
            exit_code: None,
            expect_exit: 0,
            passed: false,
            duration_ms,
            output_tail: error,
            diff_stat: None,
            guard: None,
            tests_passed: None,
            recorded_at: now_secs(),
        },
        Ok(out) => {
            let exit_code = (!out.overflow).then(|| out.status.code()).flatten();
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let mut tail: String = {
                let lines: Vec<&str> = combined.lines().collect();
                let skip = lines.len().saturating_sub(20);
                lines[skip..].join("\n").chars().take(2000).collect()
            };
            if out.overflow {
                tail = format!("출력 상한 초과 — 검증을 신뢰할 수 없습니다\n{tail}");
            }
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

/// `git diff --stat` 꼬리줄("2 files changed, 10 insertions(+), 3 deletions(-)")에서
/// 총 변경 줄 수를 뽑는다.
pub(crate) fn parse_changed_lines(stat: &str) -> usize {
    let Some(last) = stat.lines().last() else {
        return 0;
    };
    let mut total = 0usize;
    for (word, next_word) in [("insertion", "insertions"), ("deletion", "deletions")] {
        let _ = next_word;
        if let Some(idx) = last.find(word) {
            let before = &last[..idx];
            let num: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == ' ')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            total += num.trim().parse::<usize>().unwrap_or(0);
        }
    }
    total
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
async fn collect_git_output(
    workspace: &std::path::Path,
    args: &[&str],
) -> Result<Option<String>, String> {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let output = crate::quality::run_bounded_command(
        "git",
        &args,
        workspace,
        std::time::Duration::from_secs(10),
    )
    .await?;
    if output.overflow {
        return Err(format!("git {} 출력 상한 초과", args.join(" ")));
    }
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string()))
}

async fn collect_diff(workspace: &std::path::Path) -> Result<DiffInfo, String> {
    Ok(DiffInfo {
        stat: collect_git_output(workspace, &["diff", "--stat", "HEAD"]).await?,
        diff_text: collect_git_output(workspace, &["diff", "HEAD"]).await?,
    })
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
        let mut ev = run_cmd(&vc.cmd, vc.timeout_secs, workspace).await;
        ev.expect_exit = vc.expect_exit;
        ev.passed = ev.exit_code == Some(vc.expect_exit);
        evidences.push(ev);
    }
    let results = CmdResults::new(evidences);

    // 2) diff 수집 — "수정 없는 완료" 차단의 근거.
    let diff = match collect_diff(workspace).await {
        Ok(diff) => diff,
        Err(error) => return TaskOutcome::Rework(format!("diff 수집 실패: {error}")),
    };
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
    // 변경 크기 제한 (G16) — 큰 diff 는 분해를 강제한다 (설계 §6.2 constraints).
    if let Some(stat) = &diff.stat {
        let total = parse_changed_lines(stat);
        if total > task.constraints.max_diff_lines {
            return TaskOutcome::Rework(format!(
                "변경 크기 초과: {total}줄 > 상한 {}줄 — 태스크를 더 작게 분해하라",
                task.constraints.max_diff_lines
            ));
        }
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

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_verification_cleans_up_descendants() {
        let dir =
            std::env::temp_dir().join(format!("rk-verify-timeout-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("escaped");
        let command = format!(
            "(sleep 2; printf escaped > \"{}\") & sleep 30",
            marker.display()
        );

        let evidence = run_cmd(&command, 1, &dir).await;

        assert!(!evidence.passed);
        assert!(evidence.output_tail.contains("시간 초과"));
        tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
        assert!(
            !marker.exists(),
            "시간 초과한 자손 프로세스가 살아남았습니다"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_verification_output_fails_closed() {
        let evidence = run_cmd(
            "yes rafikx | head -c 300000",
            5,
            std::env::temp_dir().as_path(),
        )
        .await;

        assert!(!evidence.passed);
        assert_eq!(evidence.exit_code, None);
        assert!(evidence.output_tail.contains("출력 상한 초과"));
    }

    #[tokio::test]
    async fn verification_command_runs_in_the_task_workspace() {
        let dir =
            std::env::temp_dir().join(format!("rk-verify-workspace-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("workspace-marker"), "ok").unwrap();

        #[cfg(windows)]
        let command = "if exist workspace-marker (exit 0) else (exit 7)";
        #[cfg(not(windows))]
        let command = "test -f workspace-marker";
        let evidence = run_cmd(command, 5, &dir).await;

        assert!(evidence.passed, "{}", evidence.output_tail);
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

#[cfg(test)]
mod size_limit_tests {
    use super::*;

    #[test]
    fn parses_changed_lines_from_stat() {
        let stat = " app.ts | 2 +-\n t.rs | 18 ++++++++++++++++\n 2 files changed, 20 insertions(+), 2 deletions(-)";
        assert_eq!(parse_changed_lines(stat), 22);
        assert_eq!(parse_changed_lines(" 1 file changed, 5 insertions(+)"), 5);
        assert_eq!(parse_changed_lines(""), 0);
    }

    #[tokio::test]
    async fn oversized_diff_fails_task() {
        let t: TaskDoc = serde_json::from_value(serde_json::json!({
            "id": "T-BIG", "title": "큰 변경",
            "constraints": {"max_diff_lines": 5},
            "require_diff": false,
            "verification": [{"cmd": "true"}],
            "state": "PENDING"
        }))
        .unwrap();
        // 임시 git 저장소에 10줄 변경 → 상한 5줄 초과.
        let dir = std::env::temp_dir().join(format!("rk-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "a\n").unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .unwrap();
        for (key, value) in [("user.email", "t@t"), ("user.name", "RafikX Test")] {
            let status = std::process::Command::new("git")
                .args(["config", key, value])
                .current_dir(&dir)
                .status()
                .unwrap();
            assert!(status.success(), "git config {key}");
        }
        let status = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success(), "git add");
        let status = std::process::Command::new("git")
            .args(["commit", "-qm", "init"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success(), "git commit");
        let mut big = String::new();
        for i in 0..10 {
            big.push_str(&format!("line {i}\n"));
        }
        std::fs::write(dir.join("f.txt"), big).unwrap();
        let outcome = run_task_verification(&t, &dir).await;
        let mut t = t;
        let state = t.apply(outcome);
        assert_eq!(*state, crate::verify::TaskState::Rework, "상한 초과는 Rework");
        assert!(t.evidence.last().unwrap().output_tail.contains("변경 크기 초과"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
