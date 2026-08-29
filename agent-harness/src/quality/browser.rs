//! 브라우저 스모크 게이트 (S4 보강) — HTML/JS 산출물의 런타임 오류를 실제 브라우저로 잡는다.
//! 기원: 2026-08-29 사용자 실측 — "슈퍼마리오 게임이 실행도 안 되는" 결과물.
//! game.js 의 잔재 변수(camTarget)가 첫 프레임에서 ReferenceError 를 냈지만,
//! node --check(구문만)·eslint(미설치)·내장 보안 스캐너(보안 전용) 어디에도 걸리지 않았다.
//! "사용자가 결함을 발견하는 순간 = 게이트 설계의 실패" 원칙에 따라 추가된 게이트다.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const ERROR_MARKER: &str = "__RAFIKX_BROWSER_ERROR__";
const READY_MARKER: &str = "__RAFIKX_BROWSER_READY__";
const PROBE_PATH: &str = "/__rafikx_probe.js";
const MAX_STAGED_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STAGED_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const SECURITY_HEADERS: &str = "Content-Security-Policy: default-src 'self'; base-uri 'none'; connect-src 'self'; font-src 'self' data:; form-action 'none'; frame-ancestors 'none'; frame-src 'none'; img-src 'self' data: blob:; media-src 'self' blob:; object-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; worker-src 'self' blob:\r\nX-Content-Type-Options: nosniff\r\nX-DNS-Prefetch-Control: off\r\nReferrer-Policy: no-referrer\r\n";
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

fn evaluate_browser_output(
    success: bool,
    code: Option<i32>,
    stderr: &str,
    server_errors: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut errors = parse_console_errors(stderr);
    for error in server_errors {
        if !errors.contains(error) {
            errors.push(error.clone());
        }
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
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("wasm") => "application/wasm",
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
    let workspace = workspace.canonicalize()?;
    let entry = entry_html.canonicalize()?;
    if !entry.starts_with(&workspace) || !entry.is_file() {
        anyhow::bail!("브라우저 엔트리가 워크스페이스 밖에 있습니다");
    }
    let source_root = entry
        .parent()
        .ok_or_else(|| anyhow::anyhow!("브라우저 엔트리의 상위 폴더가 없습니다"))?
        .to_path_buf();
    std::fs::create_dir_all(stage)?;
    let mut staged_bytes = 0u64;
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
                && !matches!(
                    item.file_name().to_string_lossy().as_ref(),
                    "node_modules" | "target"
                )
        })
        .build();
    for item in walker.flatten() {
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
    }
    let relative_entry = entry.strip_prefix(&source_root)?;
    let staged_entry = stage.join(relative_entry);
    if !staged_entry.is_file() {
        anyhow::bail!("브라우저 엔트리를 격리 스테이징하지 못했습니다");
    }
    Ok(staged_entry)
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
                errors.push(format!("HTTP 404: /{decoded}"));
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
pub async fn smoke_test(
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
        .args([
            "--proxy-bypass-list=127.0.0.1",
            "--proxy-server=http://127.0.0.1:9",
            "--host-resolver-rules=MAP * 0.0.0.0, EXCLUDE 127.0.0.1",
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        ])
        .arg(&url)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| anyhow::anyhow!("브라우저 실행 실패: {error}"))?;
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill().await;
        anyhow::bail!("브라우저 stderr를 열 수 없습니다");
    };
    let stderr_log = Arc::new(Mutex::new(String::new()));
    let reader_log = stderr_log.clone();
    let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(false);
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains(READY_MARKER) {
                let _ = ready_tx.send(true);
            }
            if let Ok(mut log) = reader_log.lock() {
                log.push_str(&line);
                log.push('\n');
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
                let _ = child.kill().await;
                let _ = child.wait().await;
                (true, Some(0))
            }
        }
    } else if let Some(status) = early_status {
        (status.success(), status.code())
    } else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        if let Some(reader) = resources.reader.take() {
            let _ = reader.await;
        }
        anyhow::bail!("브라우저 준비 프로브 시간 초과 (15초)");
    };
    if let Some(reader) = resources.reader.take() {
        let _ = reader.await;
    }
    let captured_server_errors = server_errors
        .lock()
        .map(|errors| errors.clone())
        .unwrap_or_default();
    let stderr = stderr_log.lock().map(|log| log.clone()).unwrap_or_default();
    Ok(Some(evaluate_browser_output(
        success,
        code,
        &stderr,
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
        let error = evaluate_browser_output(false, Some(9), "chrome crashed", &[])
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
        let error =
            evaluate_browser_output(true, Some(0), "", &[]).expect_err("missing probe must fail");
        assert!(error.to_string().contains("프로브"));
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
    fn browser_responses_restrict_network_and_embedding() {
        assert!(SECURITY_HEADERS.contains("connect-src 'self'"));
        assert!(SECURITY_HEADERS.contains("form-action 'none'"));
        assert!(SECURITY_HEADERS.contains("frame-ancestors 'none'"));
        assert!(SECURITY_HEADERS.contains("X-Content-Type-Options: nosniff"));
        assert!(SECURITY_HEADERS.contains("X-DNS-Prefetch-Control: off"));
        assert!(safe_extra_browser_flag("--no-sandbox"));
        assert!(!safe_extra_browser_flag("https://example.com"));
        assert!(!safe_extra_browser_flag("--no-proxy-server"));
        assert!(!safe_extra_browser_flag("--disable-web-security"));
        assert!(!safe_extra_browser_flag("--user-data-dir=/tmp/shared"));
    }
}
