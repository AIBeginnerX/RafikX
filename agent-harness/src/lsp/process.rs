use std::path::Path;
use std::process::Stdio;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::{framing, protocol};

pub struct ServerSpec {
    pub program: String,
    pub args: Vec<&'static str>,
    pub language_id: &'static str,
    pub wait_for_status: bool,
}

pub struct LspProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl LspProcess {
    pub async fn start(spec: &ServerSpec, workspace: &Path) -> Result<Self> {
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                anyhow!(
                    "LSP 서버 '{}' 를 시작하지 못했습니다: {error}. 설치: {}. 또는 RAFIKX_LSP_* 환경변수로 경로를 지정하세요. RafikX는 언어 서버를 자동 설치하지 않습니다.",
                    spec.program,
                    install_hint(&spec.program)
                )
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("LSP server stdin is unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("LSP server stdout is unavailable"))?;
        let mut process = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };
        process
            .request(
                "initialize",
                protocol::initialize(workspace, std::process::id()),
            )
            .await?;
        process.notify("initialized", json!({})).await?;
        Ok(process)
    }

    pub async fn open(&mut self, path: &Path, language_id: &str, text: &str) -> Result<()> {
        self.notify(
            "textDocument/didOpen",
            protocol::text_document(path, language_id, text),
        )
        .await
    }

    pub async fn wait_until_ready(&mut self) -> Result<()> {
        loop {
            let message = self.read_message().await?;
            if message.get("method").and_then(Value::as_str) != Some("experimental/serverStatus") {
                continue;
            }
            let params = &message["params"];
            if params["quiescent"].as_bool() != Some(true) {
                continue;
            }
            let health = params["health"].as_str().unwrap_or("unknown");
            if health == "ok" {
                return Ok(());
            }
            let detail = params["message"].as_str().unwrap_or("no detail provided");
            return Err(anyhow!(
                "LSP server is ready with health '{health}': {detail}"
            ));
        }
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        framing::write_message(&mut self.stdin, &request).await?;
        loop {
            let message = self.read_message().await?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(anyhow!("LSP {method} failed: {error}"));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    /// push 모델 진단 수집 — pull(textDocument/diagnostic)을 지원하지 않는 서버
    /// (pylsp·typescript-language-server 등)는 open 직후 publishDiagnostics 알림으로
    /// 진단을 보낸다. 해당 uri 의 첫 진단 세트가 올 때까지, 최대 wait_ms 만큼 읽는다.
    pub async fn collect_diagnostics(&mut self, uri: &str, wait_ms: u64) -> Result<Value> {
        // 서버가 초기 파싱 직후 빈 진단 세트를 먼저 보내고 린트 완료 후 다시 보내는
        // 경우가 있다 (pylsp 실측) — 창 전체를 읽고 마지막 세트를 채택한다.
        let mut last: Option<Value> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Ok(last.unwrap_or(Value::Null));
            }
            match tokio::time::timeout(remaining, self.read_message()).await {
                Ok(Ok(message)) => {
                    let is_diag = message.get("method").and_then(Value::as_str)
                        == Some("textDocument/publishDiagnostics");
                    let same_file = message
                        .pointer("/params/uri")
                        .and_then(Value::as_str)
                        == Some(uri);
                    if is_diag && same_file {
                        last = Some(
                            message
                                .pointer("/params/diagnostics")
                                .cloned()
                                .unwrap_or(Value::Null),
                        );
                    }
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => return Ok(last.unwrap_or(Value::Null)),
            }
        }
    }

    pub async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let notification = json!({"jsonrpc":"2.0","method":method,"params":params});
        framing::write_message(&mut self.stdin, &notification).await
    }

    pub async fn shutdown(mut self) {
        let _ = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;
        if tokio::time::timeout(std::time::Duration::from_secs(1), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.start_kill();
        }
    }

    async fn respond_to_server(&mut self, message: &Value) -> Result<()> {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let result =
            if message.get("method").and_then(Value::as_str) == Some("workspace/configuration") {
                json!([])
            } else {
                Value::Null
            };
        framing::write_message(
            &mut self.stdin,
            &json!({"jsonrpc":"2.0","id":id,"result":result}),
        )
        .await
    }

