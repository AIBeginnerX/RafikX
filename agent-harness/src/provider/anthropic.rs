use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{
    ChatRequest, ChatResponse, ContentBlock, LimitHint, Message, Role, StopReason, limit_hint,
    map_stop_reason, rate_limit_error,
};

const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    client: reqwest::Client,
    token: String,
    oauth: bool,
}

/// 멀티바이트(한글 3바이트)가 청크 경계에서 잘려도 깨지지 않게
/// 바이트 버퍼에서 빈 줄("\n\n") 위치까지만 안전하게 잘라낸다.
fn drain_event(buf: &mut Vec<u8>) -> Option<String> {
    let pos = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| {
            buf.windows(2)
                .position(|w| w == b"\n\n")
                .map(|i| (i, 2))
        })?;
    let (idx, sep_len) = pos;
    let event_bytes: Vec<u8> = buf.drain(..idx + sep_len).collect();
    Some(String::from_utf8_lossy(&event_bytes).into_owned())
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Result<Self> {
        Self::create(api_key, false)
    }

    pub fn with_oauth(token: String) -> Result<Self> {
        Self::create(token, true)
    }

    fn create(token: String, oauth: bool) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("HTTP 클라이언트를 만들 수 없습니다")?;
        Ok(Self {
            client,
            token,
            oauth,
        })
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.oauth {
            req.header("Authorization", format!("Bearer {}", self.token))
                .header("anthropic-beta", "oauth-2025-04-20")
        } else {
            req.header("x-api-key", &self.token)
        }
    }

    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let body = build_body(req);
        let resp = self
            .apply_auth(
                self.client
                    .post(MESSAGES_URL)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json"),
            )
            .json(&body)
            .send()
            .await
            .context("Anthropic API 요청에 실패했습니다")?;

        let status = resp.status();
        let hint = limit_hint(resp.headers());
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &text, &hint));
        }
        let mut parsed = parse_message_json(&text)?;
        parsed.limit = hint;
        Ok(parsed)
    }

    pub async fn chat_stream<F>(&self, req: &ChatRequest, mut on_text: F) -> Result<ChatResponse>
    where
        F: FnMut(&str),
    {
        let body = build_body(req);
        let resp = self
            .apply_auth(
                self.client
                    .post(MESSAGES_URL)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json"),
            )
            .json(&body)
            .send()
            .await
            .context("Anthropic API 요청에 실패했습니다")?;

        let status = resp.status();
        let hint = limit_hint(resp.headers());
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(api_error(status.as_u16(), &text, &hint));
        }

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut full_text = String::new();
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;
        let mut stop_reason = StopReason::Other;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("응답 스트림이 중간에 끊겼습니다")?;
            buf.extend_from_slice(&chunk);
            while let Some(event) = drain_event(&mut buf) {
                if let Some(data) = sse_data(&event) {
                    if data.trim() == "[DONE]" {
                        continue;
                    }
                    let v: Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    match v.get("type").and_then(|t| t.as_str()) {
                        Some("message_start") => {
                            input_tokens = v
                                .pointer("/message/usage/input_tokens")
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0) as u32;
                        }
                        Some("content_block_delta") => {
                            if v.pointer("/delta/type").and_then(|x| x.as_str()) == Some("text_delta") {
                                if let Some(piece) = v.pointer("/delta/text").and_then(|x| x.as_str()) {
                                    if !piece.is_empty() {
                                        on_text(piece);
                                        full_text.push_str(piece);
                                    }
                                }
                            }
                        }
                        Some("message_delta") => {
                            stop_reason = map_stop_reason(
                                v.pointer("/delta/stop_reason").and_then(|x| x.as_str()),
                            );
                            if let Some(n) = v.pointer("/usage/output_tokens").and_then(|x| x.as_u64()) {
                                output_tokens = n as u32;
                            }
                        }
                        Some("message_stop") => {
                            return Ok(ChatResponse {
                                content: vec![ContentBlock::Text { text: full_text }],
                                stop_reason,
                                input_tokens,
                                output_tokens,
                                cached_tokens: crate::provider::cached_tokens_from(&v),
                                limit: hint.clone(),
                            });
                        }
                        Some("error") => {
                            let msg = v
                                .pointer("/error/message")
                                .and_then(|x| x.as_str())
                                .unwrap_or("스트림 오류");
                            return Err(anyhow!("Anthropic API 오류: {}", redact_secrets(msg)));
                        }
                        _ => {}
                    }
                }
            }
        }

        // message_stop 이 오면 위에서 반환된다 — EOF 도달은 미완료 응답이다.
        if full_text.is_empty() && input_tokens == 0 {
            anyhow::bail!("응답이 시작되기 전에 Anthropic 스트림이 종료되었습니다");
        }
        anyhow::bail!(
            "응답이 완료되기 전에 Anthropic 스트림이 종료되었습니다 ({}자 수신)",
            full_text.chars().count()
        );
    }
}

