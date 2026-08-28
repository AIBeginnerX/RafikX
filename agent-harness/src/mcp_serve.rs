//! MCP 서버 모드 — facts 메모리(remember/recall/forget/list)를 MCP 프로토콜로 노출한다.
//!
//! `rafikx mcp-serve` 는 stdio NDJSON(줄당 JSON 하나)으로 MCP 를 말한다.
//! MCP 클라이언트(Claude Code, Codex, Cursor 등)의 config 에
//! {"command": "rafikx", "args": ["mcp-serve"]} 로 등록하면 다른 도구·에이전트가
//! 같은 기억을 공유한다. 워크스페이스는 서버 프로세스의 현재 디렉터리다.

use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::db::Db;

const PROTOCOL_VERSION: &str = "2025-06-18";

/// 도구 명세 — tools/facts.rs 의 도구와 같은 계약이다.
fn tool_specs() -> Value {
    json!([
        {
            "name": "remember",
            "description": "사용자·프로젝트의 지속 사실(스택, 선호, 관습, 환경)을 기억한다.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "value": {"type": "string"},
                    "kind": {"type": "string", "enum": ["stack", "preference", "convention", "env", "goal", "other"]},
                    "global": {"type": "boolean"}
                },
                "required": ["key", "value"]
            }
        },
        {
            "name": "recall",
            "description": "기억한 지속 사실을 검색한다 (query 비우면 전체 목록).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "kind": {"type": "string"}
                }
            }
        },
        {
            "name": "forget",
            "description": "지속 사실을 삭제한다.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "global": {"type": "boolean"}
                },
                "required": ["key"]
            }
        },
        {
            "name": "facts_list",
            "description": "전역+프로젝트 지속 사실 전체 목록.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ])
}

fn text_result(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": false})
}

/// 도구 호출 — MCP 측은 승인 채널이 없으므로 forget 도 승인 없이 실행된다는 점에 주의
/// (MCP 서버를 등록한 사용자 자신의 명시적 신뢰 경계다).
pub fn call_tool(db: &Db, workspace: &Path, name: &str, args: &Value) -> Result<Value> {
    let scope = if args.get("global").and_then(Value::as_bool).unwrap_or(false) {
        None
    } else {
        Some(workspace)
    };
    match name {
        "remember" => {
            let key = args.get("key").and_then(Value::as_str).unwrap_or("").trim();
            let value = args.get("value").and_then(Value::as_str).unwrap_or("").trim();
            if key.is_empty() || value.is_empty() {
                return Ok(text_result("key와 value가 필요합니다.".into()));
            }
            let kind = args.get("kind").and_then(Value::as_str).unwrap_or("other");
            let write = db.upsert_fact(scope, kind, key, value, "mcp")?;
            let verb = match write {
                crate::db::FactWrite::Inserted { .. } => "기록했습니다",
                crate::db::FactWrite::Updated { .. } => "갱신했습니다",
            };
            Ok(text_result(format!("{verb}: {key} = {value}")))
        }
        "recall" => {
            let query = args.get("query").and_then(Value::as_str).unwrap_or("");
            let kind = args.get("kind").and_then(Value::as_str);
            let rows = db.recall_facts(scope, query, kind, 10)?;
            if rows.is_empty() {
                return Ok(text_result("기억하는 사실이 없습니다.".into()));
            }
            let body = rows
                .iter()
                .map(|r| format!("- ({}) {}: {}", r.kind, r.key, r.value))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(text_result(body))
        }
        "forget" => {
            let key = args.get("key").and_then(Value::as_str).unwrap_or("").trim();
            if key.is_empty() {
                return Ok(text_result("key가 필요합니다.".into()));
            }
            match db.forget_fact(scope, key)? {
                Some(row) => Ok(text_result(format!("삭제했습니다: {} = {}", row.key, row.value))),
                None => Ok(text_result(format!("해당 키를 찾지 못했습니다: {key}"))),
            }
        }
        "facts_list" => {
            let rows = db.list_facts(scope)?;
            if rows.is_empty() {
                return Ok(text_result("기억하는 사실이 없습니다.".into()));
            }
            let body = rows
                .iter()
                .map(|r| {
                    let scope = if r.project_id.is_empty() { "전역" } else { "프로젝트" };
                    format!("- ({}·{}) {}: {}", r.kind, scope, r.key, r.value)
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(text_result(body))
        }
        _ => Ok(json!({
            "content": [{"type": "text", "text": format!("알 수 없는 도구: {name}")}],
            "isError": true
        })),
    }
}

/// 요청 한 건 처리 — 알림(id 없음)은 None 을 돌려 응답하지 않는다.
pub fn handle(request: &Value, db: &Db, workspace: &Path) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str)?;
    let id = request.get("id").cloned();
    match (method, id) {
        ("initialize", Some(id)) => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "rafikx-facts", "version": env!("CARGO_PKG_VERSION")}
            }
        })),
        ("ping", Some(id)) => Some(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
        ("tools/list", Some(id)) => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": tool_specs()}
        })),
        ("tools/call", Some(id)) => {
            let params = request.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let result = match call_tool(db, workspace, name, &args) {
                Ok(v) => v,
                Err(e) => json!({
                    "content": [{"type": "text", "text": format!("오류: {e:#}")}],
                    "isError": true
                }),
            };
            Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        }
        // 알림·미지원 메서드는 조용히 무시 (MCP 관례)
        _ => None,
    }
}

