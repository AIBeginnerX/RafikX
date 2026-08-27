//! MCP(Model Context Protocol) stdio 클라이언트.
//!
//! 설정(`~/.rafikx/config.toml`):
//! ```toml
//! [mcp_servers.filesystem]
//! command = "npx"
//! args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
//!
//! [mcp_servers.github]
//! command = "mcp-server-github"
//! env = { GITHUB_TOKEN = "…" }
//! ```
//!
//! 각 서버 프로세스는 부팅 때 spawn 되고 줄 단위 NDJSON JSON-RPC 2.0으로 통신한다.
//! 응답 수신은 서버별 전용 스레드가 맡고, 요청 대기는 recv_timeout 기반 동기라서
//! 어떤 실행 컨텍스트에서도 호출할 수 있다. 연결 실패한 서버는 로그만 남기고
//! 건너뛴다(Harness 전체 부팅을 막지 않는다).

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::tools::{Tool, ToolCtx};

/// 하나의 MCP 서버 연결 설정.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct McpServerCfg {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

const INIT_TIMEOUT: Duration = Duration::from_secs(12);
const LIST_TIMEOUT: Duration = Duration::from_secs(12);
const CALL_TIMEOUT: Duration = Duration::from_secs(75);

struct McpClientInner {
    stdin: Mutex<std::process::ChildStdin>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, std::sync::mpsc::Sender<Value>>>,
    server_info: Mutex<Value>,
}

/// 살아있는 MCP 서버 프로세스 하나.
pub struct McpClient {
    inner: Arc<McpClientInner>,
    _child: Mutex<Child>,
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Ok(mut child) = self._child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 서버 프로세스에 넘길 최소 시스템 환경 키.
fn base_env_keys() -> &'static [&'static str] {
    &["PATH", "HOME", "USER", "LOGNAME", "LANG", "TMPDIR"]
}

impl McpClient {
    fn spawn(cfg: &McpServerCfg) -> Result<Arc<Self>> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for k in base_env_keys() {
            if let Ok(v) = std::env::var(k) {
                cmd.env(k, v);
            }
        }
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("stdin 없음"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("stdout 없음"))?;

        let inner = Arc::new(McpClientInner {
            stdin: Mutex::new(stdin),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            server_info: Mutex::new(Value::Null),
        });

        {
            let inner = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("mcp-reader".into())
                .spawn(move || reader_loop(inner, BufReader::new(stdout)))?;
        }

        let cli = Arc::new(Self {
            inner,
            _child: Mutex::new(child),
        });
        let info = cli.request(
            "initialize",
            json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "rafikx-harness", "version": env!("CARGO_PKG_VERSION") }
            }),
            INIT_TIMEOUT,
        )?;
        *cli.inner.server_info.lock().unwrap() =
            info.get("serverInfo").cloned().unwrap_or(Value::Null);
        cli.notify("notifications/initialized")?;
        Ok(cli)
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);

        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        {
            let mut w = self.inner.stdin.lock().unwrap();
            writeln!(w, "{frame}")?;
            w.flush()?;
        }
        match rx.recv_timeout(timeout) {
            Ok(v) => {
                if let Some(err) = v.get("error") {
                    bail!(
                        "MCP {} 오류: {}",
                        method,
                        err.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("알 수 없는 오류")
                    );
                }
                Ok(v.get("result").cloned().unwrap_or(Value::Null))
            }
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                bail!("MCP {} 시간 초과({:?})", method, timeout);
            }
        }
    }

    /// 알림(응답 없음).
    fn notify(&self, method: &str) -> Result<()> {
        let line = json!({"jsonrpc": "2.0", "method": method});
        let mut w = self.inner.stdin.lock().unwrap();
        writeln!(w, "{line}")?;
        w.flush()?;
        Ok(())
    }

    fn list_tools(&self) -> Result<Vec<(String, String, Value)>> {
        let res = self.request("tools/list", json!({}), LIST_TIMEOUT)?;
        let arr = res
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(arr
            .into_iter()
            .filter_map(|t| {
                Some((
                    t.get("name")?.as_str()?.to_string(),
                    t.get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    t.get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object"})),
                ))
            })
            .collect())
    }

    fn call_tool(&self, name: &str, arguments: Value) -> Result<String> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        });
        {
            let mut w = self.inner.stdin.lock().unwrap();
            writeln!(w, "{frame}")?;
            w.flush()?;
        }
        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(v) => {
                if let Some(err) = v.get("error") {
                    bail!(
                        "mcp_call 오류: {}",
                        err.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("알 수 없는 오류")
                    );
                }
                let res = v.get("result").cloned().unwrap_or(Value::Null);
                if res.get("isError").and_then(Value::as_bool).unwrap_or(false) {
                    bail!(content_text(&res).unwrap_or_else(|| "도구 실행 오류".into()));
                }
                content_text(&res)
                    .or_else(|| content_structured(&res))
                    .ok_or_else(|| anyhow!("빈 결과"))
            }
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                bail!("mcp_call 시간 초과({CALL_TIMEOUT:?})")
            }
        }
    }
}

