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

use std::collections::VecDeque;
use std::path::Path;
use std::process::ExitStatus;

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt};
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

const MAX_QUALITY_STREAM_BYTES: usize = 128 * 1024;
const QUALITY_READER_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

pub(super) struct BoundedCommandOutput {
    pub(super) status: ExitStatus,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) overflow: bool,
}

async fn read_bounded_tail<R>(mut reader: R, limit: usize) -> std::io::Result<(Vec<u8>, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut tail = VecDeque::with_capacity(limit);
    let mut chunk = [0u8; 8 * 1024];
    let mut overflow = false;
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        if read >= limit {
            tail.clear();
            tail.extend(&chunk[read - limit..read]);
            overflow = true;
            continue;
        }
        let excess = tail.len().saturating_add(read).saturating_sub(limit);
        if excess > 0 {
            tail.drain(..excess);
            overflow = true;
        }
        tail.extend(&chunk[..read]);
    }
    Ok((tail.into_iter().collect(), overflow))
}

async fn await_bounded_reader(
    mut reader: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
) -> Result<(Vec<u8>, bool), String> {
    match tokio::time::timeout(QUALITY_READER_SHUTDOWN_TIMEOUT, &mut reader).await {
        Ok(Ok(Ok(output))) => Ok(output),
        Ok(Ok(Err(error))) => Err(format!("출력 수집 실패: {error}")),
        Ok(Err(error)) => Err(format!("출력 수집 작업 실패: {error}")),
        Err(_) => {
            reader.abort();
            let _ = reader.await;
            Err("출력 수집 종료 시간 초과".into())
        }
    }
}

async fn await_bounded_readers(
    stdout: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
    stderr: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
) -> Result<(Vec<u8>, Vec<u8>, bool), String> {
    let (stdout, stderr) = tokio::join!(await_bounded_reader(stdout), await_bounded_reader(stderr));
    let (stdout, stdout_overflow) = stdout?;
    let (stderr, stderr_overflow) = stderr?;
    Ok((stdout, stderr, stdout_overflow || stderr_overflow))
}

pub(super) async fn run_bounded_command(
    program: &str,
    args: &[String],
    workspace: &Path,
    deadline: std::time::Duration,
) -> Result<BoundedCommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(workspace)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    crate::process_tree::isolate(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("실행 실패: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "표준 출력 파이프를 열 수 없습니다".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "표준 오류 파이프를 열 수 없습니다".to_string())?;
    let stdout_reader = tokio::spawn(read_bounded_tail(stdout, MAX_QUALITY_STREAM_BYTES));
    let stderr_reader = tokio::spawn(read_bounded_tail(stderr, MAX_QUALITY_STREAM_BYTES));

    let status = match tokio::time::timeout(deadline, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            crate::process_tree::terminate(&mut child).await;
            let _ = await_bounded_readers(stdout_reader, stderr_reader).await;
            return Err(format!("실행 대기 실패: {error}"));
        }
        Err(_) => {
            crate::process_tree::terminate(&mut child).await;
            let _ = await_bounded_readers(stdout_reader, stderr_reader).await;
            return Err(format!("시간 초과 ({}초)", deadline.as_secs()));
        }
    };
    let (stdout, stderr, overflow) = await_bounded_readers(stdout_reader, stderr_reader).await?;
    Ok(BoundedCommandOutput {
        status,
        stdout,
        stderr,
        overflow,
    })
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
    let relative = resolved
        .strip_prefix(&root)
        .map_err(|_| format!("워크스페이스 밖 변경 경로: {file}"))?;
    Ok(Some(relative.to_string_lossy().into_owned()))
}

fn is_node_syntax_source(file: &str) -> bool {
    matches!(
        Path::new(file)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "js" | "mjs" | "cjs"
    )
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
    run_argv_step_with_timeout(
        stage,
        display,
        program,
        args,
        workspace,
        std::time::Duration::from_secs(600),
    )
    .await
}

