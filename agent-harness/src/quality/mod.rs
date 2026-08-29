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

pub use browser::parse_console_errors;
pub use design_note::DesignNote;
pub use profile::{Language, LanguageProfile};
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

fn validated_changed_arg(workspace: &Path, file: &str) -> Result<Option<String>, String> {
    let relative = Path::new(file);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(format!("워크스페이스 밖 변경 경로: {file}"));
    }
    let joined = workspace.join(relative);
    if !joined.exists() {
        return Ok(None);
    }
    let root = workspace
        .canonicalize()
        .map_err(|error| format!("워크스페이스 확인 실패: {error}"))?;
    let resolved = joined
        .canonicalize()
        .map_err(|error| format!("변경 경로 확인 실패({file}): {error}"))?;
    if !resolved.starts_with(&root) {
        return Err(format!("워크스페이스 밖 변경 경로: {file}"));
    }
    Ok(Some(file.to_string()))
}

fn command_argv(
    cmd: &str,
    workspace: &Path,
    changed: &[String],
) -> Result<Option<(String, Vec<String>)>, String> {
    let mut words = cmd.split_whitespace();
    let program = words
        .next()
        .filter(|word| !word.is_empty())
        .ok_or_else(|| "빈 품질 명령".to_string())?
        .to_string();
    let mut args = Vec::new();
    let mut had_files_placeholder = false;
    let mut file_args = 0usize;
    for word in words {
        if word == "{files}" {
            had_files_placeholder = true;
            for file in changed {
                if let Some(file) = validated_changed_arg(workspace, file)? {
                    args.push(file);
                    file_args += 1;
                }
            }
        } else {
            args.push(word.to_string());
        }
    }
    if had_files_placeholder && file_args == 0 {
        return Ok(None);
    }
    Ok(Some((program, args)))
}

/// S3 — 셸을 거치지 않고 프로그램과 인자를 분리해 게이트 스텝으로 변환한다.
async fn run_argv_step(
    stage: &'static str,
    display: String,
    program: String,
    args: Vec<String>,
    workspace: &Path,
) -> GateStep {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(600),
        Command::new(&program)
            .args(&args)
            .current_dir(workspace)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await;
    match out {
        Err(_) => GateStep {
            stage,
            command: display,
            passed: false,
            exit_code: None,
            note: Some("시간 초과 (600초)".into()),
        },
        Ok(Err(e)) => GateStep {
            stage,
            command: display,
            passed: false,
            exit_code: None,
            note: Some(redact_local_output(&format!("실행 실패: {e}"), workspace)),
        },
        Ok(Ok(o)) => {
            let code = o.status.code();
            let failed = code != Some(0);
            let tail = {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stderr),
                    String::from_utf8_lossy(&o.stdout)
                );
                let lines: Vec<&str> = text.lines().collect();
                let skip = lines.len().saturating_sub(10);
                let tail: String = lines[skip..].join("\n").chars().take(1500).collect();
                redact_local_output(&tail, workspace)
            };
            GateStep {
                stage,
                command: display,
                passed: !failed,
                exit_code: code,
                note: failed.then_some(tail),
            }
        }
    }
}

async fn run_step(
    stage: &'static str,
    cmd: &str,
    workspace: &Path,
    changed: &[String],
) -> GateStep {
    let display = cmd.replace("{files}", "<changed files>");
    match command_argv(cmd, workspace, changed) {
        Ok(Some((program, args))) => run_argv_step(stage, display, program, args, workspace).await,
        Ok(None) => GateStep {
            stage,
            command: display,
            passed: true,
            exit_code: None,
            note: Some("실행할 변경 파일 없음".into()),
        },
        Err(error) => GateStep {
            stage,
            command: display,
            passed: false,
            exit_code: None,
            note: Some(error),
        },
    }
}