fn build_body(req: &ChatRequest) -> Value {
    // 프롬프트 캐시: 시스템 프롬프트(기본+교훈+프로파일 지침)는 매 반복 동일하므로
    // cache_control 을 붙여 입력 토큰 비용을 크게 줄인다. 실패해도 API 가 무시하지 않으므로
    // 구형 호환 엔드포인트를 위한 설정 스위치는 환경변수로만 끈다.
    let caching = std::env::var("RAFIKX_NO_PROMPT_CACHE")
        .map(|v| v.is_empty() || v == "0")
        .unwrap_or(true);
    let system: Value = if caching && !req.system.is_empty() {
        json!([{
            "type": "text",
            "text": req.system,
            "cache_control": {"type": "ephemeral"}
        }])
    } else {
        json!(req.system)
    };
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "system": system,
        "messages": to_api_messages(&req.messages),
        "stream": req.stream,
    });
    if !req.tools.is_empty() {
        let mut tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        // 도구 목록도 시스템 다음 캐시 경계 — 마지막 도구에 두 번째 브레이크포인트.
        if caching {
            if let Some(last) = tools.last_mut() {
                last["cache_control"] = json!({"type": "ephemeral"});
            }
        }
        body["tools"] = Value::Array(tools);
    }
    body
}

fn to_api_messages(msgs: &[Message]) -> Vec<Value> {
    msgs.iter()
        .filter_map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => return None,
            };
            let content: Vec<Value> = m
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => json!({"type": "text", "text": text}),
                    ContentBlock::ToolUse { id, name, input } => {
                        json!({"type": "tool_use", "id": id, "name": name, "input": input})
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                        "is_error": is_error,
                    }),
                })
                .collect();
            Some(json!({"role": role, "content": content}))
        })
        .collect()
}

fn parse_message_json(text: &str) -> Result<ChatResponse> {
    let v: Value = serde_json::from_str(text).context("Anthropic 응답 JSON을 해석할 수 없습니다")?;
    let mut blocks = Vec::new();
    if let Some(arr) = v.get("content").and_then(|c| c.as_array()) {
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    blocks.push(ContentBlock::Text { text: t.to_string() });
                }
            } else if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                let id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = item.get("input").cloned().unwrap_or_else(|| json!({}));
                blocks.push(ContentBlock::ToolUse { id, name, input });
            }
        }
    }
    Ok(ChatResponse {
        content: blocks,
        stop_reason: map_stop_reason(v.get("stop_reason").and_then(|s| s.as_str())),
        input_tokens: v
            .pointer("/usage/input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        output_tokens: v
            .pointer("/usage/output_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        cached_tokens: crate::provider::cached_tokens_from(&v),
        limit: LimitHint::default(),
    })
}

fn sse_data(event: &str) -> Option<&str> {
    let mut data_lines = Vec::new();
    for line in event.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
    }
    if data_lines.is_empty() {
        None
    } else {
        // 여러 data: 줄을 합치되, 실제로는 한 줄 JSON.
        Some(data_lines[0])
    }
}

fn api_error(status: u16, body: &str, hint: &LimitHint) -> anyhow::Error {
    let safe = redact_secrets(body);
    if status == 429 {
        return rate_limit_error(status, &safe, hint);
    }
    let snippet: String = safe.chars().take(400).collect();
    if status == 401 {
        anyhow!("Anthropic API 인증 실패 (HTTP 401). API 키를 확인하세요.")
    } else {
        anyhow!("Anthropic API 오류 HTTP {status}: {snippet}")
    }
}

fn redact_secrets(s: &str) -> String {
    let mut out = s.to_string();
    let prefix = "sk-ant-";
    while let Some(i) = out.find(prefix) {
        let rest = &out[i + prefix.len()..];
        let n = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(rest.len());
        out.replace_range(i..i + prefix.len() + n, "[redacted]");
    }
    out
}
