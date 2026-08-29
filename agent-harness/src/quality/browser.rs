//! 브라우저 스모크 게이트 (S4 보강) — HTML/JS 산출물의 런타임 오류를 실제 브라우저로 잡는다.
//! 기원: 2026-08-29 사용자 실측 — "슈퍼마리오 게임이 실행도 안 되는" 결과물.
//! game.js 의 잔재 변수(camTarget)가 첫 프레임에서 ReferenceError 를 냈지만,
//! node --check(구문만)·eslint(미설치)·내장 보안 스캐너(보안 전용) 어디에도 걸리지 않았다.
//! "사용자가 결함을 발견하는 순간 = 게이트 설계의 실패" 원칙에 따라 추가된 게이트다.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const ERROR_MARKER: &str = "__RAFIKX_BROWSER_ERROR__";
const READY_MARKER: &str = "__RAFIKX_BROWSER_READY__";
const PROBE_PATH: &str = "/__rafikx_probe.js";
const MAX_STAGED_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STAGED_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_STAGED_ENTRIES: usize = 25_000;
const MAX_STAGING_DURATION: Duration = Duration::from_secs(3);
const MAX_BROWSER_STDERR_BYTES: usize = 256 * 1024;
const MAX_BROWSER_ERRORS: usize = 64;
const MAX_DISCOVERY_ENTRIES: usize = 25_000;
const MAX_BROWSER_ENTRIES: usize = 8;
const MAX_DISCOVERY_DURATION: Duration = Duration::from_secs(3);
const MAX_DISCOVERY_HTML_BYTES: u64 = 256 * 1024;
const MAX_DISCOVERY_TOTAL_HTML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REFERENCE_GRAPH_ENTRIES: usize = 512;
const MAX_REFERENCE_GRAPH_BYTES: u64 = 16 * 1024 * 1024;
const SECURITY_HEADERS: &str = "Content-Security-Policy: default-src 'self'; base-uri 'none'; connect-src 'self'; font-src 'self' data:; form-action 'none'; frame-ancestors 'none'; frame-src 'none'; img-src 'self' data: blob:; media-src 'self' blob:; object-src 'none'; sandbox allow-same-origin allow-scripts; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; worker-src 'self' blob:\r\nX-Content-Type-Options: nosniff\r\nX-DNS-Prefetch-Control: off\r\nReferrer-Policy: no-referrer\r\n";
const PROBE_SCRIPT: &str = r#"(() => {
  const emit = (kind, value) => console.log('__RAFIKX_BROWSER_ERROR__' + kind + ': ' + String(value));
  const originalError = console.error.bind(console);
  console.error = (...args) => {
    emit('console', args.map(value => String(value)).join(' '));
    originalError(...args);
  };
  window.addEventListener('error', event => {
    if (event.target === window) {
      emit('runtime', event.message || 'unknown runtime error');
    } else {
      emit('resource', event.target?.src || event.target?.href || event.target?.tagName || 'unknown resource');
    }
  }, true);
  window.addEventListener('unhandledrejection', event => emit('promise', event.reason || 'unhandled rejection'));
  window.addEventListener('load', () => setTimeout(() => console.log('__RAFIKX_BROWSER_READY__'), 0), { once: true });
})();"#;

/// 콘솔 로그(stderr)에서 런타임 오류를 추출한다 — 순수 함수(테스트 가능).
pub fn parse_console_errors(stderr: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        if out.len() >= MAX_BROWSER_ERRORS {
            break;
        }
        if let Some(marker) = line.find(ERROR_MARKER) {
            let reason: String = line[marker + ERROR_MARKER.len()..]
                .trim_matches([' ', '"', ','])
                .chars()
                .take(300)
                .collect();
            if !reason.is_empty() && !out.contains(&reason) {
                out.push(reason);
            }
            continue;
        }

        let lower = line.to_ascii_lowercase();
        let web_load_error = lower.contains("blocked by cors policy")
            || lower.contains("failed to load resource")
            || lower.contains("failed to load module script")
            || lower.contains("not allowed to load local resource")
            || lower.contains("refused to execute script")
            || lower.contains("net::err_");
        let console_error = line.contains("CONSOLE")
            && (lower.contains("uncaught")
                || lower.contains("syntaxerror")
                || lower.contains("referenceerror")
                || lower.contains("typeerror")
                || lower.contains("is not defined")
                || lower.contains("is not a function"));
        if !web_load_error && !console_error {
            continue;
        }
        let detail = if console_error {
            line.find(']')
                .map(|index| line[index + 1..].trim())
                .unwrap_or(line)
        } else {
            line
        };
        let reason: String = detail.chars().take(300).collect();
        if !out.contains(&reason) {
            out.push(reason);
        }
    }
    out
}

/// 설치된 브라우저 바이너리를 찾는다 — 없으면 None.
pub fn detect_browser() -> Option<&'static str> {
    const CANDIDATES: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium-browser",
        "/usr/bin/chromium",
    ];
    CANDIDATES
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .copied()
}

fn network_isolation_flags(address: std::net::SocketAddr) -> [String; 4] {
    [
        format!(
            "--proxy-bypass-list=<-loopback>;http://127.0.0.1:{}",
            address.port()
        ),
        "--proxy-server=http://127.0.0.1:9".into(),
        "--host-resolver-rules=MAP * 0.0.0.0, EXCLUDE 127.0.0.1".into(),
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
    ]
}

fn evaluate_browser_output(
    success: bool,
    code: Option<i32>,
    stderr: &str,
    stderr_overflow: bool,
    server_errors: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut errors = parse_console_errors(stderr);
    for error in server_errors {
        if !errors.contains(error) {
            errors.push(error.clone());
        }
    }
    if stderr_overflow {
        anyhow::bail!("브라우저 stderr가 수집 상한을 초과했습니다");
    }
    if !success {
        let detail = errors
            .first()
            .map(|error| format!(": {error}"))
            .unwrap_or_default();
        anyhow::bail!(
            "브라우저가 종료 코드 {}로 실패했습니다{detail}",
            code.map_or_else(|| "signal".into(), |code| code.to_string())
        );
    }
    if !stderr.contains(READY_MARKER) {
        anyhow::bail!("브라우저 준비 프로브가 실행되지 않았습니다");
    }
    Ok(errors)
}

fn inject_probe(html: &str) -> String {
    let tag = format!(r#"<script src="{PROBE_PATH}"></script>"#);
    let lower = html.to_ascii_lowercase();
    if let Some(head) = lower.find("<head")
        && let Some(close) = html[head..].find('>')
    {
        let at = head + close + 1;
        let mut injected = String::with_capacity(html.len() + tag.len());
        injected.push_str(&html[..at]);
        injected.push_str(&tag);
        injected.push_str(&html[at..]);
        return injected;
    }
    format!("{tag}{html}")
}

fn percent_encode_path(path: &Path) -> String {
    let mut encoded = String::new();
    for (index, component) in path.components().enumerate() {
        let Component::Normal(part) = component else {
            continue;
        };
        if index > 0 {
            encoded.push('/');
        }
        for byte in part.to_string_lossy().as_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(*byte));
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    encoded
}

fn percent_decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "htm" | "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "cjs" | "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn is_web_asset(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "avif"
            | "cjs"
            | "css"
            | "gif"
            | "htm"
            | "html"
            | "ico"
            | "jpeg"
            | "jpg"
            | "js"
            | "m4a"
            | "mjs"
            | "mp3"
            | "mp4"
            | "ogg"
            | "otf"
            | "png"
            | "svg"
            | "ttf"
            | "wasm"
            | "wav"
            | "webm"
            | "webp"
            | "woff"
            | "woff2"
    )
}