fn content_text(res: &Value) -> Option<String> {
    let arr = res.get("content")?.as_array()?;
    let texts: Vec<&str> = arr
        .iter()
        .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|c| c.get("text").and_then(Value::as_str))
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

fn content_structured(res: &Value) -> Option<String> {
    res.get("structuredContent").map(|v| v.to_string())
}

fn reader_loop(inner: Arc<McpClientInner>, rd: BufReader<std::process::ChildStdout>) {
    for line in rd.lines().map_while(Result::ok) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(t) else {
            continue;
        };
        let Some(id) = v.get("id").and_then(Value::as_u64) else {
            continue;
        };
        if let Some(tx) = inner.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(v);
        }
    }
}

// ── Hub --------------------------------------------------------------------

/// 외부 도구 정의 한 개.
#[derive(Debug, Clone)]
pub struct McpToolDef {
    /// 서버가 알려준 원래 이름.
    pub name: String,
    pub description: String,
    pub schema: Value,
}

impl McpToolDef {
    /// Harness 내부 식별자(모델에게 노출되는 이름).
    pub fn qualified(&self, server: &str) -> String {
        format!("mcp__{server}__{}", self.name)
    }
}

/// 연결된 MCP 서버 묶음. 글로벌 1개만 존재한다.
pub struct McpHub {
    pub clients: BTreeMap<String, Arc<McpClient>>,
    /// (server, def) 목록.
    pub tools: Vec<(String, McpToolDef)>,
}

impl McpHub {
    pub fn empty() -> Self {
        Self {
            clients: BTreeMap::new(),
            tools: Vec::new(),
        }
    }
}

static HUB: std::sync::OnceLock<Arc<McpHub>> = std::sync::OnceLock::new();

/// 기본 설정 파일(~/.rafikx/config.toml)에서 MCP 서버를 읽어 초기화한다.
/// 부팅 맨 앞에서 1회 호출하면 된다. 실패해도 조용히 빈 Hub로 진행한다.
pub fn init_default() {
    if HUB.get().is_some() {
        return;
    }
    let cfgs: HashMap<String, McpServerCfg> = (|| {
        let home = dirs::home_dir()?;
        let raw = std::fs::read_to_string(home.join(".rafikx").join("config.toml")).ok()?;
        let file: crate::config::ConfigFile = toml::from_str(&raw).ok()?;
        Some(file.mcp_servers)
    })()
    .unwrap_or_default();
    init_from_config(&cfgs);
}

/// 부팅 시 1회 호출: 설정된 서버들을 연결하고 도구 목록을 캐시한다.
/// 이미 초기화돼 있으면 아무것도 하지 않는다.
pub fn init_from_config(cfgs: &HashMap<String, McpServerCfg>) {
    HUB.get_or_init(|| {
        if cfgs.is_empty() {
            return Arc::new(McpHub::empty());
        }
        let mut clients = BTreeMap::new();
        let mut tools = Vec::new();
        for (name, cfg) in cfgs {
            match McpClient::spawn(cfg).and_then(|cli| {
                let defs = cli.list_tools()?;
                Ok((cli, defs))
            }) {
                Ok((cli, defs)) => {
                    eprintln!("[Harness] MCP 서버 '{name}' 연결: 도구 {}개", defs.len());
                    for (tn, td, ts) in defs {
                        tools.push((
                            name.clone(),
                            McpToolDef {
                                name: tn,
                                description: td,
                                schema: ts,
                            },
                        ));
                    }
                    clients.insert(name.clone(), cli);
                }
                Err(e) => eprintln!("[Harness] MCP '{name}' 연결 실패(건너뜀): {e:#}"),
            }
        }
        Arc::new(McpHub { clients, tools })
    });
}