/// stdio NDJSON 루프 — 줄당 JSON-RPC 한 건.
pub async fn stdio() -> Result<()> {
    let db = Db::open(&Db::db_path()?)?;
    let workspace = std::env::current_dir()?;
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(response) = handle(&request, &db, &workspace) {
            let mut body = serde_json::to_string(&response)?;
            body.push('\n');
            stdout.write_all(body.as_bytes()).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(tag: &str) -> (std::path::PathBuf, Db, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("rafikx-mcp-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db")).unwrap();
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        (dir, db, ws)
    }

    #[test]
    fn initialize_lists_protocol_and_tools() {
        let (dir, db, ws) = temp_db("init");
        let init = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"}), &db, &ws).unwrap();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        let list = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), &db, &ws).unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remember_recall_forget_roundtrip_over_mcp() {
        let (dir, db, ws) = temp_db("roundtrip");
        let remember = handle(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"remember","arguments":{"key":"test-runner","value":"pytest","kind":"stack"}}}),
            &db, &ws,
        ).unwrap();
        assert!(remember["result"]["content"][0]["text"].as_str().unwrap().contains("기록했습니다"));

        let recall = handle(
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"recall","arguments":{"query":"pytest"}}}),
            &db, &ws,
        ).unwrap();
        assert!(recall["result"]["content"][0]["text"].as_str().unwrap().contains("pytest"));

        let forget = handle(
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"forget","arguments":{"key":"test-runner"}}}),
            &db, &ws,
        ).unwrap();
        assert!(forget["result"]["content"][0]["text"].as_str().unwrap().contains("삭제했습니다"));

        let after = handle(
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"recall","arguments":{"query":"pytest"}}}),
            &db, &ws,
        ).unwrap();
        assert!(after["result"]["content"][0]["text"].as_str().unwrap().contains("없습니다"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn notifications_are_not_answered() {
        let (dir, db, ws) = temp_db("notify");
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}), &db, &ws).is_none());
        assert!(handle(&json!({"jsonrpc":"2.0","method":"resources/list","id":9}), &db, &ws).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_tool_is_error_result() {
        let (dir, db, ws) = temp_db("unknown");
        let res = handle(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope","arguments":{}}}),
            &db, &ws,
        ).unwrap();
        assert_eq!(res["result"]["isError"], true);
        let _ = std::fs::remove_dir_all(dir);
    }
}