fn has_hidden_component(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            Component::Normal(part) if part.to_string_lossy().starts_with('.')
        )
    })
}

fn has_sensitive_component(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(part) = component else {
            return false;
        };
        let name = part.to_string_lossy().to_ascii_lowercase();
        let stem = Path::new(&*name)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(&name);
        let normalized_stem = stem.replace('-', "_");
        normalized_stem.contains("credential")
            || normalized_stem.contains("secret")
            || matches!(
                normalized_stem.as_str(),
                "id_ed25519" | "id_rsa" | "private_key"
            )
            || matches!(
                Path::new(&*name)
                    .extension()
                    .and_then(|value| value.to_str()),
                Some("key" | "pem")
            )
    })
}

fn is_discovery_excluded_name(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_string_lossy().as_ref(),
        "Pods" | "__pycache__" | "node_modules" | "target" | "vendor"
    )
}

fn is_changed_web_source(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "cjs" | "css" | "htm" | "html" | "js" | "jsx" | "mjs" | "ts" | "tsx"
    )
}

fn insert_browser_entry(
    entries: &mut BTreeSet<PathBuf>,
    root: &Path,
    candidate: &Path,
) -> anyhow::Result<()> {
    if !candidate.exists() {
        return Ok(());
    }
    let candidate = candidate.canonicalize()?;
    if !candidate.starts_with(root) || !candidate.is_file() {
        anyhow::bail!("브라우저 엔트리가 워크스페이스 밖에 있습니다");
    }
    entries.insert(candidate);
    if entries.len() > MAX_BROWSER_ENTRIES {
        anyhow::bail!("브라우저 엔트리 수가 검증 상한을 넘었습니다");
    }
    Ok(())
}

fn project_root_for(root: &Path, path: &Path) -> Option<PathBuf> {
    let mut directory = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
    };
    while directory.starts_with(root) {
        if directory.join("package.json").is_file() {
            return Some(directory);
        }
        if directory == root || !directory.pop() {
            break;
        }
    }
    None
}

fn resolve_html_reference(root: &Path, entry: &Path, raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.starts_with('#')
        || raw.starts_with("//")
        || raw.contains("://")
        || raw.to_ascii_lowercase().starts_with("data:")
        || raw.to_ascii_lowercase().starts_with("javascript:")
    {
        return None;
    }
    let raw = raw.split(['?', '#']).next().unwrap_or_default();
    let mut relative = if raw.starts_with('/') {
        PathBuf::new()
    } else {
        entry.parent()?.strip_prefix(root).ok()?.to_path_buf()
    };
    for component in Path::new(raw.trim_start_matches('/')).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !relative.pop() {
                    return None;
                }
            }
            Component::Normal(part) => relative.push(part),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(root.join(relative))
}

#[derive(Clone)]
enum ReferenceToken {
    Identifier(String),
    Punctuation(u8),
    ControlClose,
    Value,
}

fn push_reference_token(tokens: &mut Vec<ReferenceToken>, token: ReferenceToken) {
    if tokens.len() == 3 {
        tokens.remove(0);
    }
    tokens.push(token);
}

fn quoted_value(text: &str, start: usize, quote: u8) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut cursor = start;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            cursor = (cursor + 2).min(bytes.len());
        } else if bytes[cursor] == quote {
            return Some((text[start..cursor].to_string(), cursor + 1));
        } else {
            cursor += 1;
        }
    }
    None
}

fn reference_literal_context(tokens: &[ReferenceToken], css: bool) -> bool {
    let last_identifier = |offset: usize| {
        tokens
            .len()
            .checked_sub(offset + 1)
            .and_then(|index| match &tokens[index] {
                ReferenceToken::Identifier(value) => Some(value.as_str()),
                _ => None,
            })
    };
    if matches!(last_identifier(0), Some("import" | "from")) {
        return true;
    }
    let called = matches!(tokens.last(), Some(ReferenceToken::Punctuation(b'(')))
        .then(|| last_identifier(1))
        .flatten();
    if css {
        called == Some("url")
    } else {
        matches!(
            called,
            Some("import" | "require" | "url" | "worker" | "sharedworker")
        )
    }
}

fn regex_literal_can_start(tokens: &[ReferenceToken]) -> bool {
    match tokens.last() {
        None => true,
        Some(ReferenceToken::Punctuation(value)) => {
            matches!(
                *value,
                b'(' | b'['
                    | b'{'
                    | b'='
                    | b':'
                    | b','
                    | b';'
                    | b'!'
                    | b'?'
                    | b'+'
                    | b'-'
                    | b'*'
                    | b'%'
                    | b'&'
                    | b'|'
                    | b'^'
                    | b'~'
                    | b'<'
                    | b'>'
            )
        }
        Some(ReferenceToken::Identifier(value)) => {
            matches!(
                value.as_str(),
                "await"
                    | "case"
                    | "delete"
                    | "do"
                    | "else"
                    | "in"
                    | "instanceof"
                    | "new"
                    | "of"
                    | "return"
                    | "throw"
                    | "typeof"
                    | "void"
                    | "yield"
            )
        }
        Some(ReferenceToken::ControlClose) => true,
        Some(ReferenceToken::Value) => false,
    }
}

fn skip_regex_literal(bytes: &[u8], start: usize) -> usize {
    let mut cursor = start + 1;
    let mut character_class = false;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            b'[' => {
                character_class = true;
                cursor += 1;
            }
            b']' => {
                character_class = false;
                cursor += 1;
            }
            b'/' if !character_class => {
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
                    cursor += 1;
                }
                return cursor;
            }
            b'\n' | b'\r' => return start + 1,
            _ => cursor += 1,
        }
    }
    start + 1
}