async fn run_argv_step_with_timeout(
    stage: &'static str,
    display: String,
    program: String,
    args: Vec<String>,
    workspace: &Path,
    deadline: std::time::Duration,
) -> GateStep {
    match run_bounded_command(&program, &args, workspace, deadline).await {
        Err(error) => GateStep {
            stage,
            command: display,
            passed: false,
            exit_code: None,
            note: Some(redact_local_output(&error, workspace)),
        },
        Ok(output) => {
            let code = output.status.code();
            let failed = !output.status.success() || output.overflow;
            let tail = {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stderr),
                    String::from_utf8_lossy(&output.stdout)
                );
                let lines: Vec<&str> = text.lines().collect();
                let skip = lines.len().saturating_sub(10);
                let tail: String = lines[skip..].join("\n").chars().take(1500).collect();
                redact_local_output(&tail, workspace)
            };
            let note = if output.overflow {
                Some(if tail.is_empty() {
                    "출력이 수집 상한을 초과했습니다".into()
                } else {
                    format!("출력이 수집 상한을 초과했습니다\n{tail}")
                })
            } else {
                failed.then_some(tail)
            };
            GateStep {
                stage,
                command: display,
                passed: !failed,
                exit_code: code,
                note,
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

fn package_free_javascript_fallback(
    language: Language,
    workspace: &Path,
    changed: &[String],
) -> bool {
    if language != Language::JavaScript || workspace.join("package.json").is_file() {
        return false;
    }
    let extensions = changed
        .iter()
        .filter_map(|file| Path::new(file).extension().and_then(|value| value.to_str()))
        .map(|extension| extension.to_ascii_lowercase())
        .filter(|extension| {
            matches!(
                extension.as_str(),
                "cjs" | "css" | "htm" | "html" | "js" | "jsx" | "mjs" | "ts" | "tsx"
            )
        })
        .collect::<Vec<_>>();
    !extensions.is_empty()
        && extensions.iter().all(|extension| {
            matches!(
                extension.as_str(),
                "cjs" | "css" | "htm" | "html" | "js" | "mjs"
            )
        })
}

/// 품질 게이트 전체 — S0 감지 → S3(포맷·린트·테스트) → S5(내장 스캔 + 의존성 감사).
/// S3·S5 는 어떤 경우에도 생략되지 않는다.
pub async fn run_quality_gate(workspace: &Path, changed: &[String]) -> QualityReport {
    let lang = profile::detect(workspace, changed);
    let profile = profile::profile_for(lang);
    let mut steps = Vec::new();
    let package_free_javascript = package_free_javascript_fallback(lang, workspace, changed);

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
                passed: package_free_javascript,
                exit_code: None,
                note: Some(if package_free_javascript {
                    "패키지 없는 JavaScript — node 구문·브라우저 게이트 적용".into()
                } else {
                    "필수 품질 도구 미설치".into()
                }),
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
        let node_available = profile::tool_available("node");
        for file in changed {
            if is_node_syntax_source(file) {
                match validated_changed_arg(workspace, file) {
                    Ok(Some(file)) => {
                        if node_available {
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
                        } else {
                            steps.push(GateStep {
                                stage: "S3-syntax",
                                command: format!("node --check {file:?}"),
                                passed: false,
                                exit_code: None,
                                note: Some("필수 JavaScript 구문 검사기 node 미설치".into()),
                            });
                        }
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
            passed: ok.unwrap_or(package_free_javascript),
            exit_code: None,
            note: Some(if ok.is_none() && !package_free_javascript {
                "필수 보안 감사 도구 미설치".into()
            } else {
                redact_local_output(&summary, workspace)
            }),
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
            let browser_entry_required = changed.iter().any(|file| {
                let path = workspace.join(file);
                path.is_file()
                    && matches!(
                        Path::new(file)
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .unwrap_or_default()
                            .to_ascii_lowercase()
                            .as_str(),
                        "css" | "htm" | "html"
                    )
            });
            if entries.is_empty() && browser_entry_required {
                steps.push(GateStep {
                    stage: "S4-smoke",
                    command: "browser entry discovery".into(),
                    passed: false,
                    exit_code: None,
                    note: Some("변경된 웹 자산을 실행할 HTML 엔트리를 찾지 못했습니다".into()),
                });
            }
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
                match browser::smoke_test_in_workspace(workspace, &entry).await {
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

#[derive(Clone, Copy)]
enum CredentialToken {
    Scheme,
    Secret,
}

fn strip_ansi_sequences(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = String::with_capacity(value.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == 0x1b {
            index += 1;
            if bytes.get(index) == Some(&b'[') {
                index += 1;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            } else if index < bytes.len() {
                index += 1;
            }
            continue;
        }
        let Some(character) = value[index..].chars().next() else {
            break;
        };
        output.push(character);
        index += character.len_utf8();
    }
    output
}

fn credential_token(value: &str) -> Option<CredentialToken> {
    let cleaned = strip_ansi_sequences(value);
    let cleaned = cleaned.trim_matches(|character: char| !character.is_ascii_alphanumeric());
    let lower = cleaned.to_ascii_lowercase();
    for scheme in ["basic", "bearer"] {
        if lower == scheme {
            return Some(CredentialToken::Scheme);
        }
        if let Some(rest) = lower.strip_prefix(scheme)
            && rest
                .chars()
                .next()
                .is_some_and(|character| !character.is_ascii_alphanumeric())
        {
            return Some(CredentialToken::Secret);
        }
    }
    None
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

fn redact_spaced_assignments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut copied_through = 0usize;
    let mut search = 0usize;
    while search < bytes.len() {
        let Some(offset) = bytes[search..]
            .iter()
            .position(|byte| matches!(*byte, b'=' | b':'))
        else {
            break;
        };
        let separator = search + offset;
        let mut key_end = separator;
        while key_end > 0 && matches!(bytes[key_end - 1], b' ' | b'\t' | b'\r') {
            key_end -= 1;
        }
        let mut key_start = key_end;
        while key_start > 0
            && (bytes[key_start - 1].is_ascii_alphanumeric()
                || matches!(bytes[key_start - 1], b'_' | b'-' | b'\'' | b'"'))
        {
            key_start -= 1;
        }
        let key = &text[key_start..key_end];
        let sensitive =
            looks_like_sensitive_key(key) || (bytes[separator] == b'=' && looks_like_env_key(key));
        if !sensitive {
            search = separator + 1;
            continue;
        }

        let mut value_start = separator + 1;
        while value_start < bytes.len()
            && matches!(bytes[value_start], b' ' | b'\t' | b'\r' | b'\n')
        {
            value_start += 1;
        }
        if value_start >= bytes.len() {
            search = separator + 1;
            continue;
        }
        let mut value_end = value_start;
        while value_end < bytes.len() && bytes[value_end] != b'\n' {
            value_end += 1;
        }
        let mut scheme_needs_secret = matches!(
            credential_token(text[value_start..value_end].trim()),
            Some(CredentialToken::Scheme)
        );
        while value_end < bytes.len() && bytes[value_end] == b'\n' {
            let next_line = value_end + 1;
            let mut content = next_line;
            while content < bytes.len() && matches!(bytes[content], b' ' | b'\t' | b'\r') {
                content += 1;
            }
            let indented = content != next_line;
            if (!indented && !scheme_needs_secret)
                || content >= bytes.len()
                || bytes[content] == b'\n'
            {
                break;
            }
            value_end = content;
            while value_end < bytes.len() && bytes[value_end] != b'\n' {
                value_end += 1;
            }
            scheme_needs_secret = false;
        }
        if value_end == value_start {
            search = separator + 1;
            continue;
        }
        output.push_str(&text[copied_through..value_start]);
        output.push_str("[redacted]");
        copied_through = value_end;
        search = value_end;
    }
    output.push_str(&text[copied_through..]);
    output
}

pub(crate) fn redact_local_output(text: &str, workspace: &Path) -> String {
    let cleaned = strip_ansi_sequences(text);
    let mut normalized = cleaned.replace(&workspace.display().to_string(), "<workspace>");
    if let Some(home) = std::env::var_os("HOME") {
        normalized = normalized.replace(&Path::new(&home).display().to_string(), "<home>");
    }
    normalized = redact_spaced_assignments(&normalized);
    let mut redacted = Vec::new();
    let mut redact_next = false;
    for token in normalized.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        if redact_next {
            match credential_token(token) {
                Some(CredentialToken::Scheme) => redacted.push(token.to_string()),
                Some(CredentialToken::Secret) | None => {
                    redacted.push("[redacted]".to_string());
                    redact_next = false;
                }
            }
        } else if let Some(kind) = credential_token(token) {
            match kind {
                CredentialToken::Scheme => {
                    redacted.push(token.to_string());
                    redact_next = true;
                }
                CredentialToken::Secret => redacted.push("[redacted]".to_string()),
            }
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
    fn only_package_free_javascript_can_skip_external_profile_tools() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-quality-required-tool-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        let browser_files = vec!["index.html".into(), "style.css".into(), "game.js".into()];
        assert!(package_free_javascript_fallback(
            Language::JavaScript,
            &root,
            &browser_files,
        ));
        assert!(!package_free_javascript_fallback(
            Language::JavaScript,
            &root,
            &["component.jsx".into()],
        ));
        assert!(!package_free_javascript_fallback(
            Language::Go,
            &root,
            &["main.go".into()],
        ));
        std::fs::write(root.join("package.json"), "{}").expect("package manifest");
        assert!(!package_free_javascript_fallback(
            Language::JavaScript,
            &root,
            &browser_files,
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn node_syntax_source_extensions_are_case_insensitive() {
        for file in ["app.js", "APP.JS", "module.MjS", "legacy.CJS"] {
            assert!(is_node_syntax_source(file), "not detected: {file}");
        }
        assert!(!is_node_syntax_source("component.jsx"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn quality_command_output_cap_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-quality-output-cap-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("temp workspace");
        let step = run_argv_step_with_timeout(
            "S3-test",
            "bounded fixture".into(),
            "sh".into(),
            vec![
                "-c".into(),
                "i=0; while [ \"$i\" -lt 20000 ]; do printf '0123456789abcdef\\n'; i=$((i + 1)); done"
                    .into(),
            ],
            &root,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(!step.passed);
        assert!(
            step.note
                .as_deref()
                .is_some_and(|note| note.contains("출력") && note.contains("상한"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn quality_command_timeout_kills_the_process() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-quality-timeout-kill-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("temp workspace");
        let step = run_argv_step_with_timeout(
            "S3-test",
            "timeout fixture".into(),
            "sh".into(),
            vec![
                "-c".into(),
                "(sleep 1; printf done > SURVIVED) & wait".into(),
            ],
            &root,
            std::time::Duration::from_millis(50),
        )
        .await;
        assert!(!step.passed);
        assert!(
            step.note
                .as_deref()
                .is_some_and(|note| note.contains("시간 초과"))
        );
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert!(!root.join("SURVIVED").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn repair_output_redacts_paths_and_credentials() {
        let workspace = Path::new("/tmp/private-workspace");
        let text = concat!(
            "/tmp/private-workspace/src/main.rs token=abc123 sk-example-secret\n",
            "Authorization: Bearer visible-credential\n",
            "Authorization: [Bearer BRACKET-LEAK]\n",
            "Authorization:\n  [Bearer MULTILINE-AUTH]\n",
            "Authorization:\nBearer\nTHREE-LINE-AUTH\n",
            "Authorization:\r\n  [Basic CRLF-BASIC]\r\n",
            "Proxy-Authorization: (Bearer PROXY-AUTH)\n",
            "API_KEY :\n API-MULTILINE\n",
            "Cookie: first=COOKIE-FIRST; second=COOKIE-SECOND\n",
            "[Bearer STANDALONE-DECORATED]\n",
            "\x1b[31mBearer\x1b[0m ANSI-AUTH\n",
            "TO\x1b[31mKEN\x1b[0m=ANSI-SPLIT-KEY\n",
            "AWS_SECRET_ACCESS_KEY=aws-value PRIVATE_VALUE=short-secret ",
            "API_KEY = \"hunter2\" {\"api_key\" : \"space secret\"}\n",
            "https://evil.test/x?value=query-secret postgres://user:password@host/db",
        );
        let redacted = redact_local_output(text, workspace);
        assert!(!redacted.contains("private-workspace"));
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("sk-example"));
        assert!(!redacted.contains("visible-credential"));
        assert!(!redacted.contains("aws-value"));
        assert!(!redacted.contains("short-secret"));
        assert!(!redacted.contains("query-secret"));
        assert!(!redacted.contains("user:password"));
        assert!(!redacted.contains("hunter2"));
        assert!(!redacted.contains("space secret"));
        for marker in [
            "BRACKET-LEAK",
            "MULTILINE-AUTH",
            "THREE-LINE-AUTH",
            "CRLF-BASIC",
            "PROXY-AUTH",
            "API-MULTILINE",
            "COOKIE-FIRST",
            "COOKIE-SECOND",
            "STANDALONE-DECORATED",
            "ANSI-AUTH",
            "ANSI-SPLIT-KEY",
        ] {
            assert!(!redacted.contains(marker), "leaked marker: {marker}");
        }
        assert!(redacted.contains("[redacted]"));
        assert!(redacted.contains("postgres://[redacted]@host/db"));
    }
}