    async fn read_message(&mut self) -> Result<Value> {
        loop {
            let message = framing::read_message(&mut self.stdout).await?;
            if message.get("method").is_some() && message.get("id").is_some() {
                self.respond_to_server(&message).await?;
                continue;
            }
            return Ok(message);
        }
    }
}

pub fn server_for(path: &Path) -> Result<ServerSpec> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    match extension {
        "rs" => Ok(ServerSpec {
            program: env_or("RAFIKX_LSP_RUST", "rust-analyzer"),
            args: Vec::new(),
            language_id: "rust",
            wait_for_status: true,
        }),
        "py" => {
            let program = python_program();
            let args = python_args_for(&program);
            Ok(ServerSpec {
                program,
                args,
                language_id: "python",
                wait_for_status: false,
            })
        }
        "ts" | "tsx" | "js" | "jsx" => Ok(ServerSpec {
            program: env_or("RAFIKX_LSP_TYPESCRIPT", "typescript-language-server"),
            args: vec!["--stdio"],
            language_id: if matches!(extension, "ts" | "tsx") {
                "typescript"
            } else {
                "javascript"
            },
            wait_for_status: false,
        }),
        "go" => Ok(ServerSpec {
            program: env_or("RAFIKX_LSP_GO", "gopls"),
            args: Vec::new(),
            language_id: "go",
            wait_for_status: false,
        }),
        _ => Err(anyhow!(
            "no LSP server mapping for extension '.{extension}'"
        )),
    }
}

/// Python LSP 서버 선택 — RAFIKX_LSP_PYTHON 우선, 없으면 PATH 에서
/// pyright-langserver → pylsp 순으로 찾는다 (둘 다 없으면 기본값을 돌려
/// 기동 실패 시 설치 안내가 나가게 한다).
fn python_program() -> String {
    if let Ok(v) = std::env::var("RAFIKX_LSP_PYTHON")
        && !v.trim().is_empty()
    {
        return v;
    }
    for cand in ["pyright-langserver", "pylsp"] {
        if on_path(cand) {
            return cand.to_string();
        }
    }
    "pyright-langserver".into()
}

/// 서버별 stdio 인자 — pylsp 은 플래그 없이 stdio, pyright 는 --stdio 필요.
fn python_args_for(program: &str) -> Vec<&'static str> {
    if program.ends_with("pylsp") {
        Vec::new()
    } else {
        vec!["--stdio"]
    }
}

/// PATH 탐색 (Windows 셈 고려).
fn on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        ["", ".exe", ".cmd", ".bat"].iter().any(|ext| {
            let path = if ext.is_empty() {
                dir.join(name)
            } else {
                dir.join(format!("{name}{ext}"))
            };
            path.is_file()
        })
    })
}

/// 기동 실패 안내에 붙이는 언어별 설치 명령.
fn install_hint(program: &str) -> &'static str {
    match program {
        "rust-analyzer" => "rustup component add rust-analyzer",
        "pyright-langserver" => "npm install -g pyright  (또는 pip install python-lsp-server)",
        "pylsp" => "pip install python-lsp-server",
        "typescript-language-server" => "npm install -g typescript typescript-language-server",
        "gopls" => "go install golang.org/x/tools/gopls@latest",
        _ => "해당 언어 서버를 설치하세요",
    }
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod python_server_tests {
    use super::*;

    #[test]
    fn pylsp_uses_plain_stdio() {
        assert!(python_args_for("pylsp").is_empty());
        assert!(python_args_for("/opt/venv/bin/pylsp").is_empty());
    }

    #[test]
    fn pyright_needs_stdio_flag() {
        assert_eq!(python_args_for("pyright-langserver"), vec!["--stdio"]);
    }

    #[test]
    fn on_path_finds_coreutils() {
        assert!(on_path("ls"));
        assert!(!on_path("definitely-not-a-real-binary-xyz"));
    }

    #[test]
    fn env_override_wins() {
        unsafe { std::env::set_var("RAFIKX_LSP_PYTHON", "/custom/mypylsp") };
        assert_eq!(python_program(), "/custom/mypylsp");
        unsafe { std::env::remove_var("RAFIKX_LSP_PYTHON") };
    }
}
