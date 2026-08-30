use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{
    ChatRequest, ChatResponse, ContentBlock, LimitHint, MAX_CONTENT_BLOCKS, Message,
    ProtocolErrorKind, RequestErrorPhase, Role, SemanticStreamEvent, SseDecoder, SseDelimiter,
    StopReason, StreamBudget, StreamEvent, limit_hint, protocol_error, read_bounded_body,
    request_error, status_error, validate_chat_response,
};

const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const TOOL_ARGS_STEP: usize = 8 * 1024;

pub struct AnthropicProvider {
    client: reqwest::Client,
    token: String,
    oauth: bool,
    messages_url: String,
}

enum StreamingContentBlock {
    Text {
        text: String,
        stopped: bool,
    },
    ToolUse {
        id: String,
        name: String,
        input_json: String,
        stopped: bool,
    },
    Unknown {
        stopped: bool,
    },
}

#[derive(Default)]
struct AnthropicStreamAccumulator {
    blocks: BTreeMap<usize, StreamingContentBlock>,
    tool_args_marks: BTreeMap<usize, usize>,
    budget: StreamBudget,
}

impl AnthropicStreamAccumulator {
    fn content_block_start(&mut self, event: &Value) -> Result<()> {
        let index = stream_index(event)
            .ok_or_else(|| protocol_error("Anthropic", ProtocolErrorKind::InvalidSequence))?;
        if index >= MAX_CONTENT_BLOCKS || self.blocks.len() >= MAX_CONTENT_BLOCKS {
            return Err(protocol_error(
                "Anthropic",
                ProtocolErrorKind::LimitExceeded,
            ));
        }
        if self.blocks.contains_key(&index) {
            return Err(protocol_error(
                "Anthropic",
                ProtocolErrorKind::InvalidSequence,
            ));
        }
        let block = event
            .get("content_block")
            .ok_or_else(|| protocol_error("Anthropic", ProtocolErrorKind::InvalidSequence))?;
        self.budget.reserve_blocks(1, "Anthropic")?;
        match block.get("type").and_then(|value| value.as_str()) {
            Some("text") => {
                self.blocks.insert(
                    index,
                    StreamingContentBlock::Text {
                        text: String::new(),
                        stopped: false,
                    },
                );
            }
            Some("tool_use") => {
                let id_piece = block
                    .get("id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let name_piece = block
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if id_piece.is_empty() || name_piece.is_empty() {
                    return Err(protocol_error(
                        "Anthropic",
                        ProtocolErrorKind::InvalidSequence,
                    ));
                }
                let mut id = String::new();
                let mut name = String::new();
                self.budget
                    .append_metadata(&mut id, id_piece, "Anthropic")?;
                self.budget
                    .append_metadata(&mut name, name_piece, "Anthropic")?;
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
            _ => {
                self.blocks
                    .insert(index, StreamingContentBlock::Unknown { stopped: false });
            }
        }
        Ok(())
    }

    fn text_delta(&mut self, event: &Value) -> Result<()> {
        let index = stream_index(event)
            .ok_or_else(|| protocol_error("Anthropic", ProtocolErrorKind::InvalidSequence))?;
        let piece = event
            .pointer("/delta/text")
            .and_then(|value| value.as_str())
            .ok_or_else(|| protocol_error("Anthropic", ProtocolErrorKind::InvalidSequence))?;
        if piece.is_empty() {
            return Ok(());
        }
        match self.blocks.get_mut(&index) {
            Some(StreamingContentBlock::Text {
                text,
                stopped: false,
            }) => {
                self.budget.append_text(text, piece, true, "Anthropic")?;
            }
            _ => {
                return Err(protocol_error(
                    "Anthropic",
                    ProtocolErrorKind::InvalidSequence,
                ));
            }
        }
        Ok(())
    }

    fn input_json_delta(&mut self, event: &Value) -> Result<Option<(usize, usize)>> {
        let index = stream_index(event)
            .ok_or_else(|| protocol_error("Anthropic", ProtocolErrorKind::InvalidSequence))?;
        let piece = event
            .pointer("/delta/partial_json")
            .and_then(|value| value.as_str())
            .ok_or_else(|| protocol_error("Anthropic", ProtocolErrorKind::InvalidSequence))?;
        let StreamingContentBlock::ToolUse {
            input_json,
            stopped: false,
            ..
        } = self
            .blocks
            .get_mut(&index)
            .ok_or_else(|| protocol_error("Anthropic", ProtocolErrorKind::InvalidSequence))?
        else {
            return Err(protocol_error(
                "Anthropic",
                ProtocolErrorKind::InvalidSequence,
            ));
        };
        self.budget
            .append_tool_arguments(input_json, piece, "Anthropic")?;
        let total_bytes = input_json.len();
        if tool_args_due(&mut self.tool_args_marks, index, total_bytes) {
            Ok(Some((index, total_bytes)))
        } else {
            Ok(None)
        }
    }

    fn tool_name(&self, index: usize) -> Option<&str> {
        match self.blocks.get(&index) {
            Some(StreamingContentBlock::ToolUse { name, .. }) => Some(name),
            _ => None,
        }
    }

    fn content_block_stop(&mut self, event: &Value) -> Result<()> {
        let index = stream_index(event)
            .ok_or_else(|| protocol_error("Anthropic", ProtocolErrorKind::InvalidSequence))?;
        match self.blocks.get_mut(&index) {
            Some(StreamingContentBlock::Text { stopped, .. })
            | Some(StreamingContentBlock::ToolUse { stopped, .. })
            | Some(StreamingContentBlock::Unknown { stopped })
                if !*stopped =>
            {
                *stopped = true;
                Ok(())
            }
            _ => Err(protocol_error(
                "Anthropic",
                ProtocolErrorKind::InvalidSequence,
            )),
        }
    }

    fn finish(self, stop_reason: StopReason) -> Result<Vec<ContentBlock>> {
        let mut content = Vec::new();
        for block in self.blocks.into_values() {
            match block {
                StreamingContentBlock::Text {
                    text,
                    stopped: true,
                } if !text.is_empty() => {
                    content.push(ContentBlock::Text { text });
                }
                StreamingContentBlock::Text { text, .. }
                    if stop_reason == StopReason::MaxTokens =>
                {
                    if !text.is_empty() {
                        content.push(ContentBlock::Text { text });
                    }
                }
                StreamingContentBlock::ToolUse {
                    id,
                    name,
                    input_json,
                    stopped: true,
                } if stop_reason != StopReason::MaxTokens && !id.is_empty() && !name.is_empty() => {
                    let input = serde_json::from_str::<Value>(&input_json).map_err(|_| {
                        protocol_error("Anthropic", ProtocolErrorKind::InvalidSequence)
                    })?;
                    if !input.is_object() {
                        return Err(protocol_error(
                            "Anthropic",
                            ProtocolErrorKind::InvalidSequence,
                        ));
                    }
                    content.push(ContentBlock::ToolUse { id, name, input });
                }
                StreamingContentBlock::ToolUse { .. } if stop_reason == StopReason::MaxTokens => {}
                StreamingContentBlock::Unknown { stopped: true } => {}
                StreamingContentBlock::Unknown { .. } if stop_reason == StopReason::MaxTokens => {}
                _ => {
                    return Err(protocol_error(
                        "Anthropic",
                        ProtocolErrorKind::InvalidSequence,
                    ));
                }
            }
        }
        Ok(content)
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

impl AnthropicProvider {
    pub fn new(api_key: String) -> Result<Self> {
        Self::create(api_key, false)
    }

    pub fn with_oauth(token: String) -> Result<Self> {
        Self::create(token, true)
    }

    #[cfg(test)]
    fn with_url(messages_url: String) -> Result<Self> {
        let mut provider = Self::create("test-token".into(), false)?;
        provider.messages_url = messages_url;
        Ok(provider)
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
            messages_url: MESSAGES_URL.into(),
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
                    .post(&self.messages_url)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json"),
            )
            .json(&body)
            .send()
            .await
            .map_err(|error| request_error("Anthropic", &error, RequestErrorPhase::Send))?;

        let status = resp.status();
        let hint = limit_hint(resp.headers());
        if !status.is_success() {
            return Err(api_error(status.as_u16(), "", &hint));
        }
        let text = read_bounded_body(resp, "Anthropic").await?;
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
                    .post(&self.messages_url)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json"),
            )
            .json(&body)
            .send()
            .await
            .map_err(|error| request_error("Anthropic", &error, RequestErrorPhase::Send))?;

        let status = resp.status();
        let hint = limit_hint(resp.headers());
        if !status.is_success() {
            return Err(api_error(status.as_u16(), "", &hint));
        }

        let mut stream = resp.bytes_stream();
        let mut decoder = SseDecoder::new(SseDelimiter::Event);
        let mut stream_model = String::new();
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;
        let mut cached_tokens = 0u32;
        let mut cache_reported = false;
        let mut stop_reason = StopReason::Other;
        let mut thinking_started = false;
        let mut content = AnthropicStreamAccumulator::default();
        let mut saw_message_start = false;
        let mut saw_terminal_delta = false;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|error| request_error("Anthropic", &error, RequestErrorPhase::BodyRead))?;
            for &byte in chunk.as_ref() {
                let Some(event_bytes) = decoder.push(byte, "Anthropic")? else {
                    continue;
                };
                let event = String::from_utf8(event_bytes)
                    .map_err(|_| protocol_error("Anthropic", ProtocolErrorKind::InvalidJson))?;
                if let Some(data) = sse_data(&event) {
                    if data.trim() == "[DONE]" {
                        continue;
                    }
                    let v: Value = serde_json::from_str(data)
                        .map_err(|_| protocol_error("Anthropic", ProtocolErrorKind::InvalidJson))?;
                    if v.get("error").is_some() {
                        return Err(protocol_error(
                            "Anthropic",
                            ProtocolErrorKind::UpstreamError,
                        ));
                    }
                    let event_type = v.get("type").and_then(|t| t.as_str());
                    if matches!(
                        event_type,
                        Some(
                            "content_block_start"
                                | "content_block_delta"
                                | "content_block_stop"
                                | "message_delta"
                                | "message_stop"
                        )
                    ) && !saw_message_start
                    {
                        return Err(protocol_error(
                            "Anthropic",
                            ProtocolErrorKind::InvalidSequence,
                        ));
                    }
                    match event_type {
                        Some("message_start") => {
                            if saw_message_start {
                                return Err(protocol_error(
                                    "Anthropic",
                                    ProtocolErrorKind::InvalidSequence,
                                ));
                            }
                            saw_message_start = true;
                            if stream_model.is_empty()
                                && let Some(m) =
                                    v.pointer("/message/model").and_then(|x| x.as_str())
                            {
                                content.budget.append_metadata(
                                    &mut stream_model,
                                    m,
                                    "Anthropic",
                                )?;
                            }
                            take_stream_input_usage(
                                &v,
                                &mut input_tokens,
                                &mut cached_tokens,
                                &mut cache_reported,
                            );
                        }
                        Some("content_block_start") => content.content_block_start(&v)?,
                        Some("content_block_delta") => {
                            if v.pointer("/delta/type").and_then(|x| x.as_str())
                                == Some("text_delta")
                            {
                                if let Some(piece) =
                                    v.pointer("/delta/text").and_then(|x| x.as_str())
                                    && !piece.is_empty()
                                {
                                    content.text_delta(&v)?;
                                    if thinking_started {
                                        on_event(SemanticStreamEvent::ReasoningText(
                                            "\n[/모델 작업]\n",
                                        ));
                                        thinking_started = false;
                                    }
                                    on_event(SemanticStreamEvent::ContentCandidate(piece));
                                }
                            } else if v.pointer("/delta/type").and_then(|x| x.as_str())
                                == Some("thinking_delta")
                                && let Some(piece) =
                                    v.pointer("/delta/thinking").and_then(|x| x.as_str())
                                && !piece.is_empty()
                            {
                                content.budget.reserve_ephemeral_text(piece, "Anthropic")?;
                                if !thinking_started {
                                    on_event(SemanticStreamEvent::ReasoningText("\n[모델 작업]\n"));
                                    thinking_started = true;
                                }
                                on_event(SemanticStreamEvent::ReasoningText(piece));
                            } else if v.pointer("/delta/type").and_then(|x| x.as_str())
                                == Some("input_json_delta")
                                && let Some((index, total_bytes)) = content.input_json_delta(&v)?
                            {
                                let name = content.tool_name(index).unwrap_or("tool");
                                on_event(SemanticStreamEvent::ToolArgs { name, total_bytes });
                            }
                        }
                        Some("content_block_stop") => content.content_block_stop(&v)?,
                        Some("message_delta") => {
                            if let Some(raw_stop_reason) =
                                v.pointer("/delta/stop_reason").and_then(|x| x.as_str())
                            {
                                stop_reason = parse_anthropic_stop(Some(raw_stop_reason))?;
                                saw_terminal_delta = true;
                            }
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
                            let response = ChatResponse {
                                model: stream_model.clone(),
                                content: content.finish(stop_reason)?,
                                stop_reason,
                                input_tokens,
                                output_tokens,
                                cached_tokens,
                                cache_reported,
                                limit: hint.clone(),
                            };
                            require_anthropic_stream_completion(
                                saw_message_start,
                                saw_terminal_delta,
                                &response.content,
                            )?;
                            return Ok(response);
                        }
                        Some("error") => {
                            return Err(protocol_error(
                                "Anthropic",
                                ProtocolErrorKind::UpstreamError,
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }

        // message_stop 이 오면 위에서 반환된다 — EOF 도달은 미완료 응답이다.
        if !saw_message_start && input_tokens == 0 {
            return Err(protocol_error(
                "Anthropic",
                ProtocolErrorKind::TruncatedStream,
            ));
        }
        Err(protocol_error(
            "Anthropic",
            ProtocolErrorKind::TruncatedStream,
        ))
    }
}

fn require_anthropic_stream_completion(
    saw_message_start: bool,
    saw_terminal_delta: bool,
    content: &[ContentBlock],
) -> Result<()> {
    if !saw_message_start || !saw_terminal_delta {
        return Err(protocol_error(
            "Anthropic",
            ProtocolErrorKind::InvalidSequence,
        ));
    }
    if content.is_empty() {
        return Err(protocol_error(
            "Anthropic",
            ProtocolErrorKind::EmptyResponse,
        ));
    }
    Ok(())
}

fn parse_anthropic_stop(raw: Option<&str>) -> Result<StopReason> {
    match raw {
        Some("end_turn" | "stop_sequence") => Ok(StopReason::EndTurn),
        Some("tool_use") => Ok(StopReason::ToolUse),
        Some("max_tokens") => Ok(StopReason::MaxTokens),
        Some(_) | None => Err(protocol_error(
            "Anthropic",
            ProtocolErrorKind::InvalidSequence,
        )),
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
    let v: Value = serde_json::from_str(text)
        .map_err(|_| protocol_error("Anthropic", ProtocolErrorKind::InvalidJson))?;
    if v.get("error").is_some() || v.get("type").and_then(Value::as_str) == Some("error") {
        return Err(protocol_error(
            "Anthropic",
            ProtocolErrorKind::UpstreamError,
        ));
    }
    let stop_reason = parse_anthropic_stop(v.get("stop_reason").and_then(|s| s.as_str()))?;
    let mut budget = StreamBudget::default();
    let mut blocks = Vec::new();
    if let Some(arr) = v.get("content").and_then(|c| c.as_array()) {
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    budget.reserve_blocks(1, "Anthropic")?;
                    let mut text = String::new();
                    budget.append_text(&mut text, t, true, "Anthropic")?;
                    blocks.try_reserve_exact(1).map_err(|_| {
                        protocol_error("Anthropic", ProtocolErrorKind::LimitExceeded)
                    })?;
                    blocks.push(ContentBlock::Text { text });
                }
            } else if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                let raw_id = item.get("id").and_then(Value::as_str).unwrap_or("");
                let raw_name = item.get("name").and_then(Value::as_str).unwrap_or("");
                let input = item.get("input").unwrap_or(&Value::Null);
                if raw_id.trim().is_empty() || raw_name.trim().is_empty() || !input.is_object() {
                    if stop_reason == StopReason::MaxTokens {
                        continue;
                    }
                    return Err(protocol_error("Anthropic", ProtocolErrorKind::InvalidTool));
                }
                let input_bytes = serde_json::to_vec(input)
                    .map_err(|_| protocol_error("Anthropic", ProtocolErrorKind::InvalidJson))?;
                budget.reserve_tool_arguments(input_bytes.len(), "Anthropic")?;
                let mut id = String::new();
                budget.append_metadata(&mut id, raw_id, "Anthropic")?;
                let mut name = String::new();
                budget.append_metadata(&mut name, raw_name, "Anthropic")?;
                budget.reserve_blocks(1, "Anthropic")?;
                blocks
                    .try_reserve_exact(1)
                    .map_err(|_| protocol_error("Anthropic", ProtocolErrorKind::LimitExceeded))?;
                blocks.push(ContentBlock::ToolUse {
                    id,
                    name,
                    input: input.clone(),
                });
            }
        }
    }
    let mut model = String::new();
    if let Some(raw_model) = v.get("model").and_then(Value::as_str) {
        budget.append_metadata(&mut model, raw_model, "Anthropic")?;
    }
    let response = ChatResponse {
        model,
        content: blocks,
        stop_reason,
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
    };
    validate_chat_response(&response, "Anthropic")?;
    Ok(response)
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
    status_error("Anthropic", status, hint)
}

#[cfg(test)]
mod streaming_usage_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn sse_provider(body: String) -> AnthropicProvider {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        AnthropicProvider::with_url(format!("http://{address}")).unwrap()
    }

    fn streaming_request() -> ChatRequest {
        ChatRequest {
            model: "test-model".into(),
            system: String::new(),
            messages: vec![Message::user_text("test")],
            tools: Vec::new(),
            max_tokens: 1024,
            stream: true,
        }
    }

    fn valid_text_completion() -> &'static str {
        concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"test\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"late\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        )
    }

