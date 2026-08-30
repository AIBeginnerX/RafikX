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

use std::collections::{BTreeSet, VecDeque};
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

pub(crate) struct BoundedCommandOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) overflow: bool,
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

pub(crate) async fn run_bounded_command(
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
    let (mut child, process_scope) = crate::process_tree::spawn_scoped(&mut command)
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
            let cleanup = crate::process_tree::terminate(child, process_scope).await;
            let _ = await_bounded_readers(stdout_reader, stderr_reader).await;
            cleanup.map_err(|cleanup| format!("실행 대기 실패 후 정리 실패: {cleanup}"))?;
            return Err(format!("실행 대기 실패: {error}"));
        }
        Err(_) => {
            let cleanup = crate::process_tree::terminate(child, process_scope).await;
            let _ = await_bounded_readers(stdout_reader, stderr_reader).await;
            cleanup.map_err(|cleanup| format!("시간 초과 후 프로세스 정리 실패: {cleanup}"))?;
            return Err(format!("시간 초과 ({}초)", deadline.as_secs()));
        }
    };
    crate::process_tree::terminate(child, process_scope)
        .await
        .map_err(|cleanup| format!("명령 종료 후 프로세스 정리 실패: {cleanup}"))?;
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

fn is_typescript_syntax_source(file: &str) -> bool {
    matches!(
        Path::new(file)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "ts" | "tsx" | "jsx"
    )
}

