use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rafikx::config::{Config, ProviderConfig};
use rafikx::harness::{Binding, TaskClass, classify_rules, run_pipeline_with_context};
use rafikx::quality::detect_duplicate_blocks;
use rafikx::quality::run_quality_gate;
use rafikx::run::{RunContext, RunId};
use rafikx::tools::{ToolCtx, ToolRegistry};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct TestWorkspace(PathBuf);

impl TestWorkspace {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rafikx-recovery-{label}-{}",
            rafikx::db::Db::new_id()
        ));
        fs::create_dir_all(&path).expect("create test workspace");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scripted_config(workspace: &Path, base_url: String, context_window: u32) -> Config {
    let mut cfg = Config::load(Some(&workspace.join("config.toml"))).expect("config");
    cfg.workspace = workspace.to_path_buf();
    cfg.file.general.workspace = workspace.to_string_lossy().into_owned();
    cfg.file.general.default_provider = "scripted".into();
    cfg.file.general.engine = "rafikx".into();
    cfg.file.memory.enabled = false;
    cfg.file.fallback.enabled = false;
    cfg.file.self_harness.enabled = false;
    cfg.file.self_harness.meta = false;
    cfg.file.harness.strict_gate = false;
    cfg.file.harness.review_committee = false;
    cfg.file.providers.insert(
        "scripted".into(),
        ProviderConfig {
            kind: "openai_compat".into(),
            auth: "none".into(),
            api_key_env: String::new(),
            model: "scripted-model".into(),
            small_model: None,
            base_url: Some(base_url),
            supports_tools: true,
            models_url: None,
            model_auto: false,
            context_window: Some(context_window),
            enabled: true,
        },
    );
    cfg
}

fn scripted_binding(
    class: TaskClass,
    tools: &[&str],
    max_iterations: u32,
    verify: bool,
    context_window: u32,
) -> Binding {
    Binding {
        combo_chain: Vec::new(),
        class,
        profile_name: if class == TaskClass::Dev {
            "coder".into()
        } else {
            "quick".into()
        },
        provider_name: "scripted".into(),
        model: "scripted-model".into(),
        kind: "openai_compat".into(),
        tools: tools.iter().map(|tool| (*tool).to_string()).collect(),
        max_iterations,
        plan_first: false,
        verify,
        verify_command: String::new(),
        system_extra: String::new(),
        context_window,
        verify_model: None,
    }
}

fn game_e2e_html() -> &'static str {
    r#"<!doctype html><html><head><meta name="rafikx-browser-game-contract" content="v1"><link rel="stylesheet" href="style.css"></head><body><main><h1>Rafik Run</h1><canvas id="game" width="640" height="360"></canvas><p id="status">READY</p></main><script src="game.js"></script></body></html>"#
}

fn game_e2e_style() -> &'static str {
    "body{margin:0;background:#101828;color:#fff}main{max-width:720px;margin:auto}canvas{width:100%;background:#d9f2ff}"
}

fn game_e2e_repaired_source() -> &'static str {
    r#"const canvas = document.querySelector('#game');
const context = canvas.getContext('2d');
const status = document.querySelector('#status');
const game = { mode: 'ready', restarts: 0 };
function render() {
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.fillStyle = game.mode === 'lost' ? '#dc2626' : '#4f46e5';
  context.fillRect(48, 280, 28, 28);
  status.textContent = game.mode.toUpperCase();
}
function restart() {
  game.mode = 'ready';
  game.restarts += 1;
  render();
}
document.addEventListener('keydown', event => {
  if (event.code === 'Space' && game.mode === 'ready') game.mode = 'playing';
  else if (event.code === 'KeyP' && game.mode === 'playing') game.mode = 'paused';
  else if (event.code === 'KeyP' && game.mode === 'paused') game.mode = 'playing';
  else if (event.code === 'KeyR') restart();
  render();
});
window.__rafikxGameTest = {
  state: () => game.mode,
  restarts: () => game.restarts,
  forceLoss: () => { game.mode = 'lost'; render(); }
};
render();"#
}