    #[tokio::test]
    async fn runtime_stream_rejects_malformed_or_error_before_valid_events() {
        for prefix in [
            "data: not-json\n\n",
            "data: {\"type\":\"error\",\"error\":{\"message\":\"secret\"}}\n\n",
            "data: {\"error\":{\"message\":\"secret\"}}\n\n",
        ] {
            let provider = sse_provider(format!("{prefix}{}", valid_text_completion())).await;
            let mut emitted = 0usize;
            let error = provider
                .chat_semantic_stream(&streaming_request(), |_| emitted += 1)
                .await
                .unwrap_err();
            assert!(
                error
                    .downcast_ref::<crate::provider::ProtocolError>()
                    .is_some()
            );
            assert!(!format!("{error:#}").contains("secret"));
            assert_eq!(emitted, 0);
        }
    }

    #[tokio::test]
    async fn runtime_stream_rejects_bad_tools_even_with_text() {
        for tool_tail in [
            concat!(
                "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n"
            ),
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
        ] {
            let body = format!(
                concat!(
                    "data: {{\"type\":\"message_start\",\"message\":{{}}}}\n\n",
                    "data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\"}}}}\n\n",
                    "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"keep\"}}}}\n\n",
                    "data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n",
                    "data: {{\"type\":\"content_block_start\",\"index\":1,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"call\",\"name\":\"write\"}}}}\n\n",
                    "{}",
                    "data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"tool_use\"}}}}\n\n",
                    "data: {{\"type\":\"message_stop\"}}\n\n"
                ),
                tool_tail
            );
            let provider = sse_provider(body).await;
            assert!(
                provider
                    .chat_semantic_stream(&streaming_request(), |_| {})
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn runtime_stream_preserves_text_on_max_token_tool_truncation() {
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"keep\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call\",\"name\":\"write\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let provider = sse_provider(body.into()).await;
        let response = provider
            .chat_semantic_stream(&streaming_request(), |_| {})
            .await
            .unwrap();
        assert_eq!(response.stop_reason, StopReason::MaxTokens);
        assert!(matches!(
            response.content.as_slice(),
            [ContentBlock::Text { text }] if text == "keep"
        ));
    }

    #[tokio::test]
    async fn runtime_stream_rejects_oversized_and_delimiter_free_frames_before_emission() {
        let oversized = "x".repeat(crate::provider::MAX_SSE_FRAME_BYTES);
        for body in [
            format!(
                "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{oversized}\"}}}}\n\n"
            ),
            "x".repeat(crate::provider::MAX_SSE_FRAME_BYTES + 1),
        ] {
            let provider = sse_provider(body).await;
            let mut emitted = 0usize;
            let error = provider
                .chat_semantic_stream(&streaming_request(), |_| emitted += 1)
                .await
                .unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<crate::provider::ProtocolError>()
                    .map(crate::provider::ProtocolError::kind),
                Some(ProtocolErrorKind::LimitExceeded)
            );
            assert_eq!(emitted, 0);
        }
    }

    #[test]
    fn terminal_stream_requires_start_delta_and_content() {
        assert!(require_anthropic_stream_completion(false, true, &[]).is_err());
        assert!(require_anthropic_stream_completion(true, false, &[]).is_err());
        assert!(require_anthropic_stream_completion(true, true, &[]).is_err());
        assert!(
            require_anthropic_stream_completion(
                true,
                true,
                &[ContentBlock::Text {
                    text: "done".into(),
                }],
            )
            .is_ok()
        );
    }

    #[test]
    fn stream_accumulator_keeps_interleaved_blocks_in_index_order() {
        let mut accumulator = AnthropicStreamAccumulator::default();
        accumulator
            .content_block_start(&json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text"}
            }))
            .unwrap();
        accumulator
            .text_delta(&json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "먼저 "}
            }))
            .unwrap();
        accumulator
            .content_block_start(&json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": {"type": "tool_use", "id": "call-1", "name": "write_file"}
            }))
            .unwrap();
        for partial_json in [r#"{"path":"game.html", "#, r#""content":"<canvas/>"}"#] {
            accumulator
                .input_json_delta(&json!({
                    "type": "content_block_delta",
                    "index": 1,
                    "delta": {"type": "input_json_delta", "partial_json": partial_json}
                }))
                .unwrap();
        }
        accumulator
            .content_block_stop(&json!({"type": "content_block_stop", "index": 1}))
            .unwrap();
        accumulator
            .text_delta(&json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "다음"}
            }))
            .unwrap();
        accumulator
            .content_block_stop(&json!({"type": "content_block_stop", "index": 0}))
            .unwrap();

        let content = accumulator.finish(StopReason::ToolUse).unwrap();
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
        accumulator
            .content_block_start(&json!({
                "type": "content_block_start",
                "index": 3,
                "content_block": {"type": "tool_use", "id": "call-3", "name": "write_file"}
            }))
            .unwrap();
        let partial_json = "x".repeat(TOOL_ARGS_STEP);
        let progress = accumulator
            .input_json_delta(&json!({
                "type": "content_block_delta",
                "index": 3,
                "delta": {"type": "input_json_delta", "partial_json": partial_json}
            }))
            .unwrap();
        assert_eq!(progress, Some((3, TOOL_ARGS_STEP)));
    }

    #[test]
    fn stream_accumulator_never_returns_truncated_or_invalid_tool_input() {
        let mut truncated = AnthropicStreamAccumulator::default();
        truncated
            .content_block_start(&json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "tool_use", "id": "call-truncated", "name": "write_file"}
            }))
            .unwrap();
        truncated
            .input_json_delta(&json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"game.html\""}
            }))
            .unwrap();
        assert!(truncated.finish(StopReason::MaxTokens).unwrap().is_empty());

        let mut invalid = AnthropicStreamAccumulator::default();
        invalid
            .content_block_start(&json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "tool_use", "id": "call-invalid", "name": "write_file"}
            }))
            .unwrap();
        invalid
            .input_json_delta(&json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": "{\"path\":}"}
            }))
            .unwrap();
        invalid
            .content_block_stop(&json!({"type": "content_block_stop", "index": 0}))
            .unwrap();
        assert!(invalid.finish(StopReason::ToolUse).is_err());
    }

    #[test]
    fn malformed_tool_fails_with_surviving_text_unless_max_tokens() {
        fn accumulator() -> AnthropicStreamAccumulator {
            let mut value = AnthropicStreamAccumulator::default();
            value
                .content_block_start(&json!({
                    "index": 0,
                    "content_block": {"type": "text"}
                }))
                .unwrap();
            value
                .text_delta(&json!({"index":0,"delta":{"text":"keep"}}))
                .unwrap();
            value.content_block_stop(&json!({"index":0})).unwrap();
            value
                .content_block_start(&json!({
                    "index": 1,
                    "content_block": {"type":"tool_use","id":"call","name":"write"}
                }))
                .unwrap();
            value
                .input_json_delta(&json!({"index":1,"delta":{"partial_json":"{"}}))
                .unwrap();
            value
        }

        assert!(accumulator().finish(StopReason::ToolUse).is_err());
        let content = accumulator().finish(StopReason::MaxTokens).unwrap();
        assert!(matches!(
            content.as_slice(),
            [ContentBlock::Text { text }] if text == "keep"
        ));
    }

    #[test]
    fn event_text_and_tool_argument_limits_are_enforced() {
        let mut decoder = SseDecoder::new(SseDelimiter::Event);
        for _ in 0..crate::provider::MAX_SSE_FRAME_BYTES {
            assert!(decoder.push(b'x', "Anthropic").unwrap().is_none());
        }
        assert!(decoder.push(b'x', "Anthropic").is_err());

        let mut text = AnthropicStreamAccumulator::default();
        text.content_block_start(&json!({
            "index":0,
            "content_block":{"type":"text"}
        }))
        .unwrap();
        let oversized_text = "x".repeat(crate::provider::MAX_RESPONSE_TEXT_BYTES + 1);
        assert!(
            text.text_delta(&json!({"index":0,"delta":{"text":oversized_text}}))
                .is_err()
        );

        let mut tool = AnthropicStreamAccumulator::default();
        tool.content_block_start(&json!({
            "index":0,
            "content_block":{"type":"tool_use","id":"call","name":"write"}
        }))
        .unwrap();
        let oversized_args = "x".repeat(crate::provider::MAX_TOOL_ARGUMENT_BYTES + 1);
        let rejected =
            tool.input_json_delta(&json!({"index":0,"delta":{"partial_json":oversized_args}}));
        let mut emitted = 0usize;
        if rejected
            .as_ref()
            .ok()
            .and_then(|progress| *progress)
            .is_some()
        {
            emitted += 1;
        }
        assert!(rejected.is_err());
        assert_eq!(emitted, 0);
    }

    #[test]
    fn empty_nonstream_and_upstream_error_are_rejected_without_details() {
        assert!(parse_message_json(r#"{"content":[]}"#).is_err());
        let error =
            parse_message_json(r#"{"type":"error","error":{"message":"credential-secret"}}"#)
                .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::provider::ProtocolError>()
                .map(crate::provider::ProtocolError::kind),
            Some(ProtocolErrorKind::UpstreamError)
        );
        assert!(!format!("{error:#}").contains("credential-secret"));
    }

    #[test]
    fn nonstream_requires_supported_stop_reason() {
        for stop in [
            None,
            Some("pause_turn"),
            Some("refusal"),
            Some("context_limit"),
        ] {
            let mut value = json!({"content":[{"type":"text","text":"valid"}]});
            if let Some(stop) = stop {
                value["stop_reason"] = Value::String(stop.into());
            }
            let error = parse_message_json(&value.to_string()).unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<crate::provider::ProtocolError>()
                    .map(crate::provider::ProtocolError::kind),
                Some(ProtocolErrorKind::InvalidSequence)
            );
        }
        let response = parse_message_json(
            &json!({
                "content":[{"type":"text","text":"valid"}],
                "stop_reason":"stop_sequence"
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(response.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn nonstream_malformed_tool_only_survives_explicit_max_tokens() {
        let malformed = json!({"type":"tool_use","id":"call","name":"write","input":"{"});
        for stop in ["end_turn", "tool_use"] {
            let value = json!({
                "content":[{"type":"text","text":"keep"}, malformed.clone()],
                "stop_reason":stop
            });
            let error = parse_message_json(&value.to_string()).unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<crate::provider::ProtocolError>()
                    .map(crate::provider::ProtocolError::kind),
                Some(ProtocolErrorKind::InvalidTool)
            );
        }
        let truncated = json!({
            "content":[{"type":"text","text":"keep"}, malformed],
            "stop_reason":"max_tokens"
        });
        let response = parse_message_json(&truncated.to_string()).unwrap();
        assert_eq!(response.stop_reason, StopReason::MaxTokens);
        assert!(matches!(
            response.content.as_slice(),
            [ContentBlock::Text { text }] if text == "keep"
        ));
    }

    #[test]
    fn nonstream_enforces_total_block_and_metadata_caps() {
        let mut content = vec![json!({"type":"text","text":"text"})];
        content.extend((0..255).map(|index| {
            json!({
                "type":"tool_use",
                "id":format!("call-{index}"),
                "name":"tool",
                "input":{}
            })
        }));
        let accepted = json!({"content":content,"stop_reason":"tool_use"});
        assert_eq!(
            parse_message_json(&accepted.to_string())
                .unwrap()
                .content
                .len(),
            crate::provider::MAX_CONTENT_BLOCKS
        );

        let mut overflow = accepted;
        overflow["content"].as_array_mut().unwrap().push(json!({
            "type":"tool_use","id":"overflow","name":"tool","input":{}
        }));
        assert!(parse_message_json(&overflow.to_string()).is_err());

        for (id, name, model) in [
            (
                "x".repeat(crate::provider::MAX_METADATA_FIELD_BYTES + 1),
                "tool".into(),
                String::new(),
            ),
            (
                "call".into(),
                "x".repeat(crate::provider::MAX_METADATA_FIELD_BYTES + 1),
                String::new(),
            ),
            (
                "call".into(),
                "tool".into(),
                "x".repeat(crate::provider::MAX_METADATA_FIELD_BYTES + 1),
            ),
        ] {
            let value = json!({
                "model":model,
                "content":[{"type":"tool_use","id":id,"name":name,"input":{}}],
                "stop_reason":"tool_use"
            });
            assert!(parse_message_json(&value.to_string()).is_err());
        }
    }

    #[test]
    fn nonstream_enforces_aggregate_text_tool_and_state_caps() {
        let half_text = crate::provider::MAX_RESPONSE_TEXT_BYTES / 2;
        let aggregate_text = json!({
            "content":[
                {"type":"text","text":"x".repeat(half_text)},
                {"type":"text","text":"x".repeat(half_text + 1)}
            ],
            "stop_reason":"end_turn"
        });
        assert!(parse_message_json(&aggregate_text.to_string()).is_err());

        let oversized_tool = json!({
            "content":[{
                "type":"tool_use",
                "id":"call",
                "name":"tool",
                "input":{"value":"x".repeat(crate::provider::MAX_TOOL_ARGUMENT_BYTES)}
            }],
            "stop_reason":"tool_use"
        });
        assert!(parse_message_json(&oversized_tool.to_string()).is_err());

        let half_tool = crate::provider::MAX_TOOL_ARGUMENT_BYTES / 2;
        let aggregate_tools = json!({
            "content":[
                {"type":"tool_use","id":"a","name":"tool","input":{"value":"x".repeat(half_tool)}},
                {"type":"tool_use","id":"b","name":"tool","input":{"value":"x".repeat(half_tool)}}
            ],
            "stop_reason":"tool_use"
        });
        assert!(parse_message_json(&aggregate_tools.to_string()).is_err());

        let state_overflow = json!({
            "content":[
                {"type":"text","text":"x".repeat(crate::provider::MAX_RESPONSE_TEXT_BYTES)},
                {"type":"tool_use","id":"call","name":"tool","input":{
                    "value":"x".repeat(crate::provider::MAX_TOOL_ARGUMENT_BYTES - 12)
                }}
            ],
            "stop_reason":"tool_use"
        });
        assert!(parse_message_json(&state_overflow.to_string()).is_err());
    }

    #[tokio::test]
    async fn runtime_stream_rejects_missing_or_unsupported_stop_reason() {
        for terminal in [
            "data: {\"type\":\"message_stop\"}\n\n",
            concat!(
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"pause_turn\"}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            ),
        ] {
            let body = format!(
                concat!(
                    "data: {{\"type\":\"message_start\",\"message\":{{}}}}\n\n",
                    "data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\"}}}}\n\n",
                    "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"valid\"}}}}\n\n",
                    "data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n",
                    "{}"
                ),
                terminal
            );
            let provider = sse_provider(body).await;
            assert!(
                provider
                    .chat_semantic_stream(&streaming_request(), |_| {})
                    .await
                    .is_err()
            );
        }
    }

    #[test]
    fn unknown_well_formed_block_is_forward_compatible() {
        let mut accumulator = AnthropicStreamAccumulator::default();
        accumulator
            .content_block_start(&json!({
                "index":0,
                "content_block":{"type":"future_block"}
            }))
            .unwrap();
        accumulator.content_block_stop(&json!({"index":0})).unwrap();
        assert!(accumulator.finish(StopReason::EndTurn).unwrap().is_empty());
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
                if status != 429 {
                    assert!(persistent_log_message.contains(&format!("HTTP {status}")));
                }
                assert!(!persistent_log_message.contains(echoed_credential));
                assert!(!persistent_log_message.contains("upstream detail"));
            }
        }
    }
}
