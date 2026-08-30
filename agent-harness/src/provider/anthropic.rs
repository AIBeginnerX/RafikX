use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{
    ChatRequest, ChatResponse, ContentBlock, LimitHint, Message, Role, SemanticStreamEvent,
    StopReason, StreamEvent, limit_hint, map_stop_reason, rate_limit_error,
};

const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const TOOL_ARGS_STEP: usize = 8 * 1024;

pub struct AnthropicProvider {
    client: reqwest::Client,
    token: String,
    oauth: bool,
}

enum StreamingContentBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input_json: String,
        stopped: bool,
    },
}

#[derive(Default)]
struct AnthropicStreamAccumulator {
    blocks: BTreeMap<usize, StreamingContentBlock>,
    tool_args_marks: BTreeMap<usize, usize>,
}

impl AnthropicStreamAccumulator {
    fn content_block_start(&mut self, event: &Value) {
        let Some(index) = stream_index(event) else {
            return;
        };
        let Some(block) = event.get("content_block") else {
            return;
        };
        match block.get("type").and_then(|value| value.as_str()) {
            Some("text") => {
                self.blocks
                    .insert(index, StreamingContentBlock::Text(String::new()));
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                self.blocks.insert(
                    index,
                    StreamingContentBlock::ToolUse {
                        id,
                        name,
                        input_json: String::new(),
                        stopped: false,
                    },
                );
            }
            _ => {}
        }
    }

    fn text_delta(&mut self, event: &Value) {
        let Some(index) = stream_index(event) else {
            return;
        };
        let Some(piece) = event
            .pointer("/delta/text")
            .and_then(|value| value.as_str())
        else {
            return;
        };
        if piece.is_empty() {
            return;
        }
        match self.blocks.entry(index) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(StreamingContentBlock::Text(piece.to_string()));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if let StreamingContentBlock::Text(text) = entry.get_mut() {
                    text.push_str(piece);
                }
            }
        }
    }

    fn input_json_delta(&mut self, event: &Value) -> Option<(String, usize)> {
        let index = stream_index(event)?;
        let piece = event
            .pointer("/delta/partial_json")
            .and_then(|value| value.as_str())?;
        let StreamingContentBlock::ToolUse {
            name, input_json, ..
        } = self.blocks.get_mut(&index)?
        else {
            return None;
        };
        input_json.push_str(piece);
        let total_bytes = input_json.len();
        if tool_args_due(&mut self.tool_args_marks, index, total_bytes) {
            Some((name.clone(), total_bytes))
        } else {
            None
        }
    }

    fn content_block_stop(&mut self, event: &Value) {
        let Some(index) = stream_index(event) else {
            return;
        };
        if let Some(StreamingContentBlock::ToolUse { stopped, .. }) = self.blocks.get_mut(&index) {
            *stopped = true;
        }
    }

    fn finish(self, stop_reason: StopReason) -> Vec<ContentBlock> {
        let mut content = Vec::new();
        for block in self.blocks.into_values() {
            match block {
                StreamingContentBlock::Text(text) if !text.is_empty() => {
                    content.push(ContentBlock::Text { text });
                }
                StreamingContentBlock::ToolUse {
                    id,
                    name,
                    input_json,
                    stopped: true,
                } if stop_reason != StopReason::MaxTokens && !id.is_empty() && !name.is_empty() => {
                    let Ok(input) = serde_json::from_str::<Value>(&input_json) else {
                        continue;
                    };
                    if input.is_object() {
                        content.push(ContentBlock::ToolUse { id, name, input });
                    }
                }
                _ => {}
            }
        }
        content
    }
}

fn stream_index(event: &Value) -> Option<usize> {
    event
        .get("index")
        .and_then(|value| value.as_u64())
        .and_then(|index| usize::try_from(index).ok())
}

fn tool_args_due(marks: &mut BTreeMap<usize, usize>, index: usize, total_bytes: usize) -> bool {
    let mark = marks.entry(index).or_default();
    if total_bytes < TOOL_ARGS_STEP || total_bytes.saturating_sub(*mark) < TOOL_ARGS_STEP {
        return false;
    }
    *mark = total_bytes;
    true
}