/// 품질 게이트 전체 — S0 감지 → S3(포맷·린트·테스트) → S5(내장 스캔 + 의존성 감사).
/// S3·S5 는 어떤 경우에도 생략되지 않는다: 도구가 없으면 "설치 권장" 기록 후
/// 내장 스캐너가 최소 보장을 한다.
pub async fn run_quality_gate(workspace: &Path, changed: &[String]) -> QualityReport {
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
            steps.push(run_step(stage, &cmd, workspace, changed).await);
        }
    }

    // S3-syntax — node --check 파일별 구문 검사 (JS 계열, node 있을 때).
    if matches!(
        lang,
        Language::JavaScript | Language::TypeScript | Language::Unknown
    ) {
        if profile::tool_available("node") {
            for file in changed {
                if file.ends_with(".js") || file.ends_with(".mjs") || file.ends_with(".cjs") {
                    match validated_changed_arg(workspace, file) {
                        Ok(Some(file)) => {
                            steps.push(
                                run_argv_step(
                                    "S3-syntax",
                                    format!("node --check {file:?}"),
                                    "node".into(),
                                    vec!["--check".into(), file],
                                    workspace,
                                )
                                .await,
                            );
                        }
                        Ok(None) => {}
                        Err(error) => steps.push(GateStep {
                            stage: "S3-syntax",
                            command: format!("node --check {file:?}"),
                            passed: false,
                            exit_code: None,
                            note: Some(error),
                        }),
                    }
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
            note: Some(redact_local_output(&summary, workspace)),
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
    match browser::discover_entries(workspace, changed) {
        Err(error) => steps.push(GateStep {
            stage: "S4-smoke",
            command: "browser entry discovery".into(),
            passed: false,
            exit_code: None,
            note: Some(redact_local_output(&format!("{error:#}"), workspace)),
        }),
        Ok(entries) => {
            let display_root = workspace
                .canonicalize()
                .unwrap_or_else(|_| workspace.to_path_buf());
            for entry in entries {
                let display_entry = entry
                    .strip_prefix(&display_root)
                    .unwrap_or(&entry)
                    .display()
                    .to_string();
                let command = format!("browser smoke (headless): {display_entry}");
                match browser::smoke_test(workspace, &entry).await {
                    Ok(Some(errors)) if !errors.is_empty() => {
                        let count = errors.len();
                        for error in errors {
                            findings.push(security::Finding {
                                kind: "browser-error",
                                file: display_entry.clone(),
                                line: 0,
                                detail: redact_local_output(
                                    &format!("브라우저 런타임 오류 — {error}"),
                                    workspace,
                                ),
                            });
                        }
                        steps.push(GateStep {
                            stage: "S4-smoke",
                            command,
                            passed: false,
                            exit_code: None,
                            note: Some(format!("브라우저 런타임 오류 {count}건")),
                        });
                    }
                    Ok(Some(_)) => steps.push(GateStep {
                        stage: "S4-smoke",
                        command,
                        passed: true,
                        exit_code: Some(0),
                        note: Some("콘솔 오류 0건".into()),
                    }),
                    Ok(None) => steps.push(GateStep {
                        stage: "S4-smoke",
                        command,
                        passed: false,
                        exit_code: None,
                        note: Some("브라우저 미설치 — HTML 런타임을 검증할 수 없음".into()),
                    }),
                    Err(error) => steps.push(GateStep {
                        stage: "S4-smoke",
                        command,
                        passed: false,
                        exit_code: None,
                        note: Some(redact_local_output(&format!("{error:#}"), workspace)),
                    }),
                }
            }
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
pub fn detect_duplicate_blocks(file: &str, text: &str) -> Vec<String> {
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
            if same && content_chars >= 120 && fresh_region && reported.insert(j) {
                findings.push(format!(
                    "{file}:{} — 직전 블록({})과 동일한 5줄 중복",
                    second[0].0, first[0].0
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
        out.push_str(&format!("{mark} [{}] {}\n", s.stage, s.command));
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

fn looks_like_high_entropy_secret(token: &str) -> bool {
    let value = token.trim_matches(|character: char| {
        !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_')
    });
    value.len() >= 32
        && !value.contains('/')
        && value
            .chars()
            .any(|character| character.is_ascii_alphabetic())
        && value.chars().any(|character| character.is_ascii_digit())
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn looks_like_sensitive_key(key: &str) -> bool {
    let key = key
        .trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_')
        })
        .to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "access_key",
        "authorization",
        "cookie",
        "credential",
        "password",
        "passwd",
        "private_key",
        "secret",
        "token",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

fn looks_like_env_key(key: &str) -> bool {
    let key =
        key.trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '_');
    key.len() >= 3
        && key
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_uppercase())
        && key.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

fn redact_url(token: &str) -> Option<String> {
    let scheme_end = token.find("://")?.saturating_add(3);
    let authority_end = token[scheme_end..]
        .find(['/', '?', '#'])
        .map(|index| scheme_end + index)
        .unwrap_or(token.len());
    let mut sanitized = token.to_string();
    let mut changed = false;
    if let Some(at) = token[scheme_end..authority_end].rfind('@') {
        sanitized.replace_range(scheme_end..scheme_end + at, "[redacted]");
        changed = true;
    }
    if let Some(split) = sanitized.find(['?', '#']) {
        sanitized.truncate(split);
        sanitized.push_str("?[redacted]");
        changed = true;
    }
    changed.then_some(sanitized)
}

pub(crate) fn redact_local_output(text: &str, workspace: &Path) -> String {
    let mut normalized = text.replace(&workspace.display().to_string(), "<workspace>");
    if let Some(home) = std::env::var_os("HOME") {
        normalized = normalized.replace(&Path::new(&home).display().to_string(), "<home>");
    }
    let mut redacted = Vec::new();
    let mut redact_next = false;
    for token in normalized.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        if redact_next {
            if matches!(lower.as_str(), "basic" | "bearer") {
                redacted.push(token.to_string());
            } else {
                redacted.push("[redacted]".to_string());
                redact_next = false;
            }
        } else if matches!(lower.as_str(), "authorization:" | "basic" | "bearer") {
            redacted.push(token.to_string());
            redact_next = true;
        } else if let Some(url) = redact_url(token) {
            redacted.push(url);
        } else if let Some(at) = token.find(['=', ':'])
            && (looks_like_sensitive_key(&token[..at])
                || (token.as_bytes()[at] == b'=' && looks_like_env_key(&token[..at])))
        {
            redacted.push(format!("{}[redacted]", &token[..=at]));
            redact_next = at + 1 == token.len();
        } else if lower.starts_with("sk-")
            || lower.starts_with("ghp_")
            || lower.starts_with("github_pat_")
            || lower.starts_with("xoxb-")
            || looks_like_high_entropy_secret(token)
        {
            redacted.push("[redacted]".to_string());
        } else {
            redacted.push(token.to_string());
        }
    }
    redacted.join(" ")
}

pub(crate) fn repair_evidence(report: &QualityReport, workspace: &Path) -> String {
    let mut evidence = String::new();
    for step in report.steps.iter().filter(|step| !step.passed) {
        evidence.push_str(&format!(
            "[{}] {}",
            step.stage,
            redact_local_output(&step.command, workspace)
        ));
        if let Some(note) = &step.note {
            evidence.push_str(": ");
            evidence.push_str(&redact_local_output(note, workspace));
        }
        evidence.push('\n');
    }
    for finding in &report.findings {
        evidence.push_str(&format!(
            "[{}] {}:{} {}\n",
            finding.kind,
            redact_local_output(&finding.file, workspace),
            finding.line,
            redact_local_output(&finding.detail, workspace)
        ));
    }
    evidence.chars().take(4000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn changed_javascript_paths_are_argv_not_shell_source() {
        if !profile::tool_available("node") {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("rafikx-quality-argv-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&root).expect("temp workspace");
        let names = [
            "$(touch PWNED).js",
            "`touch ALSO_PWNED`.js",
            "quote' and space.js",
            "line\nbreak.js",
        ];
        for name in names {
            std::fs::write(root.join(name), "const value = 1;\n").expect("fixture");
        }

        let changed = names
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        let report = run_quality_gate(&root, &changed).await;
        assert!(
            report
                .steps
                .iter()
                .filter(|step| step.stage == "S3-syntax")
                .all(|step| step.passed),
            "syntax steps: {:?}",
            report.steps
        );
        assert!(!root.join("PWNED").exists());
        assert!(!root.join("ALSO_PWNED").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn repair_output_redacts_paths_and_credentials() {
        let workspace = Path::new("/tmp/private-workspace");
        let text = "/tmp/private-workspace/src/main.rs token=abc123 sk-example-secret Authorization: Bearer visible-credential AWS_SECRET_ACCESS_KEY=aws-value PRIVATE_VALUE=short-secret {\"api_key\":\"json-value\"} Cookie: session-value https://evil.test/x?value=query-secret postgres://user:password@host/db";
        let redacted = redact_local_output(text, workspace);
        assert!(!redacted.contains("private-workspace"));
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("sk-example"));
        assert!(!redacted.contains("visible-credential"));
        assert!(!redacted.contains("aws-value"));
        assert!(!redacted.contains("json-value"));
        assert!(!redacted.contains("session-value"));
        assert!(!redacted.contains("short-secret"));
        assert!(!redacted.contains("query-secret"));
        assert!(!redacted.contains("user:password"));
        assert!(redacted.contains("postgres://[redacted]@host/db"));
    }
}