fn code_reference_values(text: &str, css: bool) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut values = Vec::new();
    let mut tokens = Vec::new();
    let mut control_parentheses = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            cursor += 2;
            while cursor + 1 < bytes.len() && !(bytes[cursor] == b'*' && bytes[cursor + 1] == b'/')
            {
                cursor += 1;
            }
            cursor = (cursor + 2).min(bytes.len());
            continue;
        }
        if !css && bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'/') {
            cursor += 2;
            while cursor < bytes.len() && !matches!(bytes[cursor], b'\n' | b'\r') {
                cursor += 1;
            }
            continue;
        }
        if !css && bytes[cursor] == b'/' && regex_literal_can_start(&tokens) {
            cursor = skip_regex_literal(bytes, cursor);
            push_reference_token(&mut tokens, ReferenceToken::Value);
            continue;
        }
        if matches!(bytes[cursor], b'\'' | b'"' | b'`') {
            let quote = bytes[cursor];
            if let Some((value, next)) = quoted_value(text, cursor + 1, quote) {
                if reference_literal_context(&tokens, css) {
                    values.push(value);
                }
                cursor = next;
                push_reference_token(&mut tokens, ReferenceToken::Value);
                continue;
            }
        }
        if bytes[cursor].is_ascii_alphabetic() || matches!(bytes[cursor], b'_' | b'$') {
            let start = cursor;
            cursor += 1;
            while cursor < bytes.len()
                && (bytes[cursor].is_ascii_alphanumeric()
                    || matches!(bytes[cursor], b'_' | b'$' | b'-'))
            {
                cursor += 1;
            }
            push_reference_token(
                &mut tokens,
                ReferenceToken::Identifier(text[start..cursor].to_ascii_lowercase()),
            );
            continue;
        }
        let token = match bytes[cursor] {
            b'(' => {
                let control = matches!(
                    tokens.last(),
                    Some(ReferenceToken::Identifier(value))
                        if matches!(value.as_str(), "catch" | "for" | "if" | "switch" | "while" | "with")
                );
                control_parentheses.push(control);
                ReferenceToken::Punctuation(b'(')
            }
            b')' if control_parentheses.pop().unwrap_or(false) => ReferenceToken::ControlClose,
            punctuation => ReferenceToken::Punctuation(punctuation),
        };
        push_reference_token(&mut tokens, token);
        cursor += 1;
    }
    values
}

fn html_tag_reference_values(tag: &str) -> Vec<String> {
    let bytes = tag.as_bytes();
    let mut values = Vec::new();
    let mut cursor = 1usize;
    while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() && bytes[cursor] != b'>' {
        cursor += 1;
    }
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || matches!(bytes[cursor], b'-' | b'_'))
        {
            cursor += 1;
        }
        if name_start == cursor {
            cursor += 1;
            continue;
        }
        let name = &tag[name_start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let value = if let Some(quote @ (b'\'' | b'"')) = bytes.get(cursor).copied() {
            quoted_value(tag, cursor + 1, quote).map(|(value, next)| {
                cursor = next;
                value
            })
        } else {
            let start = cursor;
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && bytes[cursor] != b'>'
            {
                cursor += 1;
            }
            (start < cursor).then(|| tag[start..cursor].to_string())
        };
        if matches!(name.to_ascii_lowercase().as_str(), "src" | "href")
            && let Some(value) = value
        {
            values.push(value);
        }
    }
    values
}

fn html_reference_values(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let lower = text.to_ascii_lowercase();
    let mut values = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"<!--") {
            cursor = lower[cursor + 4..]
                .find("-->")
                .map(|offset| cursor + 4 + offset + 3)
                .unwrap_or(bytes.len());
            continue;
        }
        if bytes[cursor] != b'<' {
            cursor += 1;
            continue;
        }
        let mut end = cursor + 1;
        let mut quote = None;
        while end < bytes.len() {
            if let Some(active) = quote {
                if bytes[end] == b'\\' {
                    end = (end + 2).min(bytes.len());
                    continue;
                }
                if bytes[end] == active {
                    quote = None;
                }
            } else if matches!(bytes[end], b'\'' | b'"') {
                quote = Some(bytes[end]);
            } else if bytes[end] == b'>' {
                break;
            }
            end += 1;
        }
        if end >= bytes.len() {
            break;
        }
        let tag = &text[cursor..=end];
        values.extend(html_tag_reference_values(tag));
        let tag_name = tag[1..]
            .trim_start_matches('/')
            .split(|character: char| character.is_ascii_whitespace() || character == '>')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(tag_name.as_str(), "script" | "style") && !tag.starts_with("</") {
            let closing = format!("</{tag_name}");
            if let Some(offset) = lower[end + 1..].find(&closing) {
                let content_end = end + 1 + offset;
                values.extend(code_reference_values(
                    &text[end + 1..content_end],
                    tag_name == "style",
                ));
                cursor = content_end;
                continue;
            }
        }
        cursor = end + 1;
    }
    values
}

fn local_references(root: &Path, source: &Path, text: &str) -> BTreeSet<PathBuf> {
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let values = match extension.as_str() {
        "htm" | "html" => html_reference_values(text),
        "css" => code_reference_values(text, true),
        _ => code_reference_values(text, false),
    };
    values
        .into_iter()
        .filter_map(|value| resolve_html_reference(root, source, &value))
        .collect()
}

fn is_reference_graph_source(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "cjs" | "css" | "htm" | "html" | "js" | "jsx" | "mjs" | "ts" | "tsx"
    )
}

fn entry_reaches_changed_sources(
    root: &Path,
    entry: &Path,
    changed_sources: &[PathBuf],
    started: Instant,
) -> anyhow::Result<Vec<PathBuf>> {
    let targets = changed_sources
        .iter()
        .map(|source| (root.join(source), source))
        .collect::<Vec<_>>();
    let mut matched = BTreeSet::new();
    let mut pending = vec![entry.to_path_buf()];
    let mut visited = BTreeSet::new();
    let mut inspected_bytes = 0u64;

    while let Some(source) = pending.pop() {
        if started.elapsed() > MAX_DISCOVERY_DURATION {
            anyhow::bail!("브라우저 참조 그래프 탐색 시간 상한을 넘었습니다");
        }
        if !visited.insert(source.clone()) {
            continue;
        }
        if visited.len() > MAX_REFERENCE_GRAPH_ENTRIES {
            anyhow::bail!("브라우저 참조 그래프 항목 수가 상한을 넘었습니다");
        }
        if !source.starts_with(root) {
            anyhow::bail!("브라우저 참조 그래프가 워크스페이스 밖을 가리킵니다");
        }
        let metadata = match std::fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) | Err(_) => continue,
        };
        if metadata.len() > MAX_DISCOVERY_HTML_BYTES {
            anyhow::bail!("브라우저 참조 그래프 파일이 상한을 넘었습니다");
        }
        inspected_bytes = inspected_bytes.saturating_add(metadata.len());
        if inspected_bytes > MAX_REFERENCE_GRAPH_BYTES {
            anyhow::bail!("브라우저 참조 그래프 합계가 상한을 넘었습니다");
        }
        let text = std::fs::read_to_string(&source).map_err(|error| {
            anyhow::anyhow!("브라우저 참조 그래프 파일을 읽을 수 없습니다: {error}")
        })?;
        for reference in local_references(root, &source, &text) {
            let reference = reference.canonicalize().unwrap_or(reference);
            if !reference.starts_with(root) {
                anyhow::bail!("브라우저 참조 그래프가 워크스페이스 밖을 가리킵니다");
            }
            for (absolute, changed) in &targets {
                if reference == *absolute {
                    matched.insert((*changed).clone());
                }
            }
            if reference.starts_with(root) && is_reference_graph_source(&reference) {
                pending.push(reference);
            }
        }
    }
    Ok(matched.into_iter().collect())
}