/// 멀티바이트(한글 3바이트)가 청크 경계에서 잘려도 깨지지 않게
/// 바이트 버퍼에서 빈 줄("\n\n") 위치까지만 안전하게 잘라낸다.
fn drain_event(buf: &mut Vec<u8>) -> Option<String> {
    let pos = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))?;
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
        // 전체 timeout 은 긴 스트리밍 응답을 도중에 끊는다 — 연결·유휴에만 상한.
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .read_timeout(std::time::Duration::from_secs(180))
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
        if !status.is_success() {
            return Err(api_error(status.as_u16(), "", &hint));
        }
        let text = resp.text().await.unwrap_or_default();
        let mut parsed = parse_message_json(&text)?;
        parsed.limit = hint;
        Ok(parsed)
    }

    pub async fn chat_stream<F>(&self, req: &ChatRequest, mut on_event: F) -> Result<ChatResponse>
    where
        F: FnMut(StreamEvent),
    {
        self.chat_semantic_stream(req, |event| on_event(event.public()))
            .await
    }

    pub(crate) async fn chat_semantic_stream<F>(
        &self,
        req: &ChatRequest,
        mut on_event: F,
    ) -> Result<ChatResponse>
    where
        F: FnMut(SemanticStreamEvent),
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
            return Err(api_error(status.as_u16(), "", &hint));
        }

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut full_text = String::new();
        let mut stream_model = String::new();
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;
        let mut cached_tokens = 0u32;
        let mut cache_reported = false;
        let mut stop_reason = StopReason::Other;
        let mut thinking_started = false;
        let mut content = AnthropicStreamAccumulator::default();

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
                            if stream_model.is_empty()
                                && let Some(m) =
                                    v.pointer("/message/model").and_then(|x| x.as_str())
                            {
                                stream_model = m.to_string();
                            }
                            take_stream_input_usage(
                                &v,
                                &mut input_tokens,
                                &mut cached_tokens,
                                &mut cache_reported,
                            );
                        }
                        Some("content_block_start") => content.content_block_start(&v),
                        Some("content_block_delta") => {
                            if v.pointer("/delta/type").and_then(|x| x.as_str())
                                == Some("text_delta")
                            {
                                if let Some(piece) =
                                    v.pointer("/delta/text").and_then(|x| x.as_str())
                                    && !piece.is_empty()
                                {
                                    if thinking_started {
                                        on_event(SemanticStreamEvent::ReasoningText(
                                            "\n[/모델 작업]\n",
                                        ));
                                        thinking_started = false;
                                    }
                                    on_event(SemanticStreamEvent::ContentCandidate(piece));
                                    full_text.push_str(piece);
                                    content.text_delta(&v);
                                }
                            } else if v.pointer("/delta/type").and_then(|x| x.as_str())
                                == Some("thinking_delta")
                                && let Some(piece) =
                                    v.pointer("/delta/thinking").and_then(|x| x.as_str())
                                && !piece.is_empty()
                            {
                                if !thinking_started {
                                    on_event(SemanticStreamEvent::ReasoningText("\n[모델 작업]\n"));
                                    thinking_started = true;
                                }
                                on_event(SemanticStreamEvent::ReasoningText(piece));
                            } else if v.pointer("/delta/type").and_then(|x| x.as_str())
                                == Some("input_json_delta")
                                && let Some((name, total_bytes)) = content.input_json_delta(&v)
                            {
                                let name = if name.is_empty() { "tool" } else { &name };
                                on_event(SemanticStreamEvent::ToolArgs { name, total_bytes });
                            }
                        }
                        Some("content_block_stop") => content.content_block_stop(&v),
                        Some("message_delta") => {
                            stop_reason = map_stop_reason(
                                v.pointer("/delta/stop_reason").and_then(|x| x.as_str()),
                            );
                            if let Some(n) =
                                v.pointer("/usage/output_tokens").and_then(|x| x.as_u64())
                            {
                                output_tokens = crate::provider::saturating_token_count(n);
                            }
                        }
                        Some("message_stop") => {
                            if thinking_started {
                                on_event(SemanticStreamEvent::ReasoningText("\n[/모델 작업]\n"));
                            }
                            if let Some(stopped_cache) = crate::provider::cached_tokens_entry(&v) {
                                cached_tokens = stopped_cache;
                                cache_reported = true;
                            }
                            return Ok(ChatResponse {
                                model: stream_model.clone(),
                                content: content.finish(stop_reason),
                                stop_reason,
                                input_tokens,
                                output_tokens,
                                cached_tokens,
                                cache_reported,
                                limit: hint.clone(),
                            });
                        }
                        Some("error") => {
                            return Err(anyhow!("Anthropic API 스트림 오류"));
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
        if caching && let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({"type": "ephemeral"});
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
    let v: Value =
        serde_json::from_str(text).context("Anthropic 응답 JSON을 해석할 수 없습니다")?;
    let mut blocks = Vec::new();
    if let Some(arr) = v.get("content").and_then(|c| c.as_array()) {
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    blocks.push(ContentBlock::Text {
                        text: t.to_string(),
                    });
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
        model: v.get("model").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        content: blocks,
        stop_reason: map_stop_reason(v.get("stop_reason").and_then(|s| s.as_str())),
        input_tokens: v
            .pointer("/usage/input_tokens")
            .and_then(|x| x.as_u64())
            .map(crate::provider::saturating_token_count)
            .unwrap_or(0),
        output_tokens: v
            .pointer("/usage/output_tokens")
            .and_then(|x| x.as_u64())
            .map(crate::provider::saturating_token_count)
            .unwrap_or(0),
        cached_tokens: crate::provider::cached_tokens_from(&v),
        cache_reported: crate::provider::cached_tokens_entry(&v).is_some(),
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

fn take_stream_input_usage(
    v: &Value,
    input_tokens: &mut u32,
    cached_tokens: &mut u32,
    cache_reported: &mut bool,
) {
    if let Some(n) = v
        .pointer("/message/usage/input_tokens")
        .and_then(|x| x.as_u64())
    {
        *input_tokens = crate::provider::saturating_token_count(n);
    }
    let cached = v
        .pointer("/message/usage/cache_read_input_tokens")
        .or_else(|| v.pointer("/usage/cache_read_input_tokens"))
        .and_then(|x| x.as_u64());
    if let Some(cached) = cached {
        *cached_tokens = crate::provider::saturating_token_count(cached);
        *cache_reported = true;
    }
}

fn api_error(status: u16, _body: &str, hint: &LimitHint) -> anyhow::Error {
    if status == 429 {
        return rate_limit_error(status, "", hint);
    }
    if status == 401 {
        anyhow!("Anthropic API 인증 실패 (HTTP 401). API 키를 확인하세요.")
    } else {
        anyhow!("Anthropic API 오류 HTTP {status}")
    }
}

#[cfg(test)]
mod streaming_usage_tests {
    use super::*;

    #[test]
    fn stream_accumulator_keeps_interleaved_blocks_in_index_order() {
        let mut accumulator = AnthropicStreamAccumulator::default();
        accumulator.content_block_start(&json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text"}
        }));
        accumulator.text_delta(&json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "먼저 "}
        }));
        accumulator.content_block_start(&json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {"type": "tool_use", "id": "call-1", "name": "write_file"}
        }));
        for partial_json in [r#"{"path":"game.html", "#, r#""content":"<canvas/>"}"#] {
            accumulator.input_json_delta(&json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": partial_json}
            }));
        }
        accumulator.content_block_stop(&json!({"type": "content_block_stop", "index": 1}));
        accumulator.text_delta(&json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "다음"}
        }));

        let content = accumulator.finish(StopReason::ToolUse);
        assert_eq!(content.len(), 2);
        match &content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "먼저 다음"),
            block => panic!("expected text block, got {block:?}"),
        }
        match &content[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call-1");
                assert_eq!(name, "write_file");
                assert_eq!(input, &json!({"path": "game.html", "content": "<canvas/>"}));
            }
            block => panic!("expected tool block, got {block:?}"),
        }
    }

    #[test]
    fn stream_accumulator_reports_large_tool_input_progress() {
        let mut accumulator = AnthropicStreamAccumulator::default();
        accumulator.content_block_start(&json!({
            "type": "content_block_start",
            "index": 3,
            "content_block": {"type": "tool_use", "id": "call-3", "name": "write_file"}
        }));
        let partial_json = "x".repeat(TOOL_ARGS_STEP);
        let progress = accumulator.input_json_delta(&json!({
            "type": "content_block_delta",
            "index": 3,
            "delta": {"type": "input_json_delta", "partial_json": partial_json}
        }));
        assert_eq!(progress, Some(("write_file".into(), TOOL_ARGS_STEP)));
    }

    #[test]
    fn stream_accumulator_never_returns_truncated_or_invalid_tool_input() {
        let mut truncated = AnthropicStreamAccumulator::default();
        truncated.content_block_start(&json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "call-truncated", "name": "write_file"}
        }));
        truncated.input_json_delta(&json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"game.html\""}
        }));
        assert!(truncated.finish(StopReason::MaxTokens).is_empty());

        let mut invalid = AnthropicStreamAccumulator::default();
        invalid.content_block_start(&json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "call-invalid", "name": "write_file"}
        }));
        invalid.input_json_delta(&json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "{\"path\":}"}
        }));
        invalid.content_block_stop(&json!({"type": "content_block_stop", "index": 0}));
        assert!(invalid.finish(StopReason::ToolUse).is_empty());
    }

    #[test]
    fn message_start_keeps_cache_read_tokens_until_stop() {
        let event = json!({
            "type": "message_start",
            "message": {
                "usage": {
                    "input_tokens": 200,
                    "cache_read_input_tokens": 800
                }
            }
        });
        let mut input = 0;
        let mut cached = 0;
        let mut reported = false;
        take_stream_input_usage(&event, &mut input, &mut cached, &mut reported);
        assert_eq!((input, cached), (200, 800));
        assert!(reported);
    }

    #[test]
    fn api_error_discards_untrusted_body_before_persistent_logging() {
        let echoed_credential = "unlabeled-anthropic-credential-7f8db8d8";
        let body = format!("upstream detail: {echoed_credential}");

        for status in [429, 500] {
            let error = api_error(
                status,
                &body,
                &LimitHint {
                    retry_after_secs: Some(12),
                    ..LimitHint::default()
                },
            );
            for persistent_log_message in [
                format!("{error}"),
                format!("{error:#}"),
                format!("{error:?}"),
            ] {
                assert!(persistent_log_message.contains(&format!("HTTP {status}")));
                assert!(!persistent_log_message.contains(echoed_credential));
                assert!(!persistent_log_message.contains("upstream detail"));
            }
        }
    }
}
