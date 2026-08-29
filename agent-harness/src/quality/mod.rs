//! 품질 엔진 게이트 실행기 — S3 기계 게이트 + S5 보안 게이트를 한 번에 돌리고
//! 구조화 리포트를 만든다. 근거: docs/agent-upgrade/07_QUALITY.md §2.
//!
//! 파이프라인 강도: S3·S5 는 항상 강제. 나머지(S1 설계노트·루브릭)는 태스크 데이터로.

pub mod browser;
pub mod design_note;
pub mod profile;
pub mod review;
pub mod rubric;
pub mod security;

use std::path::Path;

use serde::Serialize;
use tokio::process::Command;

pub use design_note::DesignNote;
pub use profile::{Language, LanguageProfile};
pub use browser::parse_console_errors;
pub use rubric::Rubric;
pub use security::Finding;

#[derive(Debug, Clone, Serialize)]
pub struct GateStep {
    pub stage: &'static str,
    pub command: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QualityReport {
    pub language: Language,
    pub steps: Vec<GateStep>,
    pub findings: Vec<security::Finding>,
    /// 전 게이트 통과 — 이 값이 false 면 산출물은 완료가 아니다.
    pub passed: bool,
}

/// S3 — 한 명령을 실행해 게이트 스텝으로 변환한다.
async fn run_step(stage: &'static str, cmd: &str, workspace: &Path) -> GateStep {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(600),
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(workspace)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await;
    match out {
        Err(_) => GateStep {
            stage,
            command: cmd.into(),
            passed: false,
            exit_code: None,
            note: Some("시간 초과 (600초)".into()),
        },
        Ok(Err(e)) => GateStep {
            stage,
            command: cmd.into(),
            passed: false,
            exit_code: None,
            note: Some(format!("실행 실패: {e}")),
        },
        Ok(Ok(o)) => {
            let code = o.status.code();
            let failed = code != Some(0);
            let tail: String = {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stderr),
                    String::from_utf8_lossy(&o.stdout)
                );
                let lines: Vec<&str> = text.lines().collect();
                let skip = lines.len().saturating_sub(10);
                lines[skip..].join("\n").chars().take(1500).collect()
            };
            GateStep {
                stage,
                command: cmd.into(),
                passed: !failed,
                exit_code: code,
                note: failed.then_some(tail),
            }
        }
    }
}