pub(crate) fn discover_entries(
    workspace: &Path,
    changed: &[String],
) -> anyhow::Result<Vec<PathBuf>> {
    let root = workspace.canonicalize()?;
    let started = Instant::now();
    let mut entries = BTreeSet::new();
    let mut changed_sources = Vec::new();
    let mut covered_sources = BTreeSet::new();
    for file in changed {
        let relative = Path::new(file);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            anyhow::bail!("워크스페이스 밖 변경 경로: {file}");
        }
        if !is_changed_web_source(relative) {
            continue;
        }
        let normalized_relative = root
            .join(relative)
            .canonicalize()
            .ok()
            .and_then(|path| path.strip_prefix(&root).ok().map(Path::to_path_buf))
            .unwrap_or_else(|| relative.to_path_buf());
        changed_sources.push(normalized_relative.clone());
        if matches!(
            relative
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "htm" | "html"
        ) {
            insert_browser_entry(&mut entries, &root, &root.join(&normalized_relative))?;
            covered_sources.insert(normalized_relative);
        }
    }
    if changed_sources.is_empty() {
        return Ok(entries.into_iter().collect());
    }

    for source in &changed_sources {
        if covered_sources.contains(source) {
            continue;
        }
        let joined = root.join(source);
        let Some(parent) = joined.parent() else {
            continue;
        };
        let Ok(mut directory) = parent.canonicalize() else {
            continue;
        };
        'ancestor: while directory.starts_with(&root) {
            for candidate in [
                directory.join("index.html"),
                directory.join("public/index.html"),
                directory.join("www/index.html"),
            ] {
                if candidate.is_file()
                    && !entry_reaches_changed_sources(
                        &root,
                        &candidate,
                        std::slice::from_ref(source),
                        started,
                    )?
                    .is_empty()
                {
                    insert_browser_entry(&mut entries, &root, &candidate)?;
                    covered_sources.insert(source.clone());
                    break 'ancestor;
                }
            }
            if directory == root || !directory.pop() {
                break;
            }
        }
    }

    for source in &changed_sources {
        if covered_sources.contains(source) {
            continue;
        }
        let Some(project_root) = project_root_for(&root, &root.join(source)) else {
            continue;
        };
        for candidate in [
            project_root.join("index.html"),
            project_root.join("public/index.html"),
            project_root.join("src/index.html"),
            project_root.join("www/index.html"),
            project_root.join("dist/index.html"),
            project_root.join("build/index.html"),
        ] {
            if candidate.is_file()
                && !entry_reaches_changed_sources(
                    &root,
                    &candidate,
                    std::slice::from_ref(source),
                    started,
                )?
                .is_empty()
            {
                insert_browser_entry(&mut entries, &root, &candidate)?;
                covered_sources.insert(source.clone());
                break;
            }
        }
    }
    if changed_sources
        .iter()
        .all(|source| covered_sources.contains(source))
    {
        return Ok(entries.into_iter().collect());
    }

    let filter_root = root.clone();
    let walker = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(false)
        .git_exclude(false)
        .filter_entry(move |item| {
            if item.depth() == 0 {
                return true;
            }
            let relative = item
                .path()
                .strip_prefix(&filter_root)
                .unwrap_or(item.path());
            !has_hidden_component(relative)
                && !has_sensitive_component(relative)
                && !is_discovery_excluded_name(item.file_name())
        })
        .build();
    let mut inspected_html_bytes = 0u64;
    for (index, item) in walker.enumerate() {
        if index >= MAX_DISCOVERY_ENTRIES {
            anyhow::bail!("브라우저 엔트리 탐색 항목 수가 상한을 넘었습니다");
        }
        if started.elapsed() > MAX_DISCOVERY_DURATION {
            anyhow::bail!("브라우저 엔트리 탐색 시간 상한을 넘었습니다");
        }
        let item = item.map_err(|error| anyhow::anyhow!("브라우저 엔트리 탐색 실패: {error}"))?;
        if item.file_type().is_some_and(|kind| kind.is_file())
            && matches!(
                item.path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str(),
                "htm" | "html"
            )
        {
            let metadata = item
                .metadata()
                .map_err(|error| anyhow::anyhow!("브라우저 엔트리 상태 확인 실패: {error}"))?;
            if metadata.len() <= MAX_DISCOVERY_HTML_BYTES
                && inspected_html_bytes.saturating_add(metadata.len())
                    <= MAX_DISCOVERY_TOTAL_HTML_BYTES
            {
                inspected_html_bytes = inspected_html_bytes.saturating_add(metadata.len());
                let referenced =
                    entry_reaches_changed_sources(&root, item.path(), &changed_sources, started)?;
                if !referenced.is_empty() {
                    insert_browser_entry(&mut entries, &root, item.path())?;
                    covered_sources.extend(referenced);
                }
            }
        }
    }
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let uncovered = changed_sources
        .iter()
        .filter(|source| !covered_sources.contains(*source))
        .take(8)
        .map(|source| source.display().to_string())
        .collect::<Vec<_>>();
    if !uncovered.is_empty() {
        anyhow::bail!(
            "변경된 웹 소스를 실행할 HTML 엔트리를 찾지 못했습니다: {}",
            uncovered.join(", ")
        );
    }
    Ok(entries.into_iter().collect())
}

fn safe_extra_browser_flag(flag: &str) -> bool {
    let flag = flag.to_ascii_lowercase();
    flag.starts_with('-')
        && ![
            "allow-file-access-from-files",
            "allow-running-insecure-content",
            "disable-web-security",
            "host-resolver-rules",
            "host-rules",
            "proxy",
            "remote-debugging",
            "user-data-dir",
        ]
        .iter()
        .any(|blocked| flag.contains(blocked))
}

fn stage_web_root(workspace: &Path, entry_html: &Path, stage: &Path) -> anyhow::Result<PathBuf> {
    stage_web_root_with_limits(
        workspace,
        entry_html,
        stage,
        MAX_STAGED_ENTRIES,
        MAX_STAGING_DURATION,
    )
}