fn scripted_tool_response(calls: Vec<(&str, &str, serde_json::Value)>) -> String {
    let tool_calls = calls
        .into_iter()
        .enumerate()
        .map(|(index, (id, name, input))| {
            json!({
                "index": index,
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": input.to_string()}
            })
        })
        .collect::<Vec<_>>();
    let chunk = json!({
        "model": "scripted-model",
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "tool_calls": tool_calls},
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 10}
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn scripted_text_response(text: &str) -> String {
    let chunk = json!({
        "model": "scripted-model",
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 4}
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn scripted_limited_response(text: &str) -> String {
    let chunk = json!({
        "model": "scripted-model",
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": text},
            "finish_reason": "length"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 10}
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn scripted_game_response(index: usize) -> String {
    match index {
        0 => scripted_tool_response(vec![
            (
                "todo-start",
                "todo_write",
                json!({"todos":[{"content":"브라우저 게임 생성과 검증","status":"in_progress","priority":"high"}]}),
            ),
            ("write-html", "write_file", json!({"path":"index.html","content":game_e2e_html()})),
            ("write-css", "write_file", json!({"path":"style.css","content":game_e2e_style()})),
            ("write-broken", "write_file", json!({"path":"game.js","content":"const canvas = document.querySelector('#game'); missingGameLoop(canvas);"})),
        ]),
        1 => scripted_tool_response(vec![(
            "todo-complete",
            "todo_write",
            json!({"todos":[{"content":"브라우저 게임 생성과 검증","status":"completed","priority":"high"}]}),
        )]),
        2 => scripted_text_response("구현을 완료했습니다."),
        3 => scripted_tool_response(vec![(
            "repair-game",
            "write_file",
            json!({"path":"game.js","content":game_e2e_repaired_source()}),
        )]),
        4 => scripted_text_response("런타임 오류와 상태 전이를 수정했습니다."),
        other => scripted_text_response(&format!("unexpected request {other}")),
    }
}

async fn read_http_body(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buffer = Vec::new();
    let header_end = loop {
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if buffer.len() > 4 * 1024 * 1024 {
            return Err(std::io::Error::other("scripted request headers too large"));
        }
        let mut chunk = [0u8; 8192];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "scripted request ended before headers",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    if content_length > 4 * 1024 * 1024 {
        return Err(std::io::Error::other("scripted request body too large"));
    }
    while buffer.len() < header_end + content_length {
        let mut chunk = [0u8; 8192];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "scripted request body truncated",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Ok(String::from_utf8_lossy(&buffer[header_end..header_end + content_length]).into_owned())
}

async fn start_scripted_game_model() -> (
    String,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("scripted listener");
    let address = listener.local_addr().expect("scripted address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        for index in 0..5 {
            let (mut stream, _) = listener.accept().await.expect("scripted accept");
            let body = read_http_body(&mut stream).await.expect("scripted request");
            captured.lock().expect("request log").push(body);
            let body = scripted_game_response(index);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("scripted response");
        }
    });
    (format!("http://{address}/v1"), requests, server)
}

async fn start_scripted_budget_model() -> (
    String,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("budget listener");
    let address = listener.local_addr().expect("budget address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().await.expect("budget accept");
            let body = read_http_body(&mut stream).await.expect("budget request");
            captured.lock().expect("budget request log").push(body);
            let body = if index == 0 {
                scripted_tool_response(vec![
                    (
                        "task-a",
                        "task",
                        json!({"prompt":"child budget A","class":"simple","role":"quick","model":"scripted-model"}),
                    ),
                    (
                        "task-b",
                        "task",
                        json!({"prompt":"child budget B","class":"simple","role":"quick","model":"scripted-model"}),
                    ),
                ])
            } else {
                scripted_text_response("child completed")
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("budget response");
        }
    });
    (format!("http://{address}/v1"), requests, server)
}

async fn start_scripted_child_failure_model() -> (
    String,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("child failure listener");
    let address = listener.local_addr().expect("child failure address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        for index in 0..4 {
            let (mut stream, _) = listener.accept().await.expect("child failure accept");
            let request = read_http_body(&mut stream)
                .await
                .expect("child failure request");
            let body = if index == 0 {
                scripted_tool_response(vec![
                    (
                        "task-fail",
                        "task",
                        json!({"prompt":"child must fail","class":"simple","role":"quick","model":"scripted-model"}),
                    ),
                    (
                        "task-ok",
                        "task",
                        json!({"prompt":"child succeeds","class":"simple","role":"quick","model":"scripted-model"}),
                    ),
                ])
            } else if request.contains("child must fail")
                && !request.contains("child succeeds")
            {
                scripted_limited_response("partial child output")
            } else if request.contains("child succeeds")
                && !request.contains("child must fail")
            {
                scripted_text_response("child completed")
            } else {
                scripted_text_response("parent declares completion")
            };
            captured
                .lock()
                .expect("child failure request log")
                .push(request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("child failure response");
        }
    });
    (format!("http://{address}/v1"), requests, server)
}

async fn start_scripted_child_retry_model() -> (
    String,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("child retry listener");
    let address = listener.local_addr().expect("child retry address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        for index in 0..5 {
            let (mut stream, _) = listener.accept().await.expect("child retry accept");
            let request = read_http_body(&mut stream)
                .await
                .expect("child retry request");
            captured
                .lock()
                .expect("child retry request log")
                .push(request);
            let body = match index {
                0 | 2 => scripted_tool_response(vec![(
                    if index == 0 { "task-first" } else { "task-retry" },
                    "task",
                    json!({"prompt":"retry identical child","class":"simple","role":"quick","model":"scripted-model"}),
                )]),
                1 => scripted_limited_response("partial child output"),
                3 => scripted_text_response("child recovered"),
                _ => scripted_text_response("parent completion after recovery"),
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("child retry response");
        }
    });
    (format!("http://{address}/v1"), requests, server)
}

#[test]
fn english_game_creation_routes_to_dev_tools() {
    // Given: an English artifact request with no file extension.
    let prompt = "create a Super Mario browser game";

    // When: rules classify the request.
    let class = classify_rules(prompt, false);

    // Then: it must receive the tool-capable development profile.
    assert_eq!(class, TaskClass::Dev);
    assert_eq!(classify_rules("write a browser game", false), TaskClass::Dev);
    assert_eq!(classify_rules("fix my browser game", false), TaskClass::Dev);
    assert_eq!(classify_rules("add a button to the app", false), TaskClass::Dev);
    assert_eq!(classify_rules("remove obsolete function", false), TaskClass::Dev);
    assert_eq!(classify_rules("delete the old endpoint", false), TaskClass::Dev);
    assert_eq!(classify_rules("add sugar to tea", false), TaskClass::Simple);
    assert_eq!(classify_rules("make me happy", false), TaskClass::Simple);
}

#[test]
fn legitimate_parallel_initializers_are_not_duplicate_code() {
    // Given: two domain objects that legitimately initialize the same coordinates.
    let source = r#"
class Player {
  reset() {
    this.x = 0;
    this.y = 0;
    this.velocity = 0;
  }
}
class Enemy {
  reset() {
    this.x = 0;
    this.y = 0;
    this.velocity = 0;
  }
}
"#;

    // When: the readability scanner inspects the file.
    let findings = detect_duplicate_blocks("game.js", source);

    // Then: a normal three-line initializer must not fail the whole quality gate.
    assert!(
        findings.is_empty(),
        "false duplicate findings: {findings:?}"
    );
}

#[test]
fn substantial_copied_block_is_reported() {
    // Given: a substantial five-line implementation copied verbatim.
    let block = r#"
const horizontal = input.left - input.right;
player.velocity += horizontal * acceleration;
player.position += player.velocity;
player.velocity *= friction;
renderPlayer(player.position, player.velocity);
"#;
    let source = format!("function first() {{\n{block}}}\nfunction second() {{\n{block}}}\n");

    // When: the scanner inspects the copied implementation.
    let findings = detect_duplicate_blocks("game.js", &source);

    // Then: meaningful copy/paste remains detectable.
    assert_eq!(findings.len(), 1, "duplicate findings: {findings:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bash_nonzero_exit_is_an_error_result() {
    // Given: the real Bash tool in an isolated workspace.
    let workspace = TestWorkspace::new("bash-exit");
    let context = ToolCtx::new(workspace.path().to_path_buf());
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    // When: a command exits unsuccessfully.
    let result = bash.run(json!({"command": "printf failure >&2; exit 7"}), &context);

    // Then: the agent loop must receive a tool error, including its evidence.
    let error = result.expect_err("nonzero exit must be a tool error");
    let message = error.to_string();
    assert!(message.contains("failure"), "missing stderr: {message}");
    assert!(message.contains('7'), "missing exit code: {message}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bash_created_files_are_recorded_as_execution_evidence() {
    let workspace = TestWorkspace::new("bash-mutation");
    let run = RunContext::isolated(
        RunId::new("bash-mutation-test"),
        workspace.path().to_path_buf(),
    );
    let mut context = ToolCtx::new(workspace.path().to_path_buf());
    context.run = Some(run.clone());
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    bash.run(
        json!({"command": "printf '<canvas></canvas>' > game.html"}),
        &context,
    )
    .expect("bash write");

    assert_eq!(
        run.committed_paths(),
        vec![workspace
            .path()
            .join("game.html")
            .canonicalize()
            .expect("canonical game")]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reverted_bash_changes_are_not_execution_evidence() {
    let workspace = TestWorkspace::new("bash-revert");
    fs::write(workspace.path().join("game.js"), "original").expect("seed source");
    let run = RunContext::isolated(
        RunId::new("bash-revert-test"),
        workspace.path().to_path_buf(),
    );
    let mut context = ToolCtx::new(workspace.path().to_path_buf());
    context.run = Some(run.clone());
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    bash.run(
        json!({"command": "printf changed > game.js"}),
        &context,
    )
    .expect("bash change");
    assert_eq!(
        run.committed_paths(),
        vec![workspace
            .path()
            .join("game.js")
            .canonicalize()
            .expect("canonical game")]
    );

    bash.run(
        json!({"command": "printf original > game.js"}),
        &context,
    )
    .expect("bash revert");
    assert!(run.committed_paths().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untrackable_workspace_prevents_bash_execution() {
    let workspace = TestWorkspace::new("bash-tracking-cap");
    let oversized = fs::File::create(workspace.path().join("oversized.bin"))
        .expect("oversized fixture");
    oversized
        .set_len(64 * 1024 * 1024 + 1)
        .expect("sparse oversized fixture");
    let run = RunContext::isolated(
        RunId::new("bash-tracking-cap-test"),
        workspace.path().to_path_buf(),
    );
    let mut context = ToolCtx::new(workspace.path().to_path_buf());
    context.run = Some(run);
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    let error = bash
        .run(json!({"command": "printf ran > marker.txt"}), &context)
        .expect_err("untrackable workspace must block bash");
    assert!(error.to_string().contains("실행 전 변경 추적 실패"));
    assert!(!workspace.path().join("marker.txt").exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_workspace_symlink_prevents_bash_execution() {
    let workspace = TestWorkspace::new("bash-external-link");
    let outside = TestWorkspace::new("bash-external-target");
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("linked"))
        .expect("external symlink");
    let run = RunContext::isolated(
        RunId::new("bash-external-link-test"),
        workspace.path().to_path_buf(),
    );
    let mut context = ToolCtx::new(workspace.path().to_path_buf());
    context.run = Some(run);
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    let error = bash
        .run(
            json!({"command": "printf escaped > linked/outside.txt"}),
            &context,
        )
        .expect_err("external symlink must block bash before execution");
    assert!(error.to_string().contains("실행 전 변경 추적 실패"));
    assert!(!outside.path().join("outside.txt").exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_symlink_under_excluded_directory_prevents_bash_execution() {
    let workspace = TestWorkspace::new("bash-excluded-external-link");
    let outside = TestWorkspace::new("bash-excluded-external-target");
    fs::create_dir_all(workspace.path().join(".cache")).expect("excluded directory");
    std::os::unix::fs::symlink(outside.path(), workspace.path().join(".cache/linked"))
        .expect("external symlink");
    let run = RunContext::isolated(
        RunId::new("bash-excluded-external-link-test"),
        workspace.path().to_path_buf(),
    );
    let mut context = ToolCtx::new(workspace.path().to_path_buf());
    context.run = Some(run);
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    let error = bash
        .run(
            json!({"command": "printf escaped > .cache/linked/outside.txt"}),
            &context,
        )
        .expect_err("excluded-directory external symlink must block bash");
    assert!(error.to_string().contains("실행 전 변경 추적 실패"));
    assert!(!outside.path().join("outside.txt").exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_symlink_alias_tools_share_one_final_baseline() {
    let workspace = TestWorkspace::new("mixed-alias-real");
    let alias = workspace.path().with_extension("alias");
    std::os::unix::fs::symlink(workspace.path(), &alias).expect("workspace alias");
    fs::write(workspace.path().join("game.js"), "original").expect("seed source");
    let run = RunContext::isolated(RunId::new("mixed-alias-test"), alias.clone());
    let mut context = ToolCtx::new(alias.clone());
    context.run = Some(run.clone());
    let registry = ToolRegistry::all();

    registry
        .get("bash")
        .expect("bash tool")
        .run(json!({"command": "printf changed > game.js"}), &context)
        .expect("bash change");
    registry
        .get("write_file")
        .expect("write tool")
        .run(
            json!({"path": "game.js", "content": "original"}),
            &context,
        )
        .expect("file-tool revert");

    assert!(run.committed_paths().is_empty());
    let _ = fs::remove_file(alias);
}

#[tokio::test]
async fn duplicate_advice_does_not_fail_a_valid_artifact() {
    // Given: a valid artifact with a substantial repeated implementation.
    let workspace = TestWorkspace::new("duplicate-advice");
    let block = r#"
const horizontal = input.left - input.right;
player.velocity += horizontal * acceleration;
player.position += player.velocity;
player.velocity *= friction;
renderPlayer(player.position, player.velocity);
"#;
    fs::write(
        workspace.path().join("game.txt"),
        format!("first {{\n{block}}}\nsecond {{\n{block}}}\n"),
    )
    .expect("write fixture");

    // When: the quality gate finds only heuristic duplication advice.
    let report = run_quality_gate(workspace.path(), &["game.txt".to_string()]).await;

    // Then: advice is visible but cannot reject an otherwise valid artifact.
    assert!(
        report.passed,
        "advisory caused failure: {:?}",
        report.findings
    );
    assert!(
        report
            .steps
            .iter()
            .any(|step| step.stage == "S7-duplication")
    );
}

#[tokio::test]
async fn browser_game_quality_gate_rejects_broken_and_accepts_repaired() {
    let workspace = TestWorkspace::new("browser-game-e2e");
    fs::write(
        workspace.path().join("index.html"),
        r#"<!doctype html><html><head><link rel="stylesheet" href="style.css"></head><body><main><h1>Rafik Run</h1><canvas id="game" width="640" height="360"></canvas><p id="status">READY</p></main><script src="game.js"></script></body></html>"#,
    )
    .expect("game html");
    fs::write(
        workspace.path().join("style.css"),
        "body{margin:0;background:#101828;color:#fff}main{max-width:720px;margin:auto}canvas{width:100%;background:#d9f2ff}",
    )
    .expect("game css");
    fs::write(
        workspace.path().join("game.js"),
        "const canvas = document.querySelector('#game'); missingGameLoop(canvas);",
    )
    .expect("broken game source");
    let changed = vec!["index.html".into(), "style.css".into(), "game.js".into()];

    let broken = run_quality_gate(workspace.path(), &changed).await;
    assert!(!broken.passed, "runtime failure passed: {broken:?}");

    fs::write(
        workspace.path().join("game.js"),
        r#"const canvas = document.querySelector('#game');
const context = canvas.getContext('2d');
const status = document.querySelector('#status');
const game = { mode: 'ready', x: 24, y: 280, velocity: 0 };
function reset() { game.mode = 'ready'; game.x = 24; game.y = 280; game.velocity = 0; }
function render() {
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.fillStyle = '#4f46e5';
  context.fillRect(game.x, game.y, 24, 24);
  status.textContent = game.mode.toUpperCase();
}
function frame() {
  if (game.mode === 'playing') {
    game.x += 2;
    game.velocity += 0.4;
    game.y = Math.min(280, game.y + game.velocity);
    if (game.x > canvas.width) game.mode = 'lost';
  }
  render();
  requestAnimationFrame(frame);
}
document.addEventListener('keydown', (event) => {
  if (event.code === 'Space' && game.mode === 'ready') game.mode = 'playing';
  if (event.code === 'KeyP' && game.mode === 'playing') game.mode = 'paused';
  else if (event.code === 'KeyP' && game.mode === 'paused') game.mode = 'playing';
  if (event.code === 'KeyR') reset();
  if (event.code === 'ArrowUp' && game.mode === 'playing' && game.y === 280) game.velocity = -8;
});
reset();
frame();"#,
    )
    .expect("repaired game source");

    let repaired = run_quality_gate(workspace.path(), &changed).await;
    let node_available = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if rafikx::quality::browser::detect_browser().is_some() && node_available {
        assert!(
            repaired.passed,
            "repaired browser game failed: {:?} {:?}",
            repaired.steps, repaired.findings
        );
    } else {
        assert!(!repaired.passed, "missing validator must fail closed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_delegates_share_the_root_model_iteration_budget() {
    let workspace = TestWorkspace::new("shared-delegate-budget");
    let (base_url, requests, server) = start_scripted_budget_model().await;
    let cfg = scripted_config(workspace.path(), base_url, 8_000);
    let binding = scripted_binding(TaskClass::Simple, &["task"], 1, false, 8_000);
    let run = RunContext::for_config(RunId::new("shared-delegate-budget"), Arc::new(cfg.clone()));
    let outcome = run_pipeline_with_context(
        &cfg,
        &binding,
        "delegate two independent child checks",
        true,
        None,
        None,
        None,
        None,
        run,
    )
    .await
    .expect("budget pipeline");
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .expect("budget server timeout")
        .expect("budget server task");
    assert_eq!(outcome.iterations, 2);
    assert_ne!(outcome.status, "ok");
    assert_eq!(requests.lock().expect("budget requests").len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_parallel_child_prevents_parent_success() {
    let workspace = TestWorkspace::new("failed-parallel-child");
    let (base_url, requests, server) = start_scripted_child_failure_model().await;
    let cfg = scripted_config(workspace.path(), base_url, 8_000);
    let binding = scripted_binding(TaskClass::Simple, &["task"], 3, false, 8_000);
    let run = RunContext::for_config(RunId::new("failed-parallel-child"), Arc::new(cfg.clone()));
    let outcome = run_pipeline_with_context(
        &cfg,
        &binding,
        "delegate one failing and one successful child, then finish",
        true,
        None,
        None,
        None,
        None,
        run,
    )
    .await
    .expect("child failure pipeline");
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .expect("child failure server timeout")
        .expect("child failure server task");
    assert_eq!(outcome.iterations, 4);
    assert_eq!(outcome.status, "incomplete");
    assert!(
        outcome
            .error
            .as_deref()
            .is_some_and(|error| error.contains("미해결 위임 작업 1건")),
        "unexpected outcome: {:?}",
        outcome.error
    );
    assert_eq!(requests.lock().expect("child failure requests").len(), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identical_successful_child_retry_clears_the_failure() {
    let workspace = TestWorkspace::new("successful-child-retry");
    let (base_url, requests, server) = start_scripted_child_retry_model().await;
    let cfg = scripted_config(workspace.path(), base_url, 8_000);
    let binding = scripted_binding(TaskClass::Simple, &["task"], 3, false, 8_000);
    let run = RunContext::for_config(RunId::new("successful-child-retry"), Arc::new(cfg.clone()));
    let outcome = run_pipeline_with_context(
        &cfg,
        &binding,
        "retry the same delegated child after a limited response",
        true,
        None,
        None,
        None,
        None,
        run,
    )
    .await
    .expect("child retry pipeline");
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .expect("child retry server timeout")
        .expect("child retry server task");
    assert_eq!(outcome.iterations, 5);
    assert_eq!(outcome.status, "ok", "retry outcome: {:?}", outcome.error);
    assert_eq!(requests.lock().expect("child retry requests").len(), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "release E2E: requires Node and Chrome/Chromium"]
async fn browser_game_agent_repair_e2e() {
    assert!(
        std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success()),
        "Node is required"
    );
    assert!(
        rafikx::quality::browser::detect_browser().is_some(),
        "Chrome or Chromium is required"
    );

    let workspace = TestWorkspace::new("browser-game-agent-e2e");
    let (base_url, requests, server) = start_scripted_game_model().await;
    let cfg = scripted_config(workspace.path(), base_url, 32_000);
    let binding = scripted_binding(
        TaskClass::Dev,
        &["todo_write", "write_file"],
        10,
        true,
        32_000,
    );
    let run = RunContext::for_config(RunId::new("browser-game-agent-e2e"), Arc::new(cfg.clone()));
    let outcome = run_pipeline_with_context(
        &cfg,
        &binding,
        "create a Super Mario browser game and verify every state",
        true,
        None,
        None,
        None,
        None,
        run,
    )
    .await
    .expect("agent pipeline");
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .expect("scripted server timeout")
        .expect("scripted server task");

    assert_eq!(outcome.status, "ok", "pipeline outcome: {:?}", outcome.error);
    assert!(outcome.verify_recovered.is_some(), "repair evidence missing");
    let mut changed = outcome.changed_files.clone();
    changed.sort();
    assert_eq!(changed, ["game.js", "index.html", "style.css"]);
    let captured = requests.lock().expect("request log");
    assert_eq!(captured.len(), 5);
    assert!(
        captured[0].contains("rafikx-browser-game-contract")
            && captured[0].contains("__rafikxGameTest"),
        "browser game contract was not sent to the coding agent"
    );
    assert!(
        captured.iter().any(|request| {
            request.contains("품질 게이트가 실패했습니다")
                && (request.contains("missingGameLoop")
                    || request.contains("ReferenceError")
                    || request.contains("browser smoke")
                    || request.contains("브라우저"))
        }),
        "quality repair feedback was not sent"
    );
    drop(captured);

    let report = run_quality_gate(
        workspace.path(),
        &["index.html".into(), "style.css".into(), "game.js".into()],
    )
    .await;
    assert!(report.passed, "final browser gate: {:?}", report.findings);
}