/// 글로벌 Hub(초기화 전이면 빈 Hub).
pub fn global() -> &'static Arc<McpHub> {
    HUB.get_or_init(|| Arc::new(McpHub::empty()))
}

// ── Harness 도구 -----------------------------------------------------------

/// MCP 서버·도구 카탈로그 조회.
pub struct McpList;

impl Tool for McpList {
    fn name(&self) -> &'static str {
        "mcp_list"
    }

    fn description(&self) -> &'static str {
        "연결된 MCP 서버와 외부 도구 목록(호출명·설명·input schema)을 반환한다. 외부 서비스 연동 전 반드시 먼저 호출해 가능한 도구를 확인하라."
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn run(&self, _input: Value, _ctx: &ToolCtx) -> Result<String> {
        let hub = global();
        if hub.clients.is_empty() {
            return Ok(
                "연결된 MCP 서버가 없다. ~/.rafikx/config.toml 에 다음처럼 설정 후 재시작:\n\
                 [mcp_servers.<이름>]\ncommand = \"npx\"\nargs = [\"-y\", \"@modelcontextprotocol/server-<종류>\"]"
                    .into(),
            );
        }
        let mut out = String::from("MCP 서버:\n");
        for (name, cli) in &hub.clients {
            let info = cli.inner.server_info.lock().unwrap();
            out.push_str(&format!(
                "- {}: {}\n",
                name,
                info.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("(정보 없음)")
            ));
        }
        out.push_str("\n도구(mcp_call 의 tool 인자로 사용):\n");
        for (server, d) in &hub.tools {
            out.push_str(&format!(
                "- {}: {}\n  schema: {}\n",
                d.qualified(server),
                d.description.replace('\n', " "),
                d.schema
            ));
        }
        Ok(out)
    }
}

/// MCP 외부 도구 실행.
pub struct McpCall;

impl Tool for McpCall {
    fn name(&self) -> &'static str {
        "mcp_call"
    }

    fn description(&self) -> &'static str {
        "MCP 외부 도구를 실행한다. 인자: server(서버 이름), tool(mcp_list 의 도구 이름), arguments(JSON 객체 문자열; 스키마는 mcp_list 참조)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": {"type": "string", "description": "MCP 서버 이름"},
                "tool": {"type": "string", "description": "호출할 외부 도구 이름(mcp__<서버>__<도구>)"},
                "arguments": {"type": "string", "description": "도구 인자(JSON 객체 문자열)"}
            },
            "required": ["server", "tool"]
        })
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        true // 외부 시스템에 영향을 줄 수 있다
    }

    fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<String> {
        let server = input
            .get("server")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("'server' 문자열이 필요하다"))?;
        let tool = input
            .get("tool")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("'tool' 문자열이 필요하다"))?;
        let arguments: Value = match input.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(Value::String(s)) if s.trim().is_empty() => json!({}),
            Some(Value::String(s)) => serde_json::from_str(s)
                .map_err(|e| anyhow!("arguments 는 유효한 JSON 문자열이어야 한다: {e}"))?,
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => bail!("arguments 는 JSON 객체 문자열이어야 한다"),
        };

        let hub = global();
        let hit = hub
            .tools
            .iter()
            .find(|(s, d)| s == server && (d.qualified(server) == *tool || d.name == *tool));
        let (_, def) = hit.ok_or_else(|| {
            anyhow!(
                "MCP 도구 '{tool}' 를 서버 '{server}' 에서 못 찾았다. mcp_list 로 이름을 확인하라"
            )
        })?;
        let cli = hub
            .clients
            .get(server)
            .ok_or_else(|| anyhow!("서버 '{server}' 에 연결돼 있지 않다"))?;
        // blocking I/O — 호출 컨텍스트를 막지 않게 별도 스레드에서 실행.
        let cli = Arc::clone(cli);
        let name = def.name.clone();
        let handle = std::thread::Builder::new()
            .name("mcp-call".into())
            .spawn(move || cli.call_tool(&name, arguments))?;
        handle.join().map_err(|_| anyhow!("mcp_call 스레드 패닝"))?
    }
}