fn stage_web_root_with_limits(
    workspace: &Path,
    entry_html: &Path,
    stage: &Path,
    max_entries: usize,
    max_duration: Duration,
) -> anyhow::Result<PathBuf> {
    let workspace = workspace.canonicalize()?;
    let entry = entry_html.canonicalize()?;
    if !entry.starts_with(&workspace) || !entry.is_file() {
        anyhow::bail!("브라우저 엔트리가 워크스페이스 밖에 있습니다");
    }
    let source_root = project_root_for(&workspace, &entry).unwrap_or_else(|| workspace.clone());
    std::fs::create_dir_all(stage)?;
    let mut staged_bytes = 0u64;
    let mut entries = 0usize;
    let started = Instant::now();
    let filter_root = source_root.clone();
    let walker = ignore::WalkBuilder::new(&source_root)
        .hidden(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(false)
        .git_exclude(false)
        .filter_entry(move |item| {
            if item.depth() == 0 {
                return true;
            }
            let relative = item
                .path()
                .strip_prefix(&filter_root)
                .unwrap_or(item.path());
            !has_hidden_component(relative)
                && !has_sensitive_component(relative)
                && !is_discovery_excluded_name(item.file_name())
        })
        .build();
    for item in walker {
        if started.elapsed() > max_duration {
            anyhow::bail!("브라우저 자산 스테이징 시간 상한을 넘었습니다");
        }
        entries = entries.saturating_add(1);
        if entries > max_entries {
            anyhow::bail!("브라우저 자산 항목 수가 상한을 넘었습니다");
        }
        let item = item.map_err(|error| anyhow::anyhow!("브라우저 자산 순회 실패: {error}"))?;
        if !item.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let source = item.into_path();
        if !is_web_asset(&source) {
            continue;
        }
        let metadata = source.metadata()?;
        if metadata.len() > MAX_STAGED_FILE_BYTES {
            anyhow::bail!(
                "브라우저 자산이 파일 상한을 넘었습니다: {}",
                source.display()
            );
        }
        staged_bytes = staged_bytes.saturating_add(metadata.len());
        if staged_bytes > MAX_STAGED_TOTAL_BYTES {
            anyhow::bail!("브라우저 자산 합계가 상한을 넘었습니다");
        }
        let relative = source.strip_prefix(&source_root)?;
        let destination = stage.join(relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&source, destination)?;
        if started.elapsed() > max_duration {
            anyhow::bail!("브라우저 자산 스테이징 시간 상한을 넘었습니다");
        }
    }
    let relative_entry = entry.strip_prefix(&source_root)?;
    let staged_entry = stage.join(relative_entry);
    if !staged_entry.is_file() {
        anyhow::bail!("브라우저 엔트리를 격리 스테이징하지 못했습니다");
    }
    Ok(staged_entry)
}

fn append_bounded_stderr(log: &mut String, chunk: &str) -> bool {
    let remaining = MAX_BROWSER_STDERR_BYTES.saturating_sub(log.len());
    if remaining == 0 {
        return !chunk.is_empty();
    }
    let overflow = chunk.len() > remaining;
    let mut end = remaining.min(chunk.len());
    while end > 0 && !chunk.is_char_boundary(end) {
        end -= 1;
    }
    log.push_str(&chunk[..end]);
    overflow
}

struct SmokeResources {
    server: Option<tokio::task::JoinHandle<()>>,
    reader: Option<tokio::task::JoinHandle<()>>,
    run_dir: PathBuf,
}

impl SmokeResources {
    fn new(run_dir: PathBuf) -> Self {
        Self {
            server: None,
            reader: None,
            run_dir,
        }
    }
}

impl Drop for SmokeResources {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
        }
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        let _ = std::fs::remove_dir_all(&self.run_dir);
    }
}

async fn await_stderr_reader(resources: &mut SmokeResources) -> anyhow::Result<()> {
    let Some(mut reader) = resources.reader.take() else {
        return Ok(());
    };
    match tokio::time::timeout(Duration::from_secs(2), &mut reader).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => anyhow::bail!("브라우저 stderr 수집 작업 실패: {error}"),
        Err(_) => {
            reader.abort();
            let _ = reader.await;
            anyhow::bail!("브라우저 stderr 수집 종료 시간 초과")
        }
    }
}

async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{SECURITY_HEADERS}Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await
}

async fn serve_request(
    mut stream: TcpStream,
    root: PathBuf,
    entry: PathBuf,
    errors: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let mut request = vec![0u8; 16 * 1024];
    let read = stream.read(&mut request).await?;
    let request = String::from_utf8_lossy(&request[..read]);
    let Some(raw_path) = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .map(|path| path.split('?').next().unwrap_or(path))
    else {
        return write_response(&mut stream, "400 Bad Request", "text/plain", b"bad request").await;
    };
    if raw_path == PROBE_PATH {
        return write_response(
            &mut stream,
            "200 OK",
            "text/javascript; charset=utf-8",
            PROBE_SCRIPT.as_bytes(),
        )
        .await;
    }
    if raw_path == "/favicon.ico" {
        return write_response(&mut stream, "204 No Content", "image/x-icon", b"").await;
    }

    let decoded = percent_decode_path(raw_path.trim_start_matches('/'));
    let Some(decoded) = decoded else {
        return write_response(&mut stream, "400 Bad Request", "text/plain", b"bad path").await;
    };
    let relative = Path::new(&decoded);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return write_response(&mut stream, "403 Forbidden", "text/plain", b"forbidden").await;
    }
    let requested = root.join(relative);
    let resolved = match requested.canonicalize() {
        Ok(path) if path.starts_with(&root) && path.is_file() => path,
        _ => {
            if let Ok(mut errors) = errors.lock() {
                if errors.len() < MAX_BROWSER_ERRORS {
                    errors.push(format!("HTTP 404: /{decoded}"));
                }
            }
            return write_response(&mut stream, "404 Not Found", "text/plain", b"not found").await;
        }
    };
    let mut body = tokio::fs::read(&resolved).await?;
    if resolved == entry {
        let html = String::from_utf8_lossy(&body);
        body = inject_probe(&html).into_bytes();
    }
    write_response(&mut stream, "200 OK", content_type(&resolved), &body).await
}

async fn start_server(
    workspace: &Path,
    entry_html: &Path,
) -> anyhow::Result<(
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<String>>>,
    PathBuf,
)> {
    let root = workspace.canonicalize()?;
    let entry = entry_html.canonicalize()?;
    if !entry.starts_with(&root) {
        anyhow::bail!("브라우저 엔트리가 워크스페이스 밖에 있습니다");
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let errors = Arc::new(Mutex::new(Vec::new()));
    let server_errors = errors.clone();
    let server_root = root.clone();
    let server_entry = entry.clone();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let root = server_root.clone();
            let entry = server_entry.clone();
            let errors = server_errors.clone();
            tokio::spawn(async move {
                let _ = serve_request(stream, root, entry, errors).await;
            });
        }
    });
    Ok((address, task, errors, root))
}

/// 엔트리 HTML 을 실제 브라우저로 로드해 콘솔 오류를 수집한다.
/// 브라우저가 없으면 Ok(None) — 호출자가 HTML 런타임 검증 불가로 처리한다.
pub async fn smoke_test(entry_html: &std::path::Path) -> anyhow::Result<Option<Vec<String>>> {
    let entry = entry_html.canonicalize()?;
    let current = std::env::current_dir()?.canonicalize()?;
    let workspace = if entry.starts_with(&current) {
        current
    } else {
        entry
            .parent()
            .ok_or_else(|| anyhow::anyhow!("브라우저 엔트리의 상위 폴더가 없습니다"))?
            .to_path_buf()
    };
    smoke_test_in_workspace(&workspace, &entry).await
}

