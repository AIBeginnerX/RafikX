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
                    "LSP server '{}' could not start: {error}. Install it or configure the RAFIKX_LSP_* environment variable; RafikX never auto-installs language servers.",
                    spec.program
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
        "py" => Ok(ServerSpec {
            program: env_or("RAFIKX_LSP_PYTHON", "pyright-langserver"),
            args: vec!["--stdio"],
            language_id: "python",
            wait_for_status: false,
        }),
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

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}