/// 품질 게이트 전체 — S0 감지 → S3(포맷·린트·테스트) → S5(내장 스캔 + 의존성 감사).
/// S3·S5 는 어떤 경우에도 생략되지 않는다: 도구가 없으면 "설치 권장" 기록 후
/// 내장 스캐너가 최소 보장을 한다.
pub async fn run_quality_gate(
    workspace: &Path,
    changed: &[String],
) -> QualityReport {
    let lang = profile::detect(workspace, changed);
    let profile = profile::profile_for(lang);
    let mut steps = Vec::new();

    // S3 — 포맷 체크 → 린트 → 테스트. 있는 도구만 실행, 없는 도구는 기록.
    for (stage, commands) in [
        ("S3-format", &profile.format_check),
        ("S3-lint", &profile.lint),
        ("S3-test", &profile.test),
    ] {
        let (runnable, missing) = profile::runnable_commands_in(workspace, commands);
        for cmd in missing {
            steps.push(GateStep {
                stage,
                command: cmd,
                passed: true, // 없는 도구로 실패 처리하지 않는다 — 대신 기록
                exit_code: None,
                note: Some("도구 미설치 — 설치 시 게이트에 합류".into()),
            });
        }
        for cmd in runnable {
            let cmd = if changed.is_empty() {
                cmd
            } else {
                cmd.replace("{files}", &changed.join(" "))
            };
            steps.push(run_step(stage, &cmd, workspace).await);
        }
    }

    // S3-syntax — node --check 파일별 구문 검사 (JS 계열, node 있을 때).
    if matches!(lang, Language::JavaScript | Language::TypeScript | Language::Unknown) {
        if profile::tool_available("node") {
            for file in changed {
                if file.ends_with(".js") || file.ends_with(".mjs") || file.ends_with(".cjs") {
                    steps.push(
                        run_step("S3-syntax", &format!("node --check {file:?}"), workspace).await,
                    );
                }
            }
        }
    }

    // S5 — 보안. 내장 스캐너는 변경 파일 원문을 직접 검사한다 (항상 강제).
    let mut findings = Vec::new();
    for file in changed {
        let path = workspace.join(file);
        if let Ok(text) = std::fs::read_to_string(&path) {
            findings.extend(security::scan(file, &text));
        }
    }
    // 프로필 금지 idiom 패턴도 스캔.
    for file in changed {
        let path = workspace.join(file);
        if let Ok(text) = std::fs::read_to_string(&path) {
            for (line_no, line) in text.lines().enumerate() {
                for (pattern, why) in &profile.forbidden_patterns {
                    if line.contains(pattern.as_str()) {
                        findings.push(security::Finding {
                            kind: "idiom",
                            file: file.to_string(),
                            line: line_no + 1,
                            detail: why.clone(),
                        });
                    }
                }
            }
        }
    }
    // 의존성 감사 — 도구 있으면 실행.
    for (cmd, ok, summary) in security::run_dependency_audit(lang, workspace).await {
        steps.push(GateStep {
            stage: "S5-audit",
            command: cmd.clone(),
            passed: ok.unwrap_or(true), // 미설치는 실패로 치지 않고 기록
            exit_code: None,
            note: Some(format!("{summary}")),
        });
    }

    for file in changed {
        let path = workspace.join(file);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let duplicates = detect_duplicate_blocks(file, &text);
            if !duplicates.is_empty() {
                steps.push(GateStep {
                    stage: "S7-duplication",
                    command: format!("duplicate scan {file}"),
                    passed: true,
                    exit_code: None,
                    note: Some(duplicates.join("\n")),
                });
            }
        }
    }

    // S4-smoke — 브라우저 런타임 스모크 (HTML/JS 산출물). 실측 실패(camTarget) 이후 추가된 게이트.
    let entry = changed
        .iter()
        .find(|f| f.ends_with(".html"))
        .map(|f| workspace.join(f))
        .or_else(|| {
            let idx = workspace.join("index.html");
            idx.exists().then_some(idx)
        });
    if let Some(entry) = entry {
        match browser::smoke_test(&entry).await {
            Ok(Some(errors)) if !errors.is_empty() => {
                for e in errors {
                    findings.push(security::Finding {
                        kind: "browser-error",
                        file: entry.display().to_string(),
                        line: 0,
                        detail: format!("브라우저 런타임 오류 — {e}"),
                    });
                }
            }
            Ok(Some(_)) => steps.push(GateStep {
                stage: "S4-smoke",
                command: "browser smoke (headless)".into(),
                passed: true,
                exit_code: Some(0),
                note: Some("콘솔 오류 0건".into()),
            }),
            Ok(None) => steps.push(GateStep {
                stage: "S4-smoke",
                command: "browser smoke (headless)".into(),
                passed: false,
                exit_code: None,
                note: Some("브라우저 미설치 — HTML 런타임을 검증할 수 없음".into()),
            }),
            Err(e) => steps.push(GateStep {
                stage: "S4-smoke",
                command: "browser smoke (headless)".into(),
                passed: false,
                exit_code: None,
                note: Some(format!("{e:#}")),
            }),
        }
    }

    let passed = steps.iter().all(|s| s.passed) && findings.is_empty();
    QualityReport {
        language: lang,
        steps,
        findings,
        passed,
    }
}

/// 중복 블록 감지 (S7 가독성·간결성 보조) — 의미 있는 5줄 구현이
/// 같은 파일에서 반복되면 지적한다 (레드팀 시나리오 6). 텍스트 기반 휴리스틱.
pub fn detect_duplicate_blocks(
    file: &str,
    text: &str,
) -> Vec<String> {
    let mut findings = Vec::new();
    let lines: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .map(|(line, text)| (line + 1, text.trim()))
        .filter(|(_, text)| !text.is_empty() && !text.starts_with("//"))
        .collect();
    let window = 5usize;
    if lines.len() < window * 2 {
        return findings;
    }
    let mut reported = std::collections::HashSet::new();
    for i in 0..=(lines.len().saturating_sub(window)) {
        for j in (i + window)..=(lines.len().saturating_sub(window)) {
            let first = &lines[i..i + window];
            let second = &lines[j..j + window];
            let same = first
                .iter()
                .zip(second)
                .all(|((_, left), (_, right))| left == right);
            let content_chars = first.iter().map(|(_, text)| text.len()).sum::<usize>();
            let fresh_region = reported
                .iter()
                .all(|start| j >= *start + window || *start >= j + window);
            if same
                && content_chars >= 120
                && fresh_region
                && reported.insert(j)
            {
                findings.push(format!(
                    "{file}:{} — 직전 블록({})과 동일한 5줄 중복",
                    second[0].0,
                    first[0].0
                ));
            }
        }
    }
    findings
}

/// 리포트 렌더 — 사람이 읽는 형태.
pub fn render_report(report: &QualityReport) -> String {
    let mut out = format!(
        "=== 품질 게이트 ({}) — {} ===\n",
        report.language.name(),
        if report.passed { "통과" } else { "실패" }
    );
    for s in &report.steps {
        let mark = if s.passed { "✓" } else { "✗" };
        out.push_str(&format!(
            "{mark} [{}] {}\n",
            s.stage, s.command
        ));
        if let Some(note) = &s.note {
            for line in note.lines().take(4) {
                out.push_str(&format!("    │ {line}\n"));
            }
        }
    }
    for f in &report.findings {
        out.push_str(&format!(
            "✗ [S5-{}] {}:{} — {}\n",
            f.kind, f.file, f.line, f.detail
        ));
    }
    out
}