fn typescript_validator(workspace: &Path) -> Option<String> {
    let local = workspace.join("node_modules/.bin/tsc");
    if local.is_file() {
        return Some("./node_modules/.bin/tsc".into());
    }
    #[cfg(windows)]
    {
        let local = workspace.join("node_modules/.bin/tsc.cmd");
        if local.is_file() {
            return Some("./node_modules/.bin/tsc.cmd".into());
        }
    }
    profile::tool_available("tsc").then(|| "tsc".into())
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
        if matches!(word, "{files}" | "{rust_files}") {
            had_files_placeholder = true;
            for file in changed {
                if word == "{rust_files}"
                    && !Path::new(file)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
                {
                    continue;
                }
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

fn successful_stdout_is_failure(program: &str, args: &[String], stdout: &[u8]) -> bool {
    if stdout.is_empty() {
        return false;
    }
    let executable = Path::new(program)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
        .to_ascii_lowercase();
    match executable.as_str() {
        "gofmt" => args.iter().any(|arg| arg == "-l" || arg == "-d"),
        "shfmt" => args.iter().any(|arg| arg == "-d"),
        _ => false,
    }
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
            let format_diff = output.status.success()
                && successful_stdout_is_failure(&program, &args, &output.stdout);
            let failed = !output.status.success() || output.overflow || format_diff;
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
            } else if format_diff {
                Some(if tail.is_empty() {
                    "포맷 차이가 출력되었습니다".into()
                } else {
                    tail
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
    let display = cmd
        .replace("{files}", "<changed files>")
        .replace("{rust_files}", "<changed Rust files>");
    if stage == "S3-lint" && cmd.starts_with("cargo clippy ") {
        return run_rust_clippy_step(display, cmd, workspace, changed).await;
    }
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

fn normalized_changed_files(
    workspace: &Path,
    changed: &[String],
) -> Result<BTreeSet<String>, String> {
    changed
        .iter()
        .filter_map(|file| validated_changed_arg(workspace, file).transpose())
        .map(|file| file.map(|path| path.replace('\\', "/")))
        .collect()
}

fn clippy_warning_summary(output: &str, changed: &BTreeSet<String>) -> (usize, Vec<String>) {
    let mut baseline = 0usize;
    let mut blocking = Vec::new();
    let output = strip_ansi_sequences(output);
    for line in output.lines() {
        let Some((location, message)) = line.split_once(": warning: ") else {
            continue;
        };
        let file = location
            .rsplit_once(':')
            .map(|(location, _)| location)
            .and_then(|location| location.rsplit_once(':').map(|(file, _)| file))
            .unwrap_or(location)
            .replace('\\', "/");
        if changed.is_empty() || changed.contains(&file) {
            blocking.push(message.to_string());
        } else {
            baseline += 1;
        }
    }
    blocking.sort();
    blocking.dedup();
    (baseline, blocking)
}

async fn run_rust_clippy_step(
    display: String,
    cmd: &str,
    workspace: &Path,
    changed: &[String],
) -> GateStep {
    let changed = match normalized_changed_files(workspace, changed) {
        Ok(changed) => changed,
        Err(error) => {
            return GateStep {
                stage: "S3-lint",
                command: display,
                passed: false,
                exit_code: None,
                note: Some(error),
            };
        }
    };
    let Some((program, args)) = command_argv(cmd, workspace, &[]).ok().flatten() else {
        return GateStep {
            stage: "S3-lint",
            command: display,
            passed: false,
            exit_code: None,
            note: Some("clippy 명령을 구성하지 못했습니다".into()),
        };
    };
    match run_bounded_command(
        &program,
        &args,
        workspace,
        std::time::Duration::from_secs(600),
    )
    .await
    {
        Err(error) => GateStep {
            stage: "S3-lint",
            command: display,
            passed: false,
            exit_code: None,
            note: Some(redact_local_output(&error, workspace)),
        },
        Ok(output) => {
            let code = output.status.code();
            let detail = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
            let (baseline, blocking) = clippy_warning_summary(&detail, &changed);
            let passed = output.status.success() && !output.overflow && blocking.is_empty();
            let note = if output.overflow {
                Some("clippy 출력이 수집 상한을 초과했습니다".into())
            } else if !blocking.is_empty() {
                Some(blocking.into_iter().take(10).collect::<Vec<_>>().join("\n"))
            } else if !output.status.success() {
                Some(redact_local_output(
                    &detail.chars().take(1500).collect::<String>(),
                    workspace,
                ))
            } else if baseline > 0 {
                Some(format!("변경 밖 기존 clippy 경고 {baseline}건 격리"))
            } else {
                None
            };
            GateStep {
                stage: "S3-lint",
                command: display,
                passed,
                exit_code: code,
                note,
            }
        }
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

fn browser_game_contract_target(html: &str, required: bool, entry_count: usize) -> bool {
    required
        && (entry_count <= 1
            || browser::entry_requires_game_contract(html)
            || browser::entry_looks_like_browser_game(html))
}

fn has_adjacent_safety_justification(lines: &[&str], line: usize) -> bool {
    lines[..line]
        .iter()
        .rev()
        .take_while(|candidate| candidate.trim_start().starts_with("//"))
        .take(4)
        .any(|candidate| {
            candidate
                .trim_start()
                .strip_prefix("//")
                .map(str::trim_start)
                .and_then(|comment| comment.strip_prefix("SAFETY:"))
                .is_some_and(|justification| !justification.trim().is_empty())
        })
}

fn code_contains_pattern(line: &str, pattern: &str) -> bool {
    let code = line.split("//").next().unwrap_or_default();
    let mut search = 0usize;
    while let Some(found) = code[search..].find(pattern) {
        let at = search + found;
        let quoted = code[..at]
            .char_indices()
            .filter(|(index, character)| {
                *character == '"'
                    && (*index == 0 || code.as_bytes().get(index - 1).copied() != Some(b'\\'))
            })
            .count()
            % 2
            == 1;
        if !quoted {
            return true;
        }
        search = at + pattern.len();
    }
    false
}

#[derive(Default)]
struct RustScanState {
    block_comment_depth: usize,
    string: bool,
    raw_hashes: Option<usize>,
}

fn rust_char_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start + 1;
    if bytes.get(cursor) == Some(&b'\\') {
        cursor += 1;
        while cursor < bytes.len() {
            if bytes[cursor] == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
            } else if bytes[cursor] == b'\'' {
                return Some(cursor + 1);
            } else {
                cursor += 1;
            }
        }
        return None;
    }
    let width = std::str::from_utf8(bytes.get(cursor..)?)
        .ok()?
        .chars()
        .next()?
        .len_utf8();
    cursor += width;
    (bytes.get(cursor) == Some(&b'\'')).then_some(cursor + 1)
}

fn rust_line_facts(line: &str, state: &mut RustScanState) -> (i32, i32, bool, String) {
    let bytes = line.as_bytes();
    let mut visible = vec![b' '; bytes.len()];
    let mut cursor = 0usize;
    let mut opens = 0i32;
    let mut closes = 0i32;
    let mut unsafe_token = false;
    while cursor < bytes.len() {
        if let Some(hashes) = state.raw_hashes {
            if bytes[cursor] == b'"'
                && bytes
                    .get(cursor + 1..cursor + 1 + hashes)
                    .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
            {
                state.raw_hashes = None;
                cursor += hashes + 1;
            } else {
                cursor += 1;
            }
            continue;
        }
        if state.string {
            match bytes[cursor] {
                b'\\' => cursor = (cursor + 2).min(bytes.len()),
                b'"' => {
                    state.string = false;
                    cursor += 1;
                }
                _ => cursor += 1,
            }
            continue;
        }
        if state.block_comment_depth > 0 {
            if bytes[cursor..].starts_with(b"/*") {
                state.block_comment_depth += 1;
                cursor += 2;
            } else if bytes[cursor..].starts_with(b"*/") {
                state.block_comment_depth -= 1;
                cursor += 2;
            } else {
                cursor += 1;
            }
            continue;
        }
        if bytes[cursor..].starts_with(b"//") {
            break;
        }
        if bytes[cursor..].starts_with(b"/*") {
            state.block_comment_depth = 1;
            cursor += 2;
            continue;
        }
        let raw_prefix = if bytes[cursor] == b'r' {
            Some(1)
        } else if bytes[cursor..].starts_with(b"br") || bytes[cursor..].starts_with(b"cr") {
            Some(2)
        } else {
            None
        };
        if let Some(prefix_len) = raw_prefix {
            let mut quote = cursor + prefix_len;
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if bytes.get(quote) == Some(&b'"') {
                state.raw_hashes = Some(quote - cursor - prefix_len);
                cursor = quote + 1;
                continue;
            }
        }
        match bytes[cursor] {
            b'"' => {
                state.string = true;
                cursor += 1;
            }
            b'\'' => {
                cursor = rust_char_literal_end(bytes, cursor).unwrap_or(cursor + 1);
            }
            b'{' => {
                opens += 1;
                visible[cursor] = bytes[cursor];
                cursor += 1;
            }
            b'}' => {
                closes += 1;
                visible[cursor] = bytes[cursor];
                cursor += 1;
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = cursor;
                cursor += 1;
                while cursor < bytes.len()
                    && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
                {
                    cursor += 1;
                }
                visible[start..cursor].copy_from_slice(&bytes[start..cursor]);
                unsafe_token |= &line[start..cursor] == "unsafe";
            }
            _ => {
                visible[cursor] = bytes[cursor];
                cursor += 1;
            }
        }
    }
    (
        opens,
        closes,
        unsafe_token,
        String::from_utf8_lossy(&visible).into_owned(),
    )
}

fn rust_unsafe_line_mask(lines: &[&str]) -> Vec<bool> {
    let mut lexer = RustScanState::default();
    lines
        .iter()
        .map(|line| rust_line_facts(line, &mut lexer).2)
        .collect()
}

pub(super) fn production_line_mask(lines: &[&str]) -> Vec<bool> {
    let mut lexer = RustScanState::default();
    let facts = lines
        .iter()
        .map(|line| rust_line_facts(line, &mut lexer))
        .collect::<Vec<_>>();
    let mut mask = vec![true; lines.len()];
    let mut pending_cfg = None;
    let mut active = false;
    let mut opened = false;
    let mut depth = 0i32;
    for index in 0..lines.len() {
        let trimmed = facts[index].3.trim_start();
        if active {
            mask[index] = false;
            let (opens, closes) = (facts[index].0, facts[index].1);
            opened |= opens > 0;
            depth += opens - closes;
            if (opened && depth <= 0) || (!opened && trimmed.ends_with(';')) {
                active = false;
            }
            continue;
        }
        let compact = trimmed
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect::<String>();
        let cfg_test = compact.starts_with("#[cfg(test)]");
        if cfg_test {
            pending_cfg = Some(index);
            let inline_module = trimmed
                .split_once(']')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with("mod "));
            if !inline_module {
                continue;
            }
        } else if let Some(start) = pending_cfg {
            if trimmed.is_empty() || trimmed.starts_with("#[") {
                continue;
            }
            if !trimmed.starts_with("mod ") {
                pending_cfg = None;
                continue;
            }
            mask[start..=index].fill(false);
        } else {
            continue;
        }
        let start = pending_cfg.take().unwrap_or(index);
        mask[start..=index].fill(false);
        let (opens, closes) = (facts[index].0, facts[index].1);
        opened = opens > 0;
        depth = opens - closes;
        active = (!opened || depth > 0) && !trimmed.ends_with(';');
    }
    mask
}

/// 품질 게이트 전체 — S0 감지 → S3(포맷·린트·테스트) → S5(내장 스캔 + 의존성 감사).
/// S3·S5 는 어떤 경우에도 생략되지 않는다.
pub async fn run_quality_gate(workspace: &Path, changed: &[String]) -> QualityReport {
    run_quality_gate_with_contract(workspace, changed, false).await
}

pub(crate) async fn run_quality_gate_for_task(
    workspace: &Path,
    changed: &[String],
    browser_game_required: bool,
) -> QualityReport {
    run_quality_gate_with_contract(workspace, changed, browser_game_required).await
}

async fn run_quality_gate_with_contract(
    workspace: &Path,
    changed: &[String],
    browser_game_required: bool,
) -> QualityReport {
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

    let typescript = typescript_validator(workspace);
    for file in changed
        .iter()
        .filter(|file| is_typescript_syntax_source(file))
    {
        match validated_changed_arg(workspace, file) {
            Ok(Some(file)) => {
                if let Some(program) = &typescript {
                    let is_jsx = Path::new(&file)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("jsx"));
                    let mut args = vec![
                        "--pretty".into(),
                        "false".into(),
                        "--noEmit".into(),
                        "--skipLibCheck".into(),
                        "--jsx".into(),
                        "preserve".into(),
                    ];
                    if is_jsx {
                        args.extend(["--allowJs".into(), "--checkJs".into(), "false".into()]);
                    }
                    args.push(file.clone());
                    steps.push(
                        run_argv_step(
                            "S3-syntax",
                            format!("tsc --noEmit {file:?}"),
                            program.clone(),
                            args,
                            workspace,
                        )
                        .await,
                    );
                } else {
                    steps.push(GateStep {
                        stage: "S3-syntax",
                        command: format!("tsc --noEmit {file:?}"),
                        passed: false,
                        exit_code: None,
                        note: Some("필수 TypeScript/JSX 구문 검사기 tsc 미설치".into()),
                    });
                }
            }
            Ok(None) => {}
            Err(error) => steps.push(GateStep {
                stage: "S3-syntax",
                command: format!("tsc --noEmit {file:?}"),
                passed: false,
                exit_code: None,
                note: Some(error),
            }),
        }
    }

    let python = ["python3", "python"]
        .into_iter()
        .find(|program| profile::tool_available(program));
    for file in changed.iter().filter(|file| {
        Path::new(file)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
    }) {
        match validated_changed_arg(workspace, file) {
            Ok(Some(file)) => {
                if let Some(program) = python {
                    steps.push(
                        run_argv_step(
                            "S3-syntax",
                            format!("{program} -m py_compile {file:?}"),
                            program.into(),
                            vec!["-m".into(), "py_compile".into(), file],
                            workspace,
                        )
                        .await,
                    );
                } else {
                    steps.push(GateStep {
                        stage: "S3-syntax",
                        command: format!("python -m py_compile {file:?}"),
                        passed: false,
                        exit_code: None,
                        note: Some("필수 Python 구문 검사기 미설치".into()),
                    });
                }
            }
            Ok(None) => {}
            Err(error) => steps.push(GateStep {
                stage: "S3-syntax",
                command: format!("python -m py_compile {file:?}"),
                passed: false,
                exit_code: None,
                note: Some(error),
            }),
        }
    }

    let gofmt_available = profile::tool_available("gofmt");
    for file in changed.iter().filter(|file| {
        Path::new(file)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("go"))
    }) {
        match validated_changed_arg(workspace, file) {
            Ok(Some(file)) => {
                if gofmt_available {
                    steps.push(
                        run_argv_step(
                            "S3-syntax",
                            format!("gofmt -d {file:?}"),
                            "gofmt".into(),
                            vec!["-d".into(), file],
                            workspace,
                        )
                        .await,
                    );
                } else {
                    steps.push(GateStep {
                        stage: "S3-syntax",
                        command: format!("gofmt -d {file:?}"),
                        passed: false,
                        exit_code: None,
                        note: Some("필수 Go 구문 검사기 gofmt 미설치".into()),
                    });
                }
            }
            Ok(None) => {}
            Err(error) => steps.push(GateStep {
                stage: "S3-syntax",
                command: format!("gofmt -d {file:?}"),
                passed: false,
                exit_code: None,
                note: Some(error),
            }),
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
            let lines = text.lines().collect::<Vec<_>>();
            let production = production_line_mask(&lines);
            let unsafe_lines = rust_unsafe_line_mask(&lines);
            for (line_no, line) in lines.iter().enumerate() {
                if !production[line_no] {
                    continue;
                }
                for (pattern, why) in &profile.forbidden_patterns {
                    let found = if pattern == "unsafe " {
                        unsafe_lines[line_no]
                    } else {
                        code_contains_pattern(line, pattern)
                    };
                    if pattern == "unsafe "
                        && found
                        && has_adjacent_safety_justification(&lines, line_no)
                    {
                        continue;
                    }
                    if found {
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
    match browser::discover_entries_for_task(workspace, changed, browser_game_required) {
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
            let entry_count = entries.len();
            let mut game_contract_targeted = !browser_game_required;
            for entry in entries {
                let html = std::fs::read_to_string(&entry).unwrap_or_default();
                let require_game_contract =
                    browser_game_contract_target(&html, browser_game_required, entry_count);
                game_contract_targeted |= require_game_contract;
                let display_entry = entry
                    .strip_prefix(&display_root)
                    .unwrap_or(&entry)
                    .display()
                    .to_string();
                let command = format!("browser smoke (headless): {display_entry}");
                match browser::smoke_test_in_workspace_with_contract(
                    workspace,
                    &entry,
                    require_game_contract,
                )
                .await
                {
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
            if !game_contract_targeted {
                steps.push(GateStep {
                    stage: "S4-smoke",
                    command: "browser game contract targeting".into(),
                    passed: false,
                    exit_code: None,
                    note: Some("브라우저 게임 작업의 실행 엔트리를 식별하지 못했습니다".into()),
                });
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

const MAX_PUBLIC_REPORT_CHARS: usize = 40_000;

fn public_report_field(value: &str, workspace: &Path, limit: usize) -> String {
    let redacted = redact_tool_output(value, workspace);
    let mut bounded = redacted.chars().take(limit).collect::<String>();
    if redacted.chars().count() > limit {
        bounded.push_str(" …[truncated]");
    }
    bounded
}

/// 리포트 렌더 — 공개 터미널·라이브 싱크에 안전한 사람이 읽는 형태.
pub fn render_report_in_workspace(report: &QualityReport, workspace: &Path) -> String {
    let mut out = format!(
        "=== 품질 게이트 ({}) — {} ===\n",
        report.language.name(),
        if report.passed { "통과" } else { "실패" }
    );
    for s in &report.steps {
        let mark = if s.passed { "✓" } else { "✗" };
        out.push_str(&format!(
            "{mark} [{}] {}\n",
            s.stage,
            public_report_field(&s.command, workspace, 1000)
        ));
        if let Some(note) = &s.note {
            let note = public_report_field(note, workspace, 4000);
            for line in note.lines().take(4) {
                out.push_str(&format!("    │ {line}\n"));
            }
        }
    }
    for f in &report.findings {
        out.push_str(&format!(
            "✗ [S5-{}] {}:{} — {}\n",
            f.kind,
            public_report_field(&f.file, workspace, 1000),
            f.line,
            public_report_field(&f.detail, workspace, 2000)
        ));
    }
    let redacted = redact_tool_output(&out, workspace);
    let mut bounded = redacted
        .chars()
        .take(MAX_PUBLIC_REPORT_CHARS)
        .collect::<String>();
    if redacted.chars().count() > MAX_PUBLIC_REPORT_CHARS {
        bounded.push_str("\n…[report truncated]\n");
    }
    bounded
}

/// 하위 호환 공개 API. 현재 디렉터리를 워크스페이스 경계로 사용한다.
pub fn render_report(report: &QualityReport) -> String {
    let workspace = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    render_report_in_workspace(report, &workspace)
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

fn looks_like_telegram_bot_token(token: &str) -> bool {
    let value = token.trim_matches(|character: char| {
        !character.is_ascii_alphanumeric() && !matches!(character, ':' | '-' | '_')
    });
    let value = value.strip_prefix("bot").unwrap_or(value);
    let Some((id, secret)) = value.split_once(':') else {
        return false;
    };
    (6..=12).contains(&id.len())
        && id.chars().all(|character| character.is_ascii_digit())
        && secret.starts_with("AA")
        && (20..=128).contains(&secret.len())
        && secret
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn looks_like_aws_access_key(token: &str) -> bool {
    let value = token.trim_matches(|character: char| !character.is_ascii_alphanumeric());
    value.len() == 20
        && [
            "AIDA", "AIPA", "AKIA", "ANPA", "ANVA", "AROA", "ASCA", "ASIA",
        ]
        .iter()
        .any(|prefix| value.starts_with(prefix))
        && value
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
}

fn looks_like_standalone_secret(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    lower.starts_with("sk-")
        || lower.starts_with("ghp_")
        || lower.starts_with("github_pat_")
        || lower.starts_with("xoxb-")
        || looks_like_telegram_bot_token(token)
        || looks_like_aws_access_key(token)
        || looks_like_high_entropy_secret(token)
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
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\x1b' => match characters.next() {
                Some('[') => {
                    for next in characters.by_ref() {
                        if next.is_ascii() && ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']' | 'P' | 'X' | '^' | '_') => loop {
                    match characters.next() {
                        Some('\x07' | '\u{009c}') | None => break,
                        Some('\x1b') if characters.peek() == Some(&'\\') => {
                            characters.next();
                            break;
                        }
                        Some(_) => {}
                    }
                },
                Some(_) | None => {}
            },
            '\u{009b}' => {
                for next in characters.by_ref() {
                    if next.is_ascii() && ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            '\u{0090}' | '\u{0098}' | '\u{009d}' | '\u{009e}' | '\u{009f}' => loop {
                match characters.next() {
                    Some('\x07' | '\u{009c}') | None => break,
                    Some('\x1b') if characters.peek() == Some(&'\\') => {
                        characters.next();
                        break;
                    }
                    Some(_) => {}
                }
            },
            '\n' | '\r' | '\t' => output.push(character),
            _ if character.is_control() => {}
            _ => output.push(character),
        }
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
    let suffix_start = token[authority_end..]
        .find(['?', '#'])
        .map(|index| authority_end + index)
        .unwrap_or(token.len());
    let authority = &token[scheme_end..authority_end];
    let mut safe_authority = authority.to_string();
    let mut changed = false;
    if let Some(at) = authority.rfind('@') {
        safe_authority.replace_range(..at, "[redacted]");
        changed = true;
    }
    let safe_path = token[authority_end..suffix_start]
        .split('/')
        .map(|segment| {
            if looks_like_standalone_secret(segment) {
                changed = true;
                "[redacted]"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    if suffix_start < token.len() {
        changed = true;
    }
    changed.then(|| {
        let mut sanitized = format!("{}{}{}", &token[..scheme_end], safe_authority, safe_path);
        if suffix_start < token.len() {
            sanitized.push_str("?[redacted]");
        }
        sanitized
    })
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

fn redaction_path_spellings(path: &Path) -> Vec<String> {
    let mut spellings = vec![path.display().to_string()];
    if let Ok(canonical) = path.canonicalize() {
        spellings.push(canonical.display().to_string());
    }
    for spelling in spellings.clone() {
        for (source_prefix, alias_prefix) in [
            ("/private/tmp", "/tmp"),
            ("/tmp", "/private/tmp"),
            ("/private/var", "/var"),
            ("/var", "/private/var"),
        ] {
            if let Some(rest) = spelling.strip_prefix(source_prefix)
                && (rest.is_empty() || rest.starts_with('/'))
            {
                spellings.push(format!("{alias_prefix}{rest}"));
            }
        }
    }
    spellings.retain(|spelling| spelling.len() > 1);
    spellings.sort();
    spellings.dedup();
    spellings.sort_by_key(|spelling| std::cmp::Reverse(spelling.len()));
    spellings
}

fn redact_path_spellings(mut text: String, path: &Path, replacement: &str) -> String {
    for spelling in redaction_path_spellings(path) {
        text = text.replace(&spelling, replacement);
    }
    text
}

fn normalize_redaction_input(text: &str, workspace: &Path) -> String {
    let cleaned = strip_ansi_sequences(text);
    let mut normalized = redact_path_spellings(cleaned, workspace, "<workspace>");
    if let Some(home) = std::env::var_os("HOME") {
        normalized = redact_path_spellings(normalized, Path::new(&home), "<home>");
    }
    redact_spaced_assignments(&normalized)
}

fn redact_output_token(token: &str, redact_next: &mut bool) -> String {
    if *redact_next {
        return match credential_token(token) {
            Some(CredentialToken::Scheme) => token.to_string(),
            Some(CredentialToken::Secret) | None => {
                *redact_next = false;
                "[redacted]".to_string()
            }
        };
    }
    if let Some(kind) = credential_token(token) {
        return match kind {
            CredentialToken::Scheme => {
                *redact_next = true;
                token.to_string()
            }
            CredentialToken::Secret => "[redacted]".to_string(),
        };
    }
    if let Some(url) = redact_url(token) {
        return url;
    }
    if let Some(at) = token.find(['=', ':'])
        && (looks_like_sensitive_key(&token[..at])
            || (token.as_bytes()[at] == b'=' && looks_like_env_key(&token[..at])))
    {
        *redact_next = at + 1 == token.len();
        return format!("{}[redacted]", &token[..=at]);
    }
    if looks_like_standalone_secret(token) {
        return "[redacted]".to_string();
    }
    token.to_string()
}

pub(crate) fn redact_local_output(text: &str, workspace: &Path) -> String {
    let normalized = normalize_redaction_input(text, workspace);
    let mut redact_next = false;
    normalized
        .split_whitespace()
        .map(|token| redact_output_token(token, &mut redact_next))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn redact_unbounded_output(text: &str, workspace: &Path) -> String {
    let normalized = normalize_redaction_input(text, workspace);
    let mut output = String::with_capacity(normalized.len());
    let mut redact_next = false;
    let mut token_start = None;
    for (index, character) in normalized.char_indices() {
        if character.is_whitespace() {
            if let Some(start) = token_start.take() {
                output.push_str(&redact_output_token(
                    &normalized[start..index],
                    &mut redact_next,
                ));
            }
            output.push(character);
        } else if token_start.is_none() {
            token_start = Some(index);
        }
    }
    if let Some(start) = token_start {
        output.push_str(&redact_output_token(&normalized[start..], &mut redact_next));
    }
    output
}

pub(crate) fn redact_tool_output(text: &str, workspace: &Path) -> String {
    let output = redact_unbounded_output(text, workspace);
    let mut bounded = output.chars().take(40_000).collect::<String>();
    if output.chars().count() > 40_000 {
        bounded.push_str("\n…[truncated]");
    }
    bounded
}

pub(crate) fn redact_bounded_error(text: &str, workspace: &Path) -> String {
    let redacted = redact_local_output(text, workspace);
    let mut bounded = redacted.chars().take(1000).collect::<String>();
    if redacted.chars().count() > 1000 {
        bounded.push_str(" …[truncated]");
    }
    bounded
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

    #[tokio::test]
    async fn mixed_rust_and_javascript_changes_always_run_node_syntax() {
        if !profile::tool_available("node") {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-quality-mixed-syntax-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("src")).expect("temp workspace");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"mixed_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("manifest");
        std::fs::write(root.join("src/lib.rs"), "pub fn ready() -> bool { true }\n")
            .expect("Rust fixture");
        std::fs::write(root.join("app.js"), "const broken = ;\n").expect("JavaScript fixture");

        let report = run_quality_gate(&root, &["src/lib.rs".into(), "app.js".into()]).await;
        let syntax = report
            .steps
            .iter()
            .find(|step| step.stage == "S3-syntax" && step.command.contains("app.js"))
            .expect("mixed JavaScript syntax step");
        assert!(
            !syntax.passed,
            "syntax step unexpectedly passed: {syntax:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn mixed_rust_python_and_go_changes_run_each_syntax_gate() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-quality-mixed-language-syntax-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("src")).expect("temp workspace");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"mixed_language_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("manifest");
        std::fs::write(root.join("src/lib.rs"), "pub fn ready() -> bool { true }\n")
            .expect("Rust fixture");
        std::fs::write(root.join("broken.py"), "def broken(:\n").expect("Python fixture");
        std::fs::write(root.join("broken.go"), "package main\nfunc broken( {\n")
            .expect("Go fixture");

        let report = run_quality_gate(
            &root,
            &["src/lib.rs".into(), "broken.py".into(), "broken.go".into()],
        )
        .await;
        for file in ["broken.py", "broken.go"] {
            let syntax = report
                .steps
                .iter()
                .find(|step| step.stage == "S3-syntax" && step.command.contains(file))
                .unwrap_or_else(|| panic!("missing syntax step for {file}"));
            assert!(!syntax.passed, "invalid {file} unexpectedly passed");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn mixed_rust_typescript_and_jsx_changes_run_each_syntax_gate() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-quality-mixed-typescript-syntax-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("src")).expect("temp workspace");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"mixed_typescript_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("manifest");
        std::fs::write(root.join("src/lib.rs"), "pub fn ready() -> bool { true }\n")
            .expect("Rust fixture");
        std::fs::write(root.join("broken.ts"), "const broken: = 1;\n").expect("TypeScript fixture");
        std::fs::write(root.join("broken.tsx"), "const view = <div>;\n").expect("TSX fixture");
        std::fs::write(root.join("broken.jsx"), "const view = <div>;\n").expect("JSX fixture");

        let report = run_quality_gate(
            &root,
            &[
                "src/lib.rs".into(),
                "broken.ts".into(),
                "broken.tsx".into(),
                "broken.jsx".into(),
            ],
        )
        .await;
        for file in ["broken.ts", "broken.tsx", "broken.jsx"] {
            let syntax = report
                .steps
                .iter()
                .find(|step| step.stage == "S3-syntax" && step.command.contains(file))
                .unwrap_or_else(|| panic!("missing syntax step for {file}"));
            assert!(!syntax.passed, "invalid {file} unexpectedly passed");
        }
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

    #[test]
    fn diff_printing_formatters_fail_on_successful_stdout() {
        assert!(successful_stdout_is_failure(
            "gofmt",
            &["-l".into(), ".".into()],
            b"main.go\n"
        ));
        assert!(successful_stdout_is_failure(
            "/usr/local/bin/gofmt",
            &["-d".into(), "main.go".into()],
            b"diff main.go\n"
        ));
        assert!(successful_stdout_is_failure(
            "shfmt.exe",
            &["-d".into(), ".".into()],
            b"diff script.sh\n"
        ));
        assert!(!successful_stdout_is_failure(
            "gofmt",
            &["-l".into(), ".".into()],
            b""
        ));
        assert!(!successful_stdout_is_failure(
            "cargo",
            &["test".into()],
            b"test output\n"
        ));
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

    #[cfg(unix)]
    #[tokio::test]
    async fn quality_command_redacts_osc_split_sensitive_keys() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-quality-osc-redaction-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("temp workspace");
        let step = run_argv_step_with_timeout(
            "S3-test",
            "OSC redaction fixture".into(),
            "sh".into(),
            vec![
                "-c".into(),
                "printf 'TOK\\033]0;title\\007EN=OSC-MARKER\\n' >&2; exit 1".into(),
            ],
            &root,
            std::time::Duration::from_secs(5),
        )
        .await;

        assert!(!step.passed);
        let note = step.note.expect("redacted failure note");
        assert!(!note.contains("OSC-MARKER"));
        assert!(note.contains("TOKEN=[redacted]"));
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
            "TOK\x1b]0;terminal-title\x07EN=OSC-SPLIT-KEY\n",
            "AUTH\x1b]0;terminal-title\x1b\\ORIZATION=OSC-ST-SPLIT-KEY\n",
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
            "OSC-SPLIT-KEY",
            "OSC-ST-SPLIT-KEY",
        ] {
            assert!(!redacted.contains(marker), "leaked marker: {marker}");
        }
        assert!(redacted.contains("[redacted]"));
        assert!(redacted.contains("postgres://[redacted]@host/db"));
    }

    #[test]
    fn tool_output_redaction_preserves_layout_and_bounds_content() {
        let workspace = Path::new("/tmp/private-workspace");
        let output = concat!(
            "first line\n",
            "  TOKEN=TOOL-SECRET\n",
            "/tmp/private-workspace/src/main.rs\n",
        );
        let redacted = redact_tool_output(output, workspace);
        assert_eq!(redacted.lines().count(), 3);
        assert!(redacted.contains("  TOKEN=[redacted]"));
        assert!(redacted.contains("<workspace>/src/main.rs"));
        assert!(!redacted.contains("TOOL-SECRET"));
        assert!(!redacted.contains("private-workspace"));
    }

    #[test]
    fn local_path_redaction_covers_private_tmp_aliases() {
        let canonical_workspace = Path::new("/private/tmp/rafikx-alias-workspace");
        let alias_output = "/tmp/rafikx-alias-workspace/private/file";
        let redacted = redact_tool_output(alias_output, canonical_workspace);
        assert_eq!(redacted, "<workspace>/private/file");

        let alias_workspace = Path::new("/tmp/rafikx-alias-workspace");
        let canonical_output = "/private/tmp/rafikx-alias-workspace/private/file";
        let redacted = redact_tool_output(canonical_output, alias_workspace);
        assert_eq!(redacted, "<workspace>/private/file");
    }

    #[test]
    fn bare_service_credentials_are_redacted_without_hiding_ratios() {
        let output = concat!(
            "123456789:AAabcdefghijklmnopqrstuvwx\n",
            "AKIA1234567890ABCDEF\n",
            "ratio 123:456 build ABCD1234\n",
        );
        let redacted = redact_tool_output(output, Path::new("/tmp/workspace"));
        assert_eq!(redacted.matches("[redacted]").count(), 2);
        assert!(!redacted.contains("AAabcdefghijklmnopqrstuvwx"));
        assert!(!redacted.contains("AKIA1234567890ABCDEF"));
        assert!(redacted.contains("123:456"));
        assert!(redacted.contains("ABCD1234"));
    }

    #[test]
    fn service_credentials_in_url_paths_are_redacted() {
        let telegram =
            "https://api.telegram.org/bot123456789:AAabcdefghijklmnopqrstuvwx/sendMessage";
        let aws = "https://example.test/build/AKIA1234567890ABCDEF/artifact";
        let openai =
            "https://example.test/build/sk-proj-1234567890abcdefghijklmnopqrstuvwxyz/artifact";
        let github = "https://example.test/build/ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890/artifact";
        let github_pat =
            "https://example.test/build/github_pat_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890/artifact";
        let slack =
            "https://example.test/build/xoxb-1234567890-abcdefghijklmnopqrstuvwxyz/artifact";
        let generic =
            "https://example.test/build/abcdefghijklmnopqrstuvwxyz1234567890ABCDEF/artifact";
        let redacted = redact_tool_output(
            &format!("{telegram}\n{aws}\n{openai}\n{github}\n{github_pat}\n{slack}\n{generic}"),
            Path::new("/tmp/workspace"),
        );
        assert!(!redacted.contains("AAabcdefghijklmnopqrstuvwx"));
        assert!(!redacted.contains("AKIA1234567890ABCDEF"));
        assert!(!redacted.contains("sk-proj-"));
        assert!(!redacted.contains("ghp_"));
        assert!(!redacted.contains("github_pat_"));
        assert!(!redacted.contains("xoxb-"));
        assert!(!redacted.contains("abcdefghijklmnopqrstuvwxyz1234567890ABCDEF"));
        assert_eq!(redacted.matches("[redacted]").count(), 7);
        assert!(redacted.contains("/sendMessage"));
        assert!(redacted.contains("/artifact"));
    }

    #[test]
    fn public_quality_report_redacts_every_field_and_has_a_total_bound() {
        let telegram = "123456789:AAabcdefghijklmnopqrstuvwx";
        let aws = "AKIA1234567890ABCDEF";
        let steps = (0..50)
            .map(|_| GateStep {
                stage: "S3-test",
                command: format!("check https://api.telegram.org/bot{telegram}/run"),
                passed: false,
                exit_code: Some(1),
                note: Some(format!("{aws} {}", "x".repeat(5000))),
            })
            .collect();
        let report = QualityReport {
            language: Language::Unknown,
            steps,
            findings: vec![security::Finding {
                kind: "secret",
                file: format!("src/{telegram}.rs"),
                line: 1,
                detail: aws.into(),
            }],
            passed: false,
        };
        let rendered = render_report_in_workspace(&report, Path::new("/tmp/workspace"));
        assert!(!rendered.contains(telegram));
        assert!(!rendered.contains(aws));
        assert!(rendered.contains("[redacted]"));
        assert!(rendered.chars().count() <= MAX_PUBLIC_REPORT_CHARS + 24);
    }

    #[test]
    fn unsafe_idiom_requires_an_adjacent_safety_justification() {
        let justified = ["// SAFETY: checked ABI", "unsafe { call(); }"];
        let unproved = ["// ordinary note", "unsafe { call(); }"];
        let empty = ["// SAFETY:", "unsafe { call(); }"];
        let disguised = ["// UNSAFETY: trust me", "unsafe { call(); }"];
        assert!(has_adjacent_safety_justification(&justified, 1));
        assert!(!has_adjacent_safety_justification(&unproved, 1));
        assert!(!has_adjacent_safety_justification(&empty, 1));
        assert!(!has_adjacent_safety_justification(&disguised, 1));
        assert!(code_contains_pattern(
            "let value = unsafe { call() };",
            "unsafe "
        ));
        assert!(!code_contains_pattern("let rule = \"unsafe \";", "unsafe "));
        let unsafe_lines = [
            "unsafe/* missing proof */{ call(); }",
            "let text = \"unsafe { not code }\";",
            "/* unsafe { not code } */ fn safe() {}",
            "let raw = r#\"unsafe { not code }\"#;",
            "const PAD: &[u8] = br#\"\\\"#; unsafe { after_raw_bytes(); }",
        ];
        assert_eq!(
            rust_unsafe_line_mask(&unsafe_lines),
            [true, false, false, false, true]
        );
        let lines = [
            "fn before() {}",
            "#[cfg(test)]",
            "mod tests {",
            "const FIXTURE: &str = r#\"}\"#;",
            "}",
            "unsafe { after_tests(); }",
        ];
        assert_eq!(
            production_line_mask(&lines),
            [true, false, false, false, false, true]
        );
        let disguised = [
            "pub fn exploit() {",
            "/*",
            "#[cfg(test)]",
            "mod tests {",
            "*/",
            "let value = 1;",
            "let _ = unsafe/* missing proof */ { std::ptr::read_volatile(&value) };",
            "}",
        ];
        assert!(
            production_line_mask(&disguised)
                .into_iter()
                .all(|line| line)
        );
        assert!(rust_unsafe_line_mask(&disguised)[6]);
    }

    #[test]
    fn clippy_warnings_are_attributed_to_changed_files() {
        let output = concat!(
            "src/old.rs:10:2: warning: old warning\n",
            "src/new.rs:20:4: \x1b[33mwarning\x1b[0m: new warning\n",
        );
        let changed = BTreeSet::from(["src/new.rs".to_string()]);
        let (baseline, blocking) = clippy_warning_summary(output, &changed);
        assert_eq!(baseline, 1);
        assert_eq!(blocking, ["new warning"]);
    }

    #[test]
    fn rust_file_placeholder_excludes_non_rust_changes() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-quality-rust-files-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("src")).expect("workspace");
        std::fs::write(root.join("src/lib.rs"), "pub fn ok() {}\n").expect("Rust fixture");
        std::fs::write(root.join("README.md"), "# fixture\n").expect("Markdown fixture");
        let (_, args) = command_argv(
            "rustfmt --check {rust_files}",
            &root,
            &["src/lib.rs".into(), "README.md".into()],
        )
        .expect("command")
        .expect("Rust file");
        assert_eq!(args, ["--check", "src/lib.rs"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn browser_game_task_targets_only_game_entries_in_multi_entry_workspaces() {
        let decoy = r#"<canvas id="game"></canvas><script src="game.js"></script>"#;
        let actual = r#"<canvas id="board"></canvas><script src="snake-engine.js"></script>"#;
        let dashboard = r#"<main id="dashboard"></main><script src="charts.js"></script>"#;
        assert!(browser_game_contract_target(decoy, true, 3));
        assert!(browser_game_contract_target(actual, true, 3));
        assert!(!browser_game_contract_target(dashboard, true, 3));
        assert!(!browser_game_contract_target(actual, false, 3));
        assert!(browser_game_contract_target(dashboard, true, 1));
    }

    #[test]
    fn shared_css_does_not_force_game_contract_onto_unrelated_entry() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-quality-shared-game-css-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("site.css"), "body { margin: 0; }\n").expect("shared CSS");
        std::fs::write(root.join("game.js"), "requestAnimationFrame(() => {});\n")
            .expect("game source");
        std::fs::write(
            root.join("game.html"),
            r#"<meta name="rafikx-browser-game-contract" content="v1"><link rel="stylesheet" href="site.css"><canvas id="game"></canvas><script src="game.js"></script>"#,
        )
        .expect("game entry");
        std::fs::write(
            root.join("about.html"),
            r#"<link rel="stylesheet" href="site.css"><main>About</main>"#,
        )
        .expect("about entry");

        let entries = browser::discover_entries_for_task(
            &root,
            &["game.html".into(), "game.js".into(), "site.css".into()],
            true,
        )
        .expect("entry discovery");
        assert_eq!(entries.len(), 2);
        let targeted = entries
            .iter()
            .map(|entry| {
                let html = std::fs::read_to_string(entry).expect("entry source");
                (
                    entry.file_name().and_then(|name| name.to_str()).unwrap(),
                    browser_game_contract_target(&html, true, entries.len()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(targeted.get("game.html"), Some(&true));
        assert_eq!(targeted.get("about.html"), Some(&false));
        let _ = std::fs::remove_dir_all(root);
    }
}