pub(crate) async fn smoke_test_in_workspace(
    workspace: &std::path::Path,
    entry_html: &std::path::Path,
) -> anyhow::Result<Option<Vec<String>>> {
    let Some(browser) = detect_browser() else {
        return Ok(None);
    };
    let run_dir =
        std::env::temp_dir().join(format!("rafikx-browser-smoke-{}", crate::db::Db::new_id()));
    std::fs::create_dir_all(&run_dir)?;
    let mut resources = SmokeResources::new(run_dir.clone());
    let stage_dir = run_dir.join("web");
    let profile_dir = run_dir.join("profile");
    let staged_entry = stage_web_root(workspace, entry_html, &stage_dir)?;
    let (address, server, server_errors, root) = start_server(&stage_dir, &staged_entry).await?;
    resources.server = Some(server);
    let entry = staged_entry.canonicalize()?;
    let relative = entry.strip_prefix(&root)?;
    let url = format!("http://{address}/{}", percent_encode_path(relative));
    std::fs::create_dir_all(&profile_dir)?;
    let profile_flag = format!("--user-data-dir={}", profile_dir.display());
    // --no-sandbox 는 넣지 않는다 — 이 플래그가 있으면 콘솔 로그가 캡처되지
    // 않는다(실측). 컨테이너 root 등 필요한 환경은 RAFIKX_BROWSER_EXTRA_FLAGS 로
    // 추가 플래그를 넣는다 (공백 구분).
    let extra: Vec<String> = std::env::var("RAFIKX_BROWSER_EXTRA_FLAGS")
        .unwrap_or_default()
        .split_whitespace()
        .filter(|flag| safe_extra_browser_flag(flag))
        .map(str::to_string)
        .collect();
    let isolation = network_isolation_flags(address);
    let mut command = tokio::process::Command::new(browser);
    command
        .args([
            "--headless",
            "--disable-gpu",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--enable-logging=stderr",
            "--metrics-recording-only",
            "--no-default-browser-check",
            "--no-first-run",
            "--v=0",
        ])
        .arg(profile_flag)
        .args(&extra)
        .args(&isolation)
        .arg(&url)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let process_scope = crate::process_tree::isolate(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| anyhow::anyhow!("브라우저 실행 실패: {error}"))?;
    let Some(stderr) = child.stderr.take() else {
        crate::process_tree::terminate(&mut child, &process_scope).await;
        anyhow::bail!("브라우저 stderr를 열 수 없습니다");
    };
    let stderr_log = Arc::new(Mutex::new(String::new()));
    let reader_log = stderr_log.clone();
    let stderr_overflow = Arc::new(AtomicBool::new(false));
    let reader_overflow = stderr_overflow.clone();
    let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(false);
    let reader = tokio::spawn(async move {
        let mut stderr = stderr;
        let mut buffer = [0u8; 8 * 1024];
        let mut marker_tail = Vec::new();
        while let Ok(read) = stderr.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            let mut scan = marker_tail;
            scan.extend_from_slice(&buffer[..read]);
            if scan
                .windows(READY_MARKER.len())
                .any(|window| window == READY_MARKER.as_bytes())
            {
                let _ = ready_tx.send(true);
            }
            let keep = READY_MARKER.len().saturating_sub(1).min(scan.len());
            marker_tail = scan[scan.len() - keep..].to_vec();
            if let Ok(mut log) = reader_log.lock() {
                if append_bounded_stderr(&mut log, &String::from_utf8_lossy(&buffer[..read])) {
                    reader_overflow.store(true, Ordering::Relaxed);
                }
            }
        }
    });
    resources.reader = Some(reader);

    let deadline = tokio::time::sleep(std::time::Duration::from_secs(15));
    tokio::pin!(deadline);
    let mut early_status = None;
    let ready = loop {
        if *ready_rx.borrow() {
            break true;
        }
        if let Some(status) = child.try_wait()? {
            early_status = Some(status);
            break false;
        }
        tokio::select! {
            _ = &mut deadline => break false,
            changed = ready_rx.changed() => {
                if changed.is_err() {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    };
    let (success, code) = if ready {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        match child.try_wait()? {
            Some(status) => (status.success(), status.code()),
            None => {
                crate::process_tree::terminate(&mut child, &process_scope).await;
                (true, Some(0))
            }
        }
    } else if let Some(status) = early_status {
        (status.success(), status.code())
    } else {
        crate::process_tree::terminate(&mut child, &process_scope).await;
        let _ = await_stderr_reader(&mut resources).await;
        anyhow::bail!("브라우저 준비 프로브 시간 초과 (15초)");
    };
    await_stderr_reader(&mut resources).await?;
    let captured_server_errors = server_errors
        .lock()
        .map(|errors| errors.clone())
        .unwrap_or_default();
    let stderr = stderr_log.lock().map(|log| log.clone()).unwrap_or_default();
    Ok(Some(evaluate_browser_output(
        success,
        code,
        &stderr,
        stderr_overflow.load(Ordering::Relaxed),
        &captured_server_errors,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uncaught_reference_error() {
        let stderr = "[38240:20464795:0829/143631.043658:INFO:CONSOLE:230] \"Uncaught ReferenceError: camTarget is not defined\", source: file:///tmp/game.js (230)";
        let errors = parse_console_errors(stderr);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("camTarget is not defined"));
    }

    #[test]
    fn ignores_info_logs_and_dedups() {
        let stderr = "\
[1:1:INFO:CONSOLE(1)] \"게임 초기화 완료\", source: x (1)
[2:2:INFO:CONSOLE(2)] \"Uncaught TypeError: foo is not a function\", source: x (2)
[3:3:INFO:CONSOLE(3)] \"Uncaught TypeError: foo is not a function\", source: x (2)";
        let errors = parse_console_errors(stderr);
        assert_eq!(errors.len(), 1, "정보 로그 무시·중복 제거: {errors:?}");
    }

    #[test]
    fn nonzero_browser_exit_is_a_gate_failure() {
        let error = evaluate_browser_output(false, Some(9), "chrome crashed", false, &[])
            .expect_err("nonzero browser exit must fail");
        assert!(error.to_string().contains('9'));
    }

    #[test]
    fn captures_console_resource_and_cors_failures() {
        let stderr = format!(
            "[1:1:INFO:CONSOLE(1)] \"{ERROR_MARKER}console: broken\"\nAccess to script blocked by CORS policy\nFailed to load resource: net::ERR_FILE_NOT_FOUND"
        );
        let errors = parse_console_errors(&stderr);
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors.iter().any(|error| error.contains("console: broken")));
    }

    #[test]
    fn successful_process_without_ready_probe_is_not_a_pass() {
        let error = evaluate_browser_output(true, Some(0), "", false, &[])
            .expect_err("missing probe must fail");
        assert!(error.to_string().contains("프로브"));
    }

    #[test]
    fn browser_stderr_overflow_is_a_gate_failure() {
        let error = evaluate_browser_output(true, Some(0), READY_MARKER, true, &[])
            .expect_err("stderr overflow must fail");
        assert!(error.to_string().contains("상한"));
    }

    #[tokio::test]
    async fn stderr_reader_wait_has_a_deadline() {
        let run_dir = std::env::temp_dir().join(format!(
            "rafikx-browser-reader-deadline-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&run_dir).expect("run directory");
        let mut resources = SmokeResources::new(run_dir);
        resources.reader = Some(tokio::spawn(std::future::pending()));
        let started = Instant::now();

        let error = await_stderr_reader(&mut resources)
            .await
            .expect_err("hung stderr reader must time out");
        assert!(error.to_string().contains("시간 초과"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn detects_browser_or_returns_none() {
        // macOS 기본 경로에 Chrome 이 있는 환경에서는 Some, 없으면 None — 둘 다 유효.
        let found = detect_browser();
        if std::path::Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
            .exists()
        {
            assert!(found.is_some());
        }
    }

    #[test]
    fn discovers_nested_entry_for_javascript_change() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-discovery-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("src")).expect("source directory");
        std::fs::create_dir_all(root.join("public")).expect("public directory");
        std::fs::write(root.join("src/app.js"), "console.log('ok')").expect("source fixture");
        std::fs::write(
            root.join("public/index.html"),
            "<script src=\"../src/app.js\"></script><canvas></canvas>",
        )
        .expect("entry fixture");

        let entries = discover_entries(&root, &["src/app.js".into()]).expect("discover entries");
        assert_eq!(
            entries,
            vec![
                root.join("public/index.html")
                    .canonicalize()
                    .expect("entry")
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unrelated_workspace_entries_do_not_consume_the_browser_cap() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-related-entry-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("app/src")).expect("source directory");
        std::fs::create_dir_all(root.join("app/public")).expect("entry directory");
        std::fs::write(root.join("app/src/app.js"), "console.log('ok')").expect("source fixture");
        std::fs::write(
            root.join("app/public/index.html"),
            "<script src=\"../src/app.js\"></script>",
        )
        .expect("related entry");
        for index in 0..12 {
            let directory = root.join(format!("examples/example-{index}"));
            std::fs::create_dir_all(&directory).expect("unrelated directory");
            std::fs::write(directory.join("index.html"), "<canvas></canvas>")
                .expect("unrelated entry");
        }

        let entries = discover_entries(&root, &["app/src/app.js".into()])
            .expect("discover only related entry");
        assert_eq!(
            entries,
            vec![
                root.join("app/public/index.html")
                    .canonicalize()
                    .expect("related entry")
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_nonconventional_entry_that_references_the_change() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-reference-entry-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("shared")).expect("source directory");
        std::fs::create_dir_all(root.join("demos/custom/launch")).expect("entry directory");
        std::fs::write(root.join("shared/runtime.js"), "console.log('ok')")
            .expect("source fixture");
        std::fs::write(
            root.join("demos/custom/launch/index.html"),
            "<script src=\"../../../shared/runtime.js\"></script>",
        )
        .expect("referencing entry");

        let entries = discover_entries(&root, &["shared/runtime.js".into()])
            .expect("discover referenced entry");
        assert_eq!(
            entries,
            vec![
                root.join("demos/custom/launch/index.html")
                    .canonicalize()
                    .expect("entry")
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_non_index_html_entry_that_references_the_change() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-non-index-entry-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("scripts")).expect("source directory");
        std::fs::write(root.join("scripts/runtime.js"), "missingFunction();")
            .expect("source fixture");
        std::fs::write(
            root.join("launch.html"),
            "<script src=\"./scripts/runtime.js\"></script>",
        )
        .expect("entry fixture");

        let entries = discover_entries(&root, &["scripts/runtime.js".into()])
            .expect("discover non-index entry");
        assert_eq!(
            entries,
            vec![root.join("launch.html").canonicalize().expect("entry")]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_entry_through_local_module_imports() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-import-graph-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("js")).expect("source directory");
        std::fs::write(
            root.join("index.html"),
            "<script type=\"module\" src=\"./js/app.js\"></script>",
        )
        .expect("entry fixture");
        std::fs::write(
            root.join("js/app.js"),
            "// don't hide the next import\nimport './events.js';",
        )
        .expect("root module");
        std::fs::write(
            root.join("js/events.js"),
            "const contraction = () => /isn't/; if (true) /don't/.test('x'); import './state.js';",
        )
        .expect("intermediate module");
        std::fs::write(root.join("js/state.js"), "missingFunction();").expect("changed module");

        let entries =
            discover_entries(&root, &["js/state.js".into()]).expect("discover transitive entry");
        assert_eq!(
            entries,
            vec![root.join("index.html").canonicalize().expect("entry")]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn discovers_case_variant_changed_source_on_case_insensitive_filesystems() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-case-variant-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("runtime-broken.js"), "missingFunction();")
            .expect("source fixture");
        std::fs::write(
            root.join("launch.html"),
            "<script src=\"./runtime-broken.js\"></script>",
        )
        .expect("entry fixture");

        let entries = discover_entries(&root, &["RUNTIME-BROKEN.JS".into()])
            .expect("discover case-variant source");
        assert_eq!(
            entries,
            vec![root.join("launch.html").canonicalize().expect("entry")]
        );

        std::fs::write(
            root.join("launch.html"),
            "<script src=\"./RUNTIME-BROKEN.JS\"></script>",
        )
        .expect("case-variant entry fixture");
        let entries = discover_entries(&root, &["runtime-broken.js".into()])
            .expect("discover case-variant reference");
        assert_eq!(
            entries,
            vec![root.join("launch.html").canonicalize().expect("entry")]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unrelated_ancestor_entry_does_not_cover_a_nested_application() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-ancestor-entry-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("app/pages/launch")).expect("entry directory");
        std::fs::write(root.join("index.html"), "<canvas></canvas>").expect("unrelated root entry");
        std::fs::write(root.join("app/app.js"), "missingFunction();").expect("changed source");
        std::fs::write(
            root.join("app/pages/launch/index.html"),
            "<script src=\"../../app.js\"></script>",
        )
        .expect("related nested entry");

        let entries = discover_entries(&root, &["app/app.js".into()])
            .expect("discover the related nested entry");
        assert_eq!(
            entries,
            vec![
                root.join("app/pages/launch/index.html")
                    .canonicalize()
                    .expect("entry")
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_every_changed_html_entry() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-multi-entry-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("first")).expect("first directory");
        std::fs::create_dir_all(root.join("second")).expect("second directory");
        std::fs::write(root.join("first/page.html"), "<canvas></canvas>").expect("first entry");
        std::fs::write(root.join("second/page.html"), "<canvas></canvas>").expect("second entry");

        let entries = discover_entries(
            &root,
            &["first/page.html".into(), "second/page.html".into()],
        )
        .expect("discover entries");
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&root.join("first/page.html").canonicalize().expect("first")));
        assert!(
            entries.contains(
                &root
                    .join("second/page.html")
                    .canonicalize()
                    .expect("second")
            )
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn one_covered_entry_cannot_mask_an_uncovered_changed_source() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-uncovered-source-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("index.html"), "<script src=\"app.js\"></script>")
            .expect("entry fixture");
        std::fs::write(root.join("app.js"), "console.log('ok')").expect("covered source");
        std::fs::write(root.join("orphan.js"), "missingFunction();")
            .expect("uncovered source");

        let error = discover_entries(&root, &["app.js".into(), "orphan.js".into()])
            .expect_err("uncovered source must fail closed");

        assert!(error.to_string().contains("orphan.js"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn standalone_javascript_without_html_entry_stays_node_only() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-standalone-js-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("script.js"), "console.log('ok')")
            .expect("standalone source");

        let entries = discover_entries(&root, &["script.js".into()])
            .expect("standalone JavaScript needs no browser entry");

        assert!(entries.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stages_only_non_sensitive_web_assets() {
        let root =
            std::env::temp_dir().join(format!("rafikx-browser-stage-{}", crate::db::Db::new_id()));
        let stage = root.join("stage");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(workspace.join(".git")).expect("git fixture");
        std::fs::create_dir_all(workspace.join("assets")).expect("asset fixture");
        std::fs::write(
            workspace.join("index.html"),
            "<script src=\"app.js\"></script>",
        )
        .expect("entry fixture");
        std::fs::write(workspace.join("app.js"), "console.log('ok')").expect("script fixture");
        std::fs::write(workspace.join("tokens.css"), ":root { --ink: #111; }")
            .expect("design tokens fixture");
        std::fs::write(workspace.join("assets/pixel.png"), b"png").expect("image fixture");
        std::fs::write(workspace.join(".env"), "PRIVATE_VALUE=short-secret").expect("env fixture");
        std::fs::write(workspace.join(".git/config"), "credential=secret").expect("git fixture");
        std::fs::write(workspace.join("notes.txt"), "short-secret").expect("text fixture");
        std::fs::write(workspace.join("secret.js"), "short-secret").expect("secret fixture");
        std::fs::write(workspace.join("aws-secrets.js"), "short-secret")
            .expect("prefixed secret fixture");

        let staged_entry = stage_web_root(&workspace, &workspace.join("index.html"), &stage)
            .expect("stage web root");
        assert_eq!(staged_entry, stage.join("index.html"));
        assert!(stage.join("app.js").is_file());
        assert!(stage.join("tokens.css").is_file());
        assert!(stage.join("assets/pixel.png").is_file());
        assert!(!stage.join(".env").exists());
        assert!(!stage.join(".git/config").exists());
        assert!(!stage.join("notes.txt").exists());
        assert!(!stage.join("secret.js").exists());
        assert!(!stage.join("aws-secrets.js").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn staging_preserves_project_relative_sibling_assets() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-project-stage-{}",
            crate::db::Db::new_id()
        ));
        let workspace = root.join("workspace");
        let stage = root.join("stage");
        std::fs::create_dir_all(workspace.join("public")).expect("public directory");
        std::fs::create_dir_all(workspace.join("src")).expect("source directory");
        std::fs::write(workspace.join("package.json"), "{}\n").expect("project marker");
        std::fs::write(
            workspace.join("public/index.html"),
            "<script src=\"../src/app.js\"></script>",
        )
        .expect("entry fixture");
        std::fs::write(workspace.join("src/app.js"), "console.log('ok')").expect("sibling source");

        let staged_entry = stage_web_root(&workspace, &workspace.join("public/index.html"), &stage)
            .expect("stage project root");
        assert_eq!(staged_entry, stage.join("public/index.html"));
        assert!(stage.join("src/app.js").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn nested_project_entry_loads_sibling_source_in_browser() {
        if detect_browser().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-project-smoke-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("public")).expect("public directory");
        std::fs::create_dir_all(root.join("src")).expect("source directory");
        std::fs::write(root.join("package.json"), "{}\n").expect("project marker");
        std::fs::write(
            root.join("public/index.html"),
            "<script src=\"../src/app.js\"></script><canvas></canvas>",
        )
        .expect("entry fixture");
        std::fs::write(root.join("src/app.js"), "window.rafikxLoaded = true;")
            .expect("sibling source");

        let errors = smoke_test_in_workspace(&root, &root.join("public/index.html"))
            .await
            .expect("browser smoke")
            .expect("installed browser");
        assert!(errors.is_empty(), "browser errors: {errors:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn staging_entry_limit_fails_closed() {
        let root =
            std::env::temp_dir().join(format!("rafikx-browser-cap-{}", crate::db::Db::new_id()));
        let workspace = root.join("workspace");
        let stage = root.join("stage");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let entry = workspace.join("index.html");
        std::fs::write(&entry, "<canvas></canvas>").expect("entry");

        let error =
            stage_web_root_with_limits(&workspace, &entry, &stage, 1, Duration::from_secs(1))
                .err()
                .expect("entry limit must fail");
        assert!(error.to_string().contains("항목 수"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn browser_stderr_is_bounded() {
        let mut log = String::new();
        assert!(append_bounded_stderr(
            &mut log,
            &"x".repeat(MAX_BROWSER_STDERR_BYTES + 10)
        ));
        assert!(append_bounded_stderr(&mut log, "more"));
        assert_eq!(log.len(), MAX_BROWSER_STDERR_BYTES);
    }

    #[test]
    fn browser_responses_restrict_network_and_embedding() {
        assert!(SECURITY_HEADERS.contains("connect-src 'self'"));
        assert!(SECURITY_HEADERS.contains("form-action 'none'"));
        assert!(SECURITY_HEADERS.contains("frame-ancestors 'none'"));
        assert!(SECURITY_HEADERS.contains("sandbox allow-same-origin allow-scripts"));
        assert!(SECURITY_HEADERS.contains("X-Content-Type-Options: nosniff"));
        assert!(SECURITY_HEADERS.contains("X-DNS-Prefetch-Control: off"));
        let flags = network_isolation_flags("127.0.0.1:43123".parse().expect("address"));
        assert_eq!(
            flags[0],
            "--proxy-bypass-list=<-loopback>;http://127.0.0.1:43123"
        );
        assert!(!flags[0].ends_with("127.0.0.1"));
        assert!(safe_extra_browser_flag("--no-sandbox"));
        assert!(!safe_extra_browser_flag("https://example.com"));
        assert!(!safe_extra_browser_flag("--no-proxy-server"));
        assert!(!safe_extra_browser_flag("--disable-web-security"));
        assert!(!safe_extra_browser_flag("--user-data-dir=/tmp/shared"));
    }

    #[tokio::test]
    async fn smoke_page_cannot_reach_another_loopback_service() {
        if detect_browser().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-loopback-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("browser fixture");
        let trap = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback trap");
        let trap_url = format!(
            "http://{}/private",
            trap.local_addr().expect("trap address")
        );
        let html =
            format!("<script>fetch('{trap_url}').catch(() => {{}});</script><canvas></canvas>");
        let entry = root.join("index.html");
        std::fs::write(&entry, html).expect("browser fixture html");

        let result = smoke_test_in_workspace(&root, &entry)
            .await
            .expect("browser smoke");
        assert!(result.is_some());
        let reached =
            tokio::time::timeout(std::time::Duration::from_millis(300), trap.accept()).await;
        assert!(reached.is_err(), "page reached a different loopback port");
        let _ = std::fs::remove_dir_all(root);
    }
}
