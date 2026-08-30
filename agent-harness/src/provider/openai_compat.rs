use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{
    ChatRequest, ChatResponse, ContentBlock, LimitHint, MAX_CONTENT_BLOCKS,
    MAX_TOOL_ARGUMENT_BYTES, Message, ProtocolErrorKind, RequestErrorPhase, Role,
    SemanticStreamEvent, SseDecoder, SseDelimiter, StopReason, StreamBudget, StreamEvent,
    limit_hint, map_stop_reason, protocol_error, read_bounded_body, request_error, status_error,
};

const CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";
const CODEX_ORIGINATOR: &str = "codex_cli_rs";

enum CompatMode {
    ChatCompletions,
    CodexResponses,
}

pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    mode: CompatMode,
    account_id: String,
}

impl OpenAiCompatProvider {
    pub fn new(base_url: String, api_key: Option<String>) -> Result<Self> {
        Self::create(
            base_url,
            api_key,
            CompatMode::ChatCompletions,
            String::new(),
        )
    }

    pub fn with_codex_oauth(token: String, account_id: String) -> Result<Self> {
        Self::create(
            CODEX_BASE.into(),
            Some(token),
            CompatMode::CodexResponses,
            account_id,
        )
    }

    #[cfg(test)]
    fn with_codex_oauth_at(base_url: String) -> Result<Self> {
        Self::create(
            base_url,
            Some("test-token".into()),
            CompatMode::CodexResponses,
            String::new(),
        )
    }

    fn create(
        base_url: String,
        api_key: Option<String>,
        mode: CompatMode,
        account_id: String,
    ) -> Result<Self> {
        // 전체 timeout 은 긴 스트리밍 응답(긴 추론·대형 파일 생성)을 도중에 끊는다.
        // 연결 수립과 청크 간 침묵에만 상한을 둔다.
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .read_timeout(std::time::Duration::from_secs(180))
            .build()
            .context("HTTP 클라이언트를 만들 수 없습니다")?;
        Ok(Self {
            client,
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            mode,
            account_id,
        })
    }

    fn url(&self) -> String {
        match self.mode {
            CompatMode::CodexResponses => format!("{}/responses", self.base_url),
            CompatMode::ChatCompletions => format!("{}/chat/completions", self.base_url),
        }
    }

    fn apply_auth(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(key) = &self.api_key
            && !key.is_empty()
        {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        if matches!(self.mode, CompatMode::CodexResponses) {
            req = req
                .header("originator", CODEX_ORIGINATOR)
                .header("User-Agent", CODEX_ORIGINATOR)
                .header("OpenAI-Beta", "responses=experimental")
                .header("session_id", session_id());
            if !self.account_id.is_empty() {
                req = req.header("chatgpt-account-id", &self.account_id);
            }
        }
        req
    }

    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let codex = matches!(self.mode, CompatMode::CodexResponses);
        let body = match self.mode {
            CompatMode::CodexResponses => build_codex_body(req, false),
            CompatMode::ChatCompletions => build_body(req, false),
        };
        let builder = self.apply_auth(
            self.client
                .post(self.url())
                .header("content-type", "application/json"),
        );
        let resp = builder.json(&body).send().await.map_err(|error| {
            request_error(provider_label(&self.mode), &error, RequestErrorPhase::Send)
        })?;
        let status = resp.status();
        let hint = limit_hint(resp.headers());
        if !status.is_success() {
            return Err(api_error(status.as_u16(), "", &hint, &self.mode));
        }
        let text = read_bounded_body(resp, "OpenAI").await?;
        let mut parsed = match self.mode {
            CompatMode::CodexResponses => parse_codex_response(&text)?,
            CompatMode::ChatCompletions => parse_completion(&text)?,
        };
        if codex {
            parsed = enforce_codex_output_limit(parsed, req.max_tokens);
        }
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
        if matches!(self.mode, CompatMode::CodexResponses) {
            return self.chat_codex_stream(req, on_event).await;
        }
        let body = build_body(req, true);
        let builder = self.apply_auth(
            self.client
                .post(self.url())
                .header("content-type", "application/json"),
        );
        let resp = builder
            .json(&body)
            .send()
            .await
            .map_err(|error| request_error("OpenAI", &error, RequestErrorPhase::Send))?;
        let status = resp.status();
        let hint = limit_hint(resp.headers());
        if !status.is_success() {
            return Err(api_error(status.as_u16(), "", &hint, &self.mode));
        }

        let mut stream = resp.bytes_stream();
        let mut decoder = SseDecoder::new(SseDelimiter::Line);
        let mut budget = StreamBudget::default();
        let mut full_text = String::new();
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;
        let mut cached_tokens = 0u32;
        let mut cache_reported = false;
        let mut finish = None::<String>;
        let mut tool_acc: Vec<(String, String, String)> = Vec::new();
        let mut stream_model = String::new();
        // tool call 인덱스별로 마지막 진행 발행 시점의 누적 바이트 수.
        let mut args_marks: Vec<usize> = Vec::new();
        let mut reasoning_started = false;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|error| request_error("OpenAI", &error, RequestErrorPhase::BodyRead))?;
            for &byte in chunk.as_ref() {
                let Some(line_bytes) = decoder.push(byte, "OpenAI")? else {
                    continue;
                };
                let line = String::from_utf8(line_bytes)
                    .map_err(|_| protocol_error("OpenAI", ProtocolErrorKind::InvalidJson))?;
                let line = line.trim_end_matches(['\n', '\r']);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    if reasoning_started {
                        on_event(SemanticStreamEvent::ReasoningText("\n[/모델 작업]\n"));
                    }
                    if finish.is_none() {
                        return Err(protocol_error("OpenAI", ProtocolErrorKind::InvalidSequence));
                    }
                    validate_tool_acc(&tool_acc, finish.as_deref(), "OpenAI")?;
                    let response = finish_stream(
                        full_text,
                        tool_acc,
                        finish.as_deref(),
                        input_tokens,
                        output_tokens,
                        cached_tokens,
                        cache_reported,
                        hint.clone(),
                        stream_model.clone(),
                    );
                    require_stream_content(&response, "OpenAI 호환")?;
                    return Ok(response);
                }
                let v = parse_stream_json(data, "OpenAI")?;
                if stream_model.is_empty()
                    && let Some(m) = v.get("model").and_then(|x| x.as_str())
                {
                    budget.append_metadata(&mut stream_model, m, "OpenAI")?;
                }
                take_stream_usage(
                    &v,
                    &mut input_tokens,
                    &mut output_tokens,
                    &mut cached_tokens,
                    &mut cache_reported,
                );
                let Some(choice) = v
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                else {
                    continue;
                };
                if finish.is_some()
                    && choice.get("delta").is_some_and(|delta| {
                        delta.get("content").is_some()
                            || delta.get("reasoning_content").is_some()
                            || delta.get("reasoning").is_some()
                            || delta.get("tool_calls").is_some()
                    })
                {
                    return Err(protocol_error("OpenAI", ProtocolErrorKind::InvalidSequence));
                }
                if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str())
                    && !fr.is_empty()
                    && fr != "null"
                {
                    parse_chat_finish(Some(fr))?;
                    if finish.is_some() {
                        return Err(protocol_error("OpenAI", ProtocolErrorKind::InvalidSequence));
                    }
                    let mut reason = String::new();
                    budget.append_metadata(&mut reason, fr, "OpenAI")?;
                    finish = Some(reason);
                }
                if let Some(delta) = choice.get("delta") {
                    if let Some(piece) = delta.get("content").and_then(|x| x.as_str())
                        && !piece.is_empty()
                    {
                        if full_text.is_empty() {
                            budget.reserve_blocks(1, "OpenAI")?;
                        }
                        budget.append_text(&mut full_text, piece, true, "OpenAI")?;
                        if reasoning_started {
                            on_event(SemanticStreamEvent::ReasoningText("\n[/모델 작업]\n"));
                            reasoning_started = false;
                        }
                        on_event(SemanticStreamEvent::ContentCandidate(piece));
                    }
                    if let Some(piece) = delta
                        .get("reasoning_content")
                        .or_else(|| delta.get("reasoning"))
                        .and_then(|x| x.as_str())
                        && !piece.is_empty()
                    {
                        budget.reserve_ephemeral_text(piece, "OpenAI")?;
                        if !reasoning_started {
                            on_event(SemanticStreamEvent::ReasoningText("\n[모델 작업]\n"));
                            reasoning_started = true;
                        }
                        on_event(SemanticStreamEvent::ReasoningText(piece));
                    }
                    if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
                        for tc in tcs {
                            let idx = tc
                                .get("index")
                                .and_then(|x| x.as_u64())
                                .and_then(|value| usize::try_from(value).ok())
                                .ok_or_else(|| {
                                    protocol_error("OpenAI", ProtocolErrorKind::InvalidSequence)
                                })?;
                            if idx >= MAX_CONTENT_BLOCKS {
                                return Err(protocol_error(
                                    "OpenAI",
                                    ProtocolErrorKind::LimitExceeded,
                                ));
                            }
                            if idx >= tool_acc.len() {
                                let added = idx
                                    .checked_add(1)
                                    .and_then(|next| next.checked_sub(tool_acc.len()))
                                    .ok_or_else(|| {
                                        protocol_error("OpenAI", ProtocolErrorKind::LimitExceeded)
                                    })?;
                                budget.reserve_blocks(added, "OpenAI")?;
                                tool_acc.try_reserve_exact(added).map_err(|_| {
                                    protocol_error("OpenAI", ProtocolErrorKind::LimitExceeded)
                                })?;
                            }
                            while tool_acc.len() <= idx {
                                tool_acc.push((String::new(), String::new(), String::new()));
                            }
                            if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                                budget.append_metadata(&mut tool_acc[idx].0, id, "OpenAI")?;
                            }
                            if let Some(name) =
                                tc.pointer("/function/name").and_then(|x| x.as_str())
                            {
                                budget.append_metadata(&mut tool_acc[idx].1, name, "OpenAI")?;
                            }
                            if let Some(args) =
                                tc.pointer("/function/arguments").and_then(|x| x.as_str())
                            {
                                budget.append_tool_arguments(
                                    &mut tool_acc[idx].2,
                                    args,
                                    "OpenAI",
                                )?;
                                // 대형 인자(수 KB~수십 KB) 생성 구간은 텍스트가 한 조각도
                                // 흐르지 않는다 — 누적량을 진행 신호로 내보내 침묵을 없앤다.
                                let total = tool_acc[idx].2.len();
                                if tool_args_due(&mut args_marks, idx, total) {
                                    let name = if tool_acc[idx].1.is_empty() {
                                        "tool"
                                    } else {
                                        tool_acc[idx].1.as_str()
                                    };
                                    on_event(SemanticStreamEvent::ToolArgs {
                                        name,
                                        total_bytes: total,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        if reasoning_started {
            on_event(SemanticStreamEvent::ReasoningText("\n[/모델 작업]\n"));
        }
        Err(protocol_error("OpenAI", ProtocolErrorKind::TruncatedStream))
    }

    async fn chat_codex_stream<F>(&self, req: &ChatRequest, mut on_event: F) -> Result<ChatResponse>
    where
        F: FnMut(SemanticStreamEvent),
    {
        let body = build_codex_body(req, true);
        let builder = self.apply_auth(
            self.client
                .post(self.url())
                .header("content-type", "application/json"),
        );
        let resp = builder
            .json(&body)
            .send()
            .await
            .map_err(|error| request_error("Codex", &error, RequestErrorPhase::Send))?;
        let stream_model = String::new();
        let status = resp.status();
        let hint = limit_hint(resp.headers());
        if !status.is_success() {
            return Err(api_error(status.as_u16(), "", &hint, &self.mode));
        }

        let mut stream = resp.bytes_stream();
        let mut decoder = SseDecoder::new(SseDelimiter::Line);
        let mut budget = StreamBudget::default();
        let mut full_text = String::new();
        let mut tools: Vec<(String, String, String)> = Vec::new();
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;
        let mut cached_tokens = 0u32;
        let mut cache_reported = false;
        let mut finish = None::<String>;
        let mut reasoning_started = false;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|error| request_error("Codex", &error, RequestErrorPhase::BodyRead))?;
            for &byte in chunk.as_ref() {
                let Some(line_bytes) = decoder.push(byte, "Codex")? else {
                    continue;
                };
                let line = String::from_utf8(line_bytes)
                    .map_err(|_| protocol_error("Codex", ProtocolErrorKind::InvalidJson))?;
                let line = line.trim_end_matches(['\n', '\r']);
                let line = line.trim();
                if line.is_empty() || line.starts_with("event:") {
                    continue;
                }
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    close_codex_reasoning(&mut reasoning_started, &mut on_event);
                    if finish.is_none() {
                        return Err(protocol_error("Codex", ProtocolErrorKind::InvalidSequence));
                    }
                    validate_tool_acc(&tools, finish.as_deref(), "Codex")?;
                    let response = enforce_codex_output_limit(
                        finish_stream(
                            full_text,
                            tools,
                            finish.as_deref(),
                            input_tokens,
                            output_tokens,
                            cached_tokens,
                            cache_reported,
                            hint,
                            stream_model,
                        ),
                        req.max_tokens,
                    );
                    require_stream_content(&response, "Codex")?;
                    return Ok(response);
                }
                let v = parse_stream_json(data, "Codex")?;
                if codex_terminal_failure(&v).is_some() {
                    return Err(protocol_error("Codex", ProtocolErrorKind::UpstreamError));
                }
                let output_limit_reached = apply_codex_event(
                    &v,
                    &mut full_text,
                    &mut tools,
                    &mut input_tokens,
                    &mut output_tokens,
                    &mut cached_tokens,
                    &mut cache_reported,
                    &mut finish,
                    &mut reasoning_started,
                    &mut budget,
                    Some(&mut on_event),
                    req.max_tokens,
                )?;
                if output_limit_reached {
                    tools.clear();
                    let response = enforce_codex_output_limit(
                        finish_stream(
                            full_text,
                            tools,
                            finish.as_deref(),
                            input_tokens,
                            output_tokens,
                            cached_tokens,
                            cache_reported,
                            hint,
                            stream_model,
                        ),
                        req.max_tokens,
                    );
                    require_stream_content(&response, "Codex")?;
                    return Ok(response);
                }
                if finish.is_some() {
                    validate_tool_acc(&tools, finish.as_deref(), "Codex")?;
                    let response = enforce_codex_output_limit(
                        finish_stream(
                            full_text,
                            tools,
                            finish.as_deref(),
                            input_tokens,
                            output_tokens,
                            cached_tokens,
                            cache_reported,
                            hint,
                            stream_model,
                        ),
                        req.max_tokens,
                    );
                    require_stream_content(&response, "Codex")?;
                    return Ok(response);
                }
            }
        }

        if full_text.is_empty() && tools.is_empty() {
            return Err(protocol_error("Codex", ProtocolErrorKind::TruncatedStream));
        }
        Err(protocol_error("Codex", ProtocolErrorKind::TruncatedStream))
    }
}

fn close_codex_reasoning<F>(reasoning_started: &mut bool, on_event: &mut F)
where
    F: FnMut(SemanticStreamEvent),
{
    if *reasoning_started {
        on_event(SemanticStreamEvent::ReasoningText("\n[/모델 작업]\n"));
        *reasoning_started = false;
    }
}

/// 도구 인자 진행 신호의 발행 간격 (바이트). 너무 촘촘하면 화면이 시끄럽고,
/// 너무 성기면 침묵 구간이 다시 생긴다.
const TOOL_ARGS_STEP: usize = 8192;

/// 인덱스별로 마지막 발행 이후 `TOOL_ARGS_STEP` 을 넘겼을 때만 true 를 돌려주고
/// 그 시점을 기록한다. 인덱스마다 독립적으로 센다 (한 응답에 도구 여러 개).
fn tool_args_due(marks: &mut Vec<usize>, idx: usize, total: usize) -> bool {
    while marks.len() <= idx {
        marks.push(0);
    }
    if total >= marks[idx] + TOOL_ARGS_STEP {
        marks[idx] = total;
        return true;
    }
    false
}

/// tool call 인자 문자열을 해석한다. 빈 문자열은 인자 없는 호출이므로 `{}`,
/// 그 외 해석 불가(스트림 절단으로 잘린 JSON 등)는 None — 호출 자체를 버려야 한다.
fn parse_tool_args(args: &str) -> Option<Value> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Some(json!({}));
    }
    serde_json::from_str(trimmed).ok().filter(Value::is_object)
}

fn parse_stream_json(data: &str, provider: &'static str) -> Result<Value> {
    let value = serde_json::from_str::<Value>(data)
        .map_err(|_| protocol_error(provider, ProtocolErrorKind::InvalidJson))?;
    if value.get("error").is_some() || value.get("type").and_then(Value::as_str) == Some("error") {
        return Err(protocol_error(provider, ProtocolErrorKind::UpstreamError));
    }
    Ok(value)
}

fn validate_tool_acc(
    tools: &[(String, String, String)],
    finish: Option<&str>,
    provider: &'static str,
) -> Result<()> {
    if matches!(finish, Some("length" | "max_tokens")) {
        return Ok(());
    }
    for (id, name, arguments) in tools {
        if arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
            return Err(protocol_error(provider, ProtocolErrorKind::LimitExceeded));
        }
        if id.is_empty() || name.is_empty() || parse_tool_args(arguments).is_none() {
            return Err(protocol_error(provider, ProtocolErrorKind::InvalidSequence));
        }
    }
    Ok(())
}

fn enforce_codex_output_limit(mut response: ChatResponse, max_tokens: u32) -> ChatResponse {
    let has_output = !response.content.is_empty();
    let usage_missing = has_output && response.output_tokens == 0;
    if max_tokens == 0 || (!usage_missing && response.output_tokens <= max_tokens) {
        return response;
    }
    // ChatGPT Codex Responses 는 max_output_tokens 를 받지 않는다. 바이트 수를
    // 토큰으로 추정하면 정상 응답을 자르므로, 서버가 보고한 실제 사용량만 판정한다.
    // 상한을 넘긴 응답의 텍스트는 증거로 보존하되 도구 호출은 실행하지 않는다.
    response
        .content
        .retain(|block| !matches!(block, ContentBlock::ToolUse { .. }));
    response.stop_reason = StopReason::MaxTokens;
    response
}

fn require_stream_content(response: &ChatResponse, provider: &str) -> Result<()> {
    let provider = match provider {
        "Codex" => "Codex",
        _ => "OpenAI",
    };
    super::validate_chat_response(response, provider)
}

#[allow(clippy::too_many_arguments)] // 스트림 종결 축이 인자별로 독립적이라 유지
fn finish_stream(
    full_text: String,
    tool_acc: Vec<(String, String, String)>,
    finish: Option<&str>,
    input_tokens: u32,
    output_tokens: u32,
    cached_tokens: u32,
    cache_reported: bool,
    limit: LimitHint,
    model: String,
) -> ChatResponse {
    let mut content = Vec::new();
    if !full_text.is_empty() {
        content.push(ContentBlock::Text { text: full_text });
    }
    for (id, name, args) in tool_acc {
        if id.trim().is_empty() || name.trim().is_empty() {
            continue;
        }
        let Some(input) = parse_tool_args(&args) else {
            // 출력 상한(length) 등으로 인자 JSON이 중간에 잘린 호출 — 빈 인자로
            // 실행하면 "path 인자가 필요합니다" 류의 오해만 낳으므로 버린다.
            continue;
        };
        content.push(ContentBlock::ToolUse { id, name, input });
    }
    let has_tools = content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    ChatResponse {
        model,
        content,
        stop_reason: if has_tools {
            StopReason::ToolUse
        } else {
            map_openai_finish(finish)
        },
        input_tokens,
        output_tokens,
        cached_tokens,
        cache_reported,
        limit,
    }
}

fn build_body(req: &ChatRequest, stream: bool) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": to_openai_messages(&req.system, &req.messages),
        "max_tokens": req.max_tokens,
        "stream": stream,
    });
    if stream {
        body["stream_options"] = json!({"include_usage": true});
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    body
}

fn to_openai_messages(system: &str, msgs: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    if !system.is_empty() {
        out.push(json!({"role": "system", "content": system}));
    }
    for m in msgs {
        match m.role {
            Role::System => {
                let text = concat_text(&m.content);
                if !text.is_empty() {
                    out.push(json!({"role": "system", "content": text}));
                }
            }
            Role::User => {
                let results: Vec<Value> = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => Some(json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": content,
                        })),
                        _ => None,
                    })
                    .collect();
                if !results.is_empty() {
                    out.extend(results);
                } else {
                    out.push(json!({"role": "user", "content": concat_text(&m.content)}));
                }
            }
            Role::Assistant => {
                let text = concat_text(&m.content);
                let tool_calls: Vec<Value> = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolUse { id, name, input } => Some(json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": input.to_string(),
                            }
                        })),
                        _ => None,
                    })
                    .collect();
                let mut msg = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
                if !tool_calls.is_empty() {
                    msg["tool_calls"] = Value::Array(tool_calls);
                }
                out.push(msg);
            }
        }
    }
    out
}

fn concat_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn parse_completion(text: &str) -> Result<ChatResponse> {
    let v: Value = serde_json::from_str(text)
        .map_err(|_| protocol_error("OpenAI", ProtocolErrorKind::InvalidJson))?;
    if v.get("error").is_some() {
        return Err(protocol_error("OpenAI", ProtocolErrorKind::UpstreamError));
    }
    let choice = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| protocol_error("OpenAI", ProtocolErrorKind::InvalidSequence))?;
    let message = choice.get("message");
    let raw_finish = choice.get("finish_reason").and_then(|x| x.as_str());
    let stop_reason = parse_chat_finish(raw_finish)?;
    let mut budget = StreamBudget::default();
    let mut content = Vec::new();
    if let Some(t) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        && !t.is_empty()
    {
        budget.reserve_blocks(1, "OpenAI")?;
        let mut retained = String::new();
        budget.append_text(&mut retained, t, true, "OpenAI")?;
        content
            .try_reserve_exact(1)
            .map_err(|_| protocol_error("OpenAI", ProtocolErrorKind::LimitExceeded))?;
        content.push(ContentBlock::Text { text: retained });
    }
    if let Some(raw_tool_calls) = message.and_then(|message| message.get("tool_calls")) {
        let Some(tcs) = raw_tool_calls.as_array() else {
            if stop_reason == StopReason::MaxTokens {
                return build_nonstream_response(v, content, stop_reason, &mut budget);
            }
            return Err(protocol_error("OpenAI", ProtocolErrorKind::InvalidTool));
        };
        for tc in tcs {
            let raw_id = tc.get("id").and_then(Value::as_str).unwrap_or("");
            let raw_name = tc
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let args = tc
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            if raw_id.trim().is_empty() || raw_name.trim().is_empty() {
                if stop_reason == StopReason::MaxTokens {
                    continue;
                }
                return Err(protocol_error("OpenAI", ProtocolErrorKind::InvalidTool));
            }
            budget.reserve_tool_arguments(args.len(), "OpenAI")?;
            let Some(input) = parse_tool_args(args) else {
                if stop_reason == StopReason::MaxTokens {
                    continue;
                }
                return Err(protocol_error("OpenAI", ProtocolErrorKind::InvalidTool));
            };
            let mut id = String::new();
            budget.append_metadata(&mut id, raw_id, "OpenAI")?;
            let mut name = String::new();
            budget.append_metadata(&mut name, raw_name, "OpenAI")?;
            budget.reserve_blocks(1, "OpenAI")?;
            content
                .try_reserve_exact(1)
                .map_err(|_| protocol_error("OpenAI", ProtocolErrorKind::LimitExceeded))?;
            content.push(ContentBlock::ToolUse { id, name, input });
        }
    }
    build_nonstream_response(v, content, stop_reason, &mut budget)
}

fn build_nonstream_response(
    v: Value,
    content: Vec<ContentBlock>,
    stop_reason: StopReason,
    budget: &mut StreamBudget,
) -> Result<ChatResponse> {
    let mut model = String::new();
    if let Some(raw_model) = v.get("model").and_then(Value::as_str) {
        budget.append_metadata(&mut model, raw_model, "OpenAI")?;
    }
    let response = ChatResponse {
        model,
        content,
        stop_reason,
        input_tokens: v
            .pointer("/usage/prompt_tokens")
            .and_then(Value::as_u64)
            .map(crate::provider::saturating_token_count)
            .unwrap_or(0),
        output_tokens: v
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)
            .map(crate::provider::saturating_token_count)
            .unwrap_or(0),
        cached_tokens: crate::provider::cached_tokens_from(&v),
        cache_reported: crate::provider::cached_tokens_entry(&v).is_some(),
        limit: LimitHint::default(),
    };
    super::validate_chat_response(&response, "OpenAI")?;
    Ok(response)
}

fn map_openai_finish(raw: Option<&str>) -> StopReason {
    match raw {
        Some("stop") | Some("completed") => StopReason::EndTurn,
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        other => map_stop_reason(other),
    }
}

fn parse_chat_finish(raw: Option<&str>) -> Result<StopReason> {
    match raw {
        Some("stop") => Ok(StopReason::EndTurn),
        Some("tool_calls") => Ok(StopReason::ToolUse),
        Some("length") => Ok(StopReason::MaxTokens),
        Some(_) | None => Err(protocol_error("OpenAI", ProtocolErrorKind::InvalidSequence)),
    }
}

fn provider_label(mode: &CompatMode) -> &'static str {
    match mode {
        CompatMode::ChatCompletions => "OpenAI",
        CompatMode::CodexResponses => "Codex",
    }
}

fn api_error(status: u16, _body: &str, hint: &LimitHint, mode: &CompatMode) -> anyhow::Error {
    status_error(provider_label(mode), status, hint)
}

fn session_id() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(1)
    )
}

fn build_codex_body(req: &ChatRequest, stream: bool) -> Value {
    let mut input = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => {
                let text = concat_text(&m.content);
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": text}]
                    }));
                }
            }
            Role::User => {
                let results: Vec<Value> = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => Some(json!({
                            "type": "function_call_output",
                            "call_id": tool_use_id,
                            "output": content,
                        })),
                        _ => None,
                    })
                    .collect();
                if !results.is_empty() {
                    input.extend(results);
                } else {
                    let text = concat_text(&m.content);
                    if !text.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "user",
                            "content": [{"type": "input_text", "text": text}]
                        }));
                    }
                }
            }
            Role::Assistant => {
                let text = concat_text(&m.content);
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}]
                    }));
                }
                for b in &m.content {
                    if let ContentBlock::ToolUse {
                        id,
                        name,
                        input: args,
                    } = b
                    {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": args.to_string(),
                        }));
                    }
                }
            }
        }
    }
    let mut body = json!({
        "model": req.model,
        "instructions": req.system,
        "input": input,
        "store": false,
        "stream": stream,
    });
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    })
                })
                .collect(),
        );
    }
    body
}

fn parse_codex_response(text: &str) -> Result<ChatResponse> {
    let v: Value = serde_json::from_str(text)
        .map_err(|_| protocol_error("Codex", ProtocolErrorKind::InvalidJson))?;
    require_completed_codex_response(&v)?;
    let mut full_text = String::new();
    let mut tools = Vec::new();
    let mut budget = StreamBudget::default();
    let mut input_tokens = 0u32;
    let mut output_tokens = 0u32;
    let mut finish = None;
    if let Some(output) = v.get("output").or_else(|| v.pointer("/response/output")) {
        collect_codex_output(output, &mut full_text, &mut tools, &mut budget)?;
    }
    take_usage(&v, &mut input_tokens, &mut output_tokens);
    let cached_tokens = crate::provider::cached_tokens_from(&v);
    let cache_reported = crate::provider::cached_tokens_entry(&v).is_some();
    if let Some(status) = v
        .get("status")
        .or_else(|| v.pointer("/response/status"))
        .and_then(Value::as_str)
    {
        let mut retained_status = String::new();
        budget.append_metadata(&mut retained_status, status, "Codex")?;
        finish = Some(retained_status);
    }
    validate_tool_acc(&tools, finish.as_deref(), "Codex")?;
    let mut model = String::new();
    if let Some(raw_model) = v.get("model").and_then(Value::as_str) {
        budget.append_metadata(&mut model, raw_model, "Codex")?;
    }
    let response = finish_stream(
        full_text,
        tools,
        finish.as_deref(),
        input_tokens,
        output_tokens,
        cached_tokens,
        cache_reported,
        LimitHint::default(),
        model,
    );
    require_stream_content(&response, "Codex")?;
    Ok(response)
}

fn require_completed_codex_response(v: &Value) -> Result<()> {
    let status = v
        .get("status")
        .or_else(|| v.pointer("/response/status"))
        .and_then(Value::as_str);
    match status {
        Some("completed") => Ok(()),
        Some("failed" | "incomplete" | "cancelled") => {
            Err(protocol_error("Codex", ProtocolErrorKind::UpstreamError))
        }
        Some(_) | None => Err(protocol_error("Codex", ProtocolErrorKind::InvalidSequence)),
    }
}

fn codex_terminal_failure(v: &Value) -> Option<&str> {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
    if kind.ends_with(".failed") || kind.ends_with(".incomplete") || kind.ends_with(".cancelled") {
        return kind.rsplit('.').next();
    }
    let status = v.pointer("/response/status").and_then(Value::as_str)?;
    match status {
        "failed" | "incomplete" | "cancelled" => Some(status),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)] // codex 이벤트 필드가 그대로 인자로 대응된다
fn apply_codex_event<F>(
    v: &Value,
    full_text: &mut String,
    tools: &mut Vec<(String, String, String)>,
    input_tokens: &mut u32,
    output_tokens: &mut u32,
    cached_tokens: &mut u32,
    cache_reported: &mut bool,
    finish: &mut Option<String>,
    reasoning_started: &mut bool,
    budget: &mut StreamBudget,
    mut on_event: Option<&mut F>,
    max_tokens: u32,
) -> Result<bool>
where
    F: FnMut(SemanticStreamEvent),
{
    let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    if kind.contains("reasoning")
        && kind.ends_with(".delta")
        && let Some(delta) = v.get("delta").and_then(|x| x.as_str())
        && !delta.is_empty()
        && let Some(cb) = on_event.as_mut()
    {
        budget.reserve_ephemeral_text(delta, "Codex")?;
        if !*reasoning_started {
            cb(SemanticStreamEvent::ReasoningText("\n[모델 작업]\n"));
            *reasoning_started = true;
        }
        cb(SemanticStreamEvent::ReasoningText(delta));
    }
    if (kind.ends_with("output_text.delta") || kind == "response.output_text.delta")
        && let Some(delta) = v.get("delta").and_then(|x| x.as_str())
        && !delta.is_empty()
    {
        if full_text.is_empty() {
            budget.reserve_blocks(1, "Codex")?;
        }
        budget.append_text(full_text, delta, true, "Codex")?;
        if *reasoning_started {
            if let Some(cb) = on_event.as_mut() {
                cb(SemanticStreamEvent::ReasoningText("\n[/모델 작업]\n"));
            }
            *reasoning_started = false;
        }
        if let Some(cb) = on_event.as_mut() {
            cb(SemanticStreamEvent::ContentCandidate(delta));
        }
    }
    if (kind.ends_with("output_item.done") || kind == "response.output_item.done")
        && let Some(item) = v.get("item")
    {
        if *reasoning_started {
            if let Some(cb) = on_event.as_mut() {
                cb(SemanticStreamEvent::ReasoningText("\n[/모델 작업]\n"));
            }
            *reasoning_started = false;
        }
        collect_codex_item(item, full_text, tools, budget)?;
    }
    let completed = kind == "response.completed";
    if completed {
        if *reasoning_started {
            if let Some(cb) = on_event.as_mut() {
                cb(SemanticStreamEvent::ReasoningText("\n[/모델 작업]\n"));
            }
            *reasoning_started = false;
        }
        let mut completed_reason = String::new();
        budget.append_metadata(&mut completed_reason, "completed", "Codex")?;
        *finish = Some(completed_reason);
        if let Some(resp) = v.get("response") {
            take_stream_usage(
                resp,
                input_tokens,
                output_tokens,
                cached_tokens,
                cache_reported,
            );
        }
        take_stream_usage(
            v,
            input_tokens,
            output_tokens,
            cached_tokens,
            cache_reported,
        );
    }
    take_stream_usage(
        v,
        input_tokens,
        output_tokens,
        cached_tokens,
        cache_reported,
    );
    let output_limit_reached = max_tokens > 0
        && (*output_tokens > max_tokens
            || (completed && *output_tokens == 0 && (!full_text.is_empty() || !tools.is_empty())));
    if output_limit_reached {
        *finish = Some("max_tokens".into());
    }
    Ok(output_limit_reached)
}

fn collect_codex_output(
    output: &Value,
    full_text: &mut String,
    tools: &mut Vec<(String, String, String)>,
    budget: &mut StreamBudget,
) -> Result<()> {
    let Some(arr) = output.as_array() else {
        return Ok(());
    };
    for item in arr {
        collect_codex_item(item, full_text, tools, budget)?;
    }
    Ok(())
}

fn collect_codex_item(
    item: &Value,
    full_text: &mut String,
    tools: &mut Vec<(String, String, String)>,
    budget: &mut StreamBudget,
) -> Result<()> {
    let kind = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
    if kind == "function_call" {
        let id_piece = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let name_piece = item.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let args_piece = item
            .get("arguments")
            .and_then(|x| x.as_str())
            .unwrap_or("{");
        budget.reserve_blocks(1, "Codex")?;
        tools
            .try_reserve_exact(1)
            .map_err(|_| protocol_error("Codex", ProtocolErrorKind::LimitExceeded))?;
        let mut id = String::new();
        let mut name = String::new();
        let mut args = String::new();
        budget.append_metadata(&mut id, id_piece, "Codex")?;
        budget.append_metadata(&mut name, name_piece, "Codex")?;
        budget.append_tool_arguments(&mut args, args_piece, "Codex")?;
        tools.push((id, name, args));
        return Ok(());
    }
    if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
        for part in content {
            if let Some(t) = part.get("text").and_then(|x| x.as_str())
                && !t.is_empty()
                && !full_text.contains(t)
            {
                if full_text.is_empty() {
                    budget.reserve_blocks(1, "Codex")?;
                }
                budget.append_text(full_text, t, true, "Codex")?;
            }
        }
    }
    Ok(())
}

fn take_usage(v: &Value, input_tokens: &mut u32, output_tokens: &mut u32) {
    if let Some(n) = v
        .pointer("/usage/input_tokens")
        .or_else(|| v.pointer("/usage/prompt_tokens"))
        .and_then(|x| x.as_u64())
    {
        *input_tokens = crate::provider::saturating_token_count(n);
    }
    if let Some(n) = v
        .pointer("/usage/output_tokens")
        .or_else(|| v.pointer("/usage/completion_tokens"))
        .and_then(|x| x.as_u64())
    {
        *output_tokens = crate::provider::saturating_token_count(n);
    }
}

fn take_stream_usage(
    v: &Value,
    input_tokens: &mut u32,
    output_tokens: &mut u32,
    cached_tokens: &mut u32,
    cache_reported: &mut bool,
) {
    take_usage(v, input_tokens, output_tokens);
    if let Some(cached) = crate::provider::cached_tokens_entry(v) {
        *cached_tokens = cached;
        *cache_reported = true;
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn sse_provider(body: String, codex: bool) -> OpenAiCompatProvider {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n{body}"
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        });
        let base_url = format!("http://{address}");
        if codex {
            OpenAiCompatProvider::with_codex_oauth_at(base_url).unwrap()
        } else {
            OpenAiCompatProvider::new(base_url, None).unwrap()
        }
    }

    async fn oversized_body_provider() -> OpenAiCompatProvider {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                crate::provider::MAX_RESPONSE_BODY_BYTES + 1
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        OpenAiCompatProvider::new(format!("http://{address}"), None).unwrap()
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

    #[test]
    fn codex_stream_labels_reasoning_and_content_candidates_separately() {
        let mut full_text = String::new();
        let mut tools = Vec::new();
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        let mut cached_tokens = 0;
        let mut cache_reported = false;
        let mut finish = None;
        let mut reasoning_started = false;
        let mut budget = StreamBudget::default();
        let mut kinds = Vec::new();
        let mut callback = |event: SemanticStreamEvent<'_>| match event {
            SemanticStreamEvent::ReasoningText(_) => kinds.push("reasoning"),
            SemanticStreamEvent::ContentCandidate(_) => kinds.push("candidate"),
            SemanticStreamEvent::ToolArgs { .. } => kinds.push("tool"),
        };

        apply_codex_event(
            &json!({"type": "response.reasoning_summary_text.delta", "delta": "검토"}),
            &mut full_text,
            &mut tools,
            &mut input_tokens,
            &mut output_tokens,
            &mut cached_tokens,
            &mut cache_reported,
            &mut finish,
            &mut reasoning_started,
            &mut budget,
            Some(&mut callback),
            1024,
        )
        .unwrap();
        apply_codex_event(
            &json!({"type": "response.output_text.delta", "delta": "완료"}),
            &mut full_text,
            &mut tools,
            &mut input_tokens,
            &mut output_tokens,
            &mut cached_tokens,
            &mut cache_reported,
            &mut finish,
            &mut reasoning_started,
            &mut budget,
            Some(&mut callback),
            1024,
        )
        .unwrap();

        assert_eq!(kinds, ["reasoning", "reasoning", "reasoning", "candidate"]);
        assert_eq!(full_text, "완료");
    }

    #[test]
    fn codex_done_closes_an_open_reasoning_marker() {
        let mut full_text = String::new();
        let mut tools = Vec::new();
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        let mut cached_tokens = 0;
        let mut cache_reported = false;
        let mut finish = None;
        let mut reasoning_started = false;
        let mut budget = StreamBudget::default();
        let mut events = Vec::new();
        let mut callback = |event: SemanticStreamEvent<'_>| {
            if let SemanticStreamEvent::ReasoningText(text) = event {
                events.push(text.to_string());
            }
        };

        apply_codex_event(
            &json!({"type": "response.reasoning_summary_text.delta", "delta": "검토"}),
            &mut full_text,
            &mut tools,
            &mut input_tokens,
            &mut output_tokens,
            &mut cached_tokens,
            &mut cache_reported,
            &mut finish,
            &mut reasoning_started,
            &mut budget,
            Some(&mut callback),
            1024,
        )
        .unwrap();
        close_codex_reasoning(&mut reasoning_started, &mut callback);

        assert_eq!(events, ["\n[모델 작업]\n", "검토", "\n[/모델 작업]\n"]);
        assert!(!reasoning_started);
    }

    #[test]
    fn codex_backend_body_omits_unsupported_output_limit() {
        let request = ChatRequest {
            model: "gpt-5.6-sol".into(),
            system: "system".into(),
            messages: vec![Message::user_text("build it")],
            tools: Vec::new(),
            max_tokens: 32_768,
            stream: true,
        };
        let body = build_codex_body(&request, true);
        assert!(body.get("max_output_tokens").is_none());
        assert_eq!(body["model"], "gpt-5.6-sol");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn codex_client_limit_uses_reported_tokens_and_drops_over_budget_tools() {
        let response = ChatResponse {
            content: vec![
                ContentBlock::Text {
                    text: "가나다".into(),
                },
                ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "write_file".into(),
                    input: json!({"path": "large.txt", "content": "overflow"}),
                },
            ],
            stop_reason: StopReason::ToolUse,
            input_tokens: 1,
            output_tokens: 12,
            cached_tokens: 0,
            model: "gpt-5.6-sol".into(),
            cache_reported: false,
            limit: LimitHint::default(),
        };
        let guarded = enforce_codex_output_limit(response, 2);
        assert_eq!(guarded.stop_reason, StopReason::MaxTokens);
        assert_eq!(guarded.content.len(), 1);
        assert!(matches!(
            &guarded.content[0],
            ContentBlock::Text { text } if text == "가나다"
        ));
    }

    #[test]
    fn codex_client_limit_does_not_reinterpret_utf8_bytes_as_tokens() {
        let response = ChatResponse {
            content: vec![
                ContentBlock::Text {
                    text: "한 토큰으로 보고된 긴 텍스트".into(),
                },
                ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "write_file".into(),
                    input: json!({"path": "valid.txt", "content": "ok"}),
                },
            ],
            stop_reason: StopReason::ToolUse,
            input_tokens: 1,
            output_tokens: 1,
            cached_tokens: 0,
            model: "gpt-5.6-sol".into(),
            cache_reported: false,
            limit: LimitHint::default(),
        };
        let guarded = enforce_codex_output_limit(response, 1);
        assert_eq!(guarded.stop_reason, StopReason::ToolUse);
        assert_eq!(guarded.content.len(), 2);
    }

    #[test]
    fn codex_client_fails_closed_when_output_usage_is_missing() {
        let response = ChatResponse {
            content: vec![ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "bash".into(),
                input: json!({"command": "true"}),
            }],
            stop_reason: StopReason::ToolUse,
            input_tokens: 1,
            output_tokens: 0,
            cached_tokens: 0,
            model: "gpt-5.6-sol".into(),
            cache_reported: false,
            limit: LimitHint::default(),
        };
        let guarded = enforce_codex_output_limit(response, 1);
        assert_eq!(guarded.stop_reason, StopReason::MaxTokens);
        assert!(guarded.content.is_empty());
    }

    #[test]
    fn codex_client_saturates_oversized_reported_usage_before_limiting() {
        let usage = json!({
            "usage": {
                "input_tokens": 4_294_967_297u64,
                "output_tokens": 4_294_967_297u64
            }
        });
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        take_usage(&usage, &mut input_tokens, &mut output_tokens);
        assert_eq!(input_tokens, u32::MAX);
        assert_eq!(output_tokens, u32::MAX);

        let response = ChatResponse {
            content: vec![ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "bash".into(),
                input: json!({"command": "true"}),
            }],
            stop_reason: StopReason::ToolUse,
            input_tokens,
            output_tokens,
            cached_tokens: 0,
            model: "gpt-5.6-sol".into(),
            cache_reported: false,
            limit: LimitHint::default(),
        };
        let guarded = enforce_codex_output_limit(response, 32_768);
        assert_eq!(guarded.stop_reason, StopReason::MaxTokens);
        assert!(guarded.content.is_empty());
    }

    #[test]
    fn codex_nonstream_requires_completed_status_and_content() {
        for status in ["failed", "incomplete", "cancelled", "in_progress"] {
            let err = parse_codex_response(&json!({"status": status}).to_string()).unwrap_err();
            assert!(
                err.downcast_ref::<crate::provider::ProtocolError>()
                    .is_some()
            );
        }
        let err = parse_codex_response(&json!({"status": "completed"}).to_string()).unwrap_err();
        assert!(format!("{err:#}").contains("empty response"));
    }

    #[test]
    fn codex_terminal_failures_are_not_completed_events() {
        for event in [
            json!({"type": "response.failed"}),
            json!({"type": "response.incomplete"}),
            json!({"type": "response.cancelled"}),
            json!({"type": "response.output_item.done", "response": {"status": "failed"}}),
        ] {
            assert!(codex_terminal_failure(&event).is_some());
        }
        assert!(codex_terminal_failure(&json!({"type": "response.completed"})).is_none());
    }

    #[test]
    fn stream_completion_requires_content() {
        let response = stream(Vec::new(), Some("stop"));
        assert!(require_stream_content(&response, "OpenAI 호환").is_err());
    }

    #[tokio::test]
    async fn done_only_chat_stream_is_rejected() {
        let provider = sse_provider("data: [DONE]\n\n".into(), false).await;
        let err = provider
            .chat_semantic_stream(&streaming_request(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<crate::provider::ProtocolError>()
                .map(crate::provider::ProtocolError::kind),
            Some(ProtocolErrorKind::InvalidSequence)
        );
    }

    #[tokio::test]
    async fn malformed_only_chat_stream_is_rejected() {
        let provider = sse_provider("data: not-json\n\ndata: [DONE]\n\n".into(), false).await;
        let err = provider
            .chat_semantic_stream(&streaming_request(), |_| {})
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("malformed JSON"));
    }

    #[tokio::test]
    async fn chat_stream_requires_done_after_finish_reason() {
        let provider = sse_provider(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"stop\"}]}\n\n"
                .into(),
            false,
        )
        .await;
        let error = provider
            .chat_semantic_stream(&streaming_request(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::provider::ProtocolError>()
                .map(crate::provider::ProtocolError::kind),
            Some(ProtocolErrorKind::TruncatedStream)
        );
    }

    #[tokio::test]
    async fn chat_stream_rejects_missing_or_unknown_finish_reason() {
        for body in [
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"valid\"}}]}\n\n",
                "data: [DONE]\n\n"
            ),
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"valid\"},\"finish_reason\":\"content_filter\"}]}\n\n",
                "data: [DONE]\n\n"
            ),
        ] {
            let provider = sse_provider(body.into(), false).await;
            let error = provider
                .chat_semantic_stream(&streaming_request(), |_| {})
                .await
                .unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<crate::provider::ProtocolError>()
                    .map(crate::provider::ProtocolError::kind),
                Some(ProtocolErrorKind::InvalidSequence)
            );
        }
    }

    #[tokio::test]
    async fn oversized_chat_text_and_tool_deltas_are_not_emitted() {
        let oversized = "x".repeat(crate::provider::MAX_SSE_FRAME_BYTES);
        let bodies = [
            format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{oversized}\"}}}}]}}\n\n"),
            format!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call\",\"function\":{{\"name\":\"write\",\"arguments\":\"{oversized}\"}}}}]}}}}]}}\n\n"
            ),
        ];
        for body in bodies {
            let provider = sse_provider(body, false).await;
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

    #[tokio::test]
    async fn malformed_or_error_event_fails_before_later_valid_event() {
        for prefix in [
            "data: not-json\n\n",
            "data: {\"error\":{\"message\":\"secret\"}}\n\n",
        ] {
            let body = format!(
                "{prefix}data: {{\"choices\":[{{\"delta\":{{\"content\":\"late\"}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
            );
            let provider = sse_provider(body, false).await;
            let error = provider
                .chat_semantic_stream(&streaming_request(), |_| {})
                .await
                .unwrap_err();
            assert!(
                error
                    .downcast_ref::<crate::provider::ProtocolError>()
                    .is_some()
            );
            assert!(!format!("{error:#}").contains("secret"));
        }
    }

    #[tokio::test]
    async fn successful_nonstream_body_is_bounded() {
        let provider = oversized_body_provider().await;
        let error = provider.chat(&streaming_request()).await.unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::provider::ProtocolError>()
                .map(crate::provider::ProtocolError::kind),
            Some(ProtocolErrorKind::LimitExceeded)
        );
    }

    #[test]
    fn delimiter_free_frame_and_accumulators_are_bounded() {
        let mut decoder = SseDecoder::new(SseDelimiter::Line);
        for _ in 0..crate::provider::MAX_SSE_FRAME_BYTES {
            assert!(decoder.push(b'x', "OpenAI").unwrap().is_none());
        }
        assert_eq!(decoder.pending_len(), crate::provider::MAX_SSE_FRAME_BYTES);
        assert!(decoder.push(b'x', "OpenAI").is_err());

        let mut budget = StreamBudget::default();
        let mut text = String::new();
        let text_limit = "x".repeat(crate::provider::MAX_RESPONSE_TEXT_BYTES);
        budget
            .append_text(&mut text, &text_limit, true, "OpenAI")
            .unwrap();
        assert!(budget.append_text(&mut text, "x", true, "OpenAI").is_err());

        let mut arguments = String::new();
        let mut budget = StreamBudget::default();
        let argument_limit = "x".repeat(MAX_TOOL_ARGUMENT_BYTES);
        budget
            .append_tool_arguments(&mut arguments, &argument_limit, "OpenAI")
            .unwrap();
        assert!(
            budget
                .append_tool_arguments(&mut arguments, "x", "OpenAI")
                .is_err()
        );
    }

    #[test]
    fn empty_nonstream_and_malformed_tool_with_text_fail() {
        let empty = json!({"choices":[{"message":{},"finish_reason":"stop"}]}).to_string();
        assert!(parse_completion(&empty).is_err());
        let malformed = json!({
            "choices": [{
                "message": {
                    "content": "surviving text",
                    "tool_calls": [{"id":"call","function":{"name":"write","arguments":"{"}}]
                },
                "finish_reason": "stop"
            }]
        })
        .to_string();
        assert!(parse_completion(&malformed).is_err());

        for tool_calls in [
            json!({}),
            json!([{"function":{"name":"write","arguments":"{}"}}]),
            json!([{"id":"call","function":{"arguments":"{}"}}]),
        ] {
            let response = json!({
                "choices":[{
                    "message":{"content":"surviving text","tool_calls":tool_calls},
                    "finish_reason":"stop"
                }]
            });
            assert!(parse_completion(&response.to_string()).is_err());
        }
    }

    #[test]
    fn nonstream_requires_supported_finish_reason() {
        for finish in [Value::Null, json!("completed"), json!("content_filter")] {
            let value = json!({
                "choices":[{"message":{"content":"valid"},"finish_reason":finish}]
            });
            let error = parse_completion(&value.to_string()).unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<crate::provider::ProtocolError>()
                    .map(crate::provider::ProtocolError::kind),
                Some(ProtocolErrorKind::InvalidSequence)
            );
        }
    }

    #[test]
    fn nonstream_parse_failures_are_typed() {
        let missing_choices = parse_completion(r#"{"model":"test"}"#).unwrap_err();
        assert_eq!(
            missing_choices
                .downcast_ref::<crate::provider::ProtocolError>()
                .map(crate::provider::ProtocolError::kind),
            Some(ProtocolErrorKind::InvalidSequence)
        );

        let malformed_codex = parse_codex_response("not-json").unwrap_err();
        assert_eq!(
            malformed_codex
                .downcast_ref::<crate::provider::ProtocolError>()
                .map(crate::provider::ProtocolError::kind),
            Some(ProtocolErrorKind::InvalidJson)
        );
    }

    #[test]
    fn nonstream_enforces_total_block_and_metadata_caps() {
        let tools = (0..255)
            .map(|index| {
                json!({
                    "id":format!("call-{index}"),
                    "function":{"name":"tool","arguments":"{}"}
                })
            })
            .collect::<Vec<_>>();
        let accepted = json!({
            "choices":[{
                "message":{"content":"text","tool_calls":tools},
                "finish_reason":"tool_calls"
            }]
        });
        assert_eq!(
            parse_completion(&accepted.to_string())
                .unwrap()
                .content
                .len(),
            crate::provider::MAX_CONTENT_BLOCKS
        );

        let mut overflow = accepted;
        overflow["choices"][0]["message"]["tool_calls"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id":"call-overflow",
                "function":{"name":"tool","arguments":"{}"}
            }));
        let error = parse_completion(&overflow.to_string()).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::provider::ProtocolError>()
                .map(crate::provider::ProtocolError::kind),
            Some(ProtocolErrorKind::LimitExceeded)
        );

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
                "choices":[{
                    "message":{"tool_calls":[{
                        "id":id,
                        "function":{"name":name,"arguments":"{}"}
                    }]},
                    "finish_reason":"tool_calls"
                }]
            });
            assert!(parse_completion(&value.to_string()).is_err());
        }
    }

    #[test]
    fn nonstream_enforces_aggregate_text_tool_and_state_caps() {
        let oversized_text = json!({
            "choices":[{
                "message":{"content":"x".repeat(crate::provider::MAX_RESPONSE_TEXT_BYTES + 1)},
                "finish_reason":"stop"
            }]
        });
        assert!(parse_completion(&oversized_text.to_string()).is_err());

        let oversized_tool = json!({
            "choices":[{
                "message":{"tool_calls":[{
                    "id":"call",
                    "function":{
                        "name":"tool",
                        "arguments":format!("{{\"value\":\"{}\"}}", "x".repeat(crate::provider::MAX_TOOL_ARGUMENT_BYTES))
                    }
                }]},
                "finish_reason":"tool_calls"
            }]
        });
        assert!(parse_completion(&oversized_tool.to_string()).is_err());

        let half = crate::provider::MAX_TOOL_ARGUMENT_BYTES / 2;
        let argument = |size| format!("{{\"value\":\"{}\"}}", "x".repeat(size));
        let aggregate_tools = json!({
            "choices":[{
                "message":{"tool_calls":[
                    {"id":"a","function":{"name":"tool","arguments":argument(half)}},
                    {"id":"b","function":{"name":"tool","arguments":argument(half)}}
                ]},
                "finish_reason":"tool_calls"
            }]
        });
        assert!(parse_completion(&aggregate_tools.to_string()).is_err());

        let state_overflow = json!({
            "choices":[{
                "message":{
                    "content":"x".repeat(crate::provider::MAX_RESPONSE_TEXT_BYTES),
                    "tool_calls":[{
                        "id":"call",
                        "function":{"name":"tool","arguments":argument(crate::provider::MAX_TOOL_ARGUMENT_BYTES - 12)}
                    }]
                },
                "finish_reason":"tool_calls"
            }]
        });
        assert!(parse_completion(&state_overflow.to_string()).is_err());
    }

    #[tokio::test]
    async fn chat_stream_preserves_interleaved_parallel_tool_indices() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"one\",\"arguments\":\"{\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"b\",\"function\":{\"name\":\"two\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let provider = sse_provider(body.into(), false).await;
        let response = provider
            .chat_semantic_stream(&streaming_request(), |_| {})
            .await
            .unwrap();
        assert_eq!(response.content.len(), 2);
        assert_eq!(response.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn codex_only_exact_response_completed_is_terminal() {
        let mut text = String::new();
        let mut tools = Vec::new();
        let mut input = 0;
        let mut output = 0;
        let mut cached = 0;
        let mut cache_reported = false;
        let mut finish = None;
        let mut reasoning = false;
        let mut budget = StreamBudget::default();
        let mut callback = |_: SemanticStreamEvent<'_>| {};
        for event_type in ["response.output_item.completed", "future.completed"] {
            assert!(
                !apply_codex_event(
                    &json!({"type":event_type}),
                    &mut text,
                    &mut tools,
                    &mut input,
                    &mut output,
                    &mut cached,
                    &mut cache_reported,
                    &mut finish,
                    &mut reasoning,
                    &mut budget,
                    Some(&mut callback),
                    1024,
                )
                .unwrap()
            );
            assert!(finish.is_none());
        }
        assert!(
            !apply_codex_event(
                &json!({"type":"response.completed"}),
                &mut text,
                &mut tools,
                &mut input,
                &mut output,
                &mut cached,
                &mut cache_reported,
                &mut finish,
                &mut reasoning,
                &mut budget,
                Some(&mut callback),
                1024,
            )
            .unwrap()
        );
        assert_eq!(finish.as_deref(), Some("completed"));
    }

    #[test]
    fn max_token_truncation_preserves_surviving_text() {
        let response = finish_stream(
            "keep me".into(),
            vec![("call".into(), "write".into(), "{".into())],
            Some("length"),
            0,
            0,
            0,
            false,
            LimitHint::default(),
            String::new(),
        );
        validate_tool_acc(
            &[("call".into(), "write".into(), "{".into())],
            Some("length"),
            "OpenAI",
        )
        .unwrap();
        assert!(matches!(
            response.content.as_slice(),
            [ContentBlock::Text { text }] if text == "keep me"
        ));
        assert_eq!(response.stop_reason, StopReason::MaxTokens);

        let nonstream = json!({
            "choices":[{
                "message":{
                    "content":"keep me",
                    "tool_calls":[{"function":{"name":"write","arguments":"{}"}}]
                },
                "finish_reason":"length"
            }]
        });
        let response = parse_completion(&nonstream.to_string()).unwrap();
        assert!(matches!(
            response.content.as_slice(),
            [ContentBlock::Text { text }] if text == "keep me"
        ));
        assert_eq!(response.stop_reason, StopReason::MaxTokens);
    }

    #[tokio::test]
    async fn codex_completed_event_returns_without_waiting_for_next_chunk() {
        let provider = sse_provider(
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ready\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
            )
            .into(),
            true,
        )
        .await;
        let response = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            provider.chat_semantic_stream(&streaming_request(), |_| {}),
        )
        .await
        .expect("completed event must finish without another network chunk")
        .unwrap();
        assert!(matches!(
            response.content.as_slice(),
            [ContentBlock::Text { text }] if text == "ready"
        ));
    }

    #[test]
    fn codex_stream_waits_for_authoritative_usage_before_limiting() {
        let event = json!({"type": "response.output_text.delta", "delta": "가나다"});
        let mut full_text = String::new();
        let mut tools = Vec::new();
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        let mut cached_tokens = 0;
        let mut cache_reported = false;
        let mut finish = None;
        fn ignore_stream_event(_: SemanticStreamEvent<'_>) {}
        let mut callback = ignore_stream_event;
        let mut reasoning_started = false;
        let mut budget = StreamBudget::default();
        let limited = apply_codex_event(
            &event,
            &mut full_text,
            &mut tools,
            &mut input_tokens,
            &mut output_tokens,
            &mut cached_tokens,
            &mut cache_reported,
            &mut finish,
            &mut reasoning_started,
            &mut budget,
            Some(&mut callback),
            2,
        )
        .unwrap();
        assert!(!limited);
        assert_eq!(full_text, "가나다");
        assert_eq!(finish, None);

        let completed = json!({
            "type": "response.completed",
            "response": {"usage": {"input_tokens": 1, "output_tokens": 3}}
        });
        let limited = apply_codex_event(
            &completed,
            &mut full_text,
            &mut tools,
            &mut input_tokens,
            &mut output_tokens,
            &mut cached_tokens,
            &mut cache_reported,
            &mut finish,
            &mut reasoning_started,
            &mut budget,
            Some(&mut callback),
            2,
        )
        .unwrap();
        assert!(limited);
        assert_eq!(output_tokens, 3);
        assert_eq!(finish.as_deref(), Some("max_tokens"));
    }

    #[test]
    fn rejected_codex_text_delta_is_never_emitted() {
        let event = json!({
            "type": "response.output_text.delta",
            "delta": "x".repeat(crate::provider::MAX_RESPONSE_TEXT_BYTES + 1)
        });
        let mut full_text = String::new();
        let mut tools = Vec::new();
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        let mut cached_tokens = 0;
        let mut cache_reported = false;
        let mut finish = None;
        let mut reasoning_started = false;
        let mut budget = StreamBudget::default();
        let mut emitted = 0usize;
        let mut callback = |_: SemanticStreamEvent<'_>| emitted += 1;
        let result = apply_codex_event(
            &event,
            &mut full_text,
            &mut tools,
            &mut input_tokens,
            &mut output_tokens,
            &mut cached_tokens,
            &mut cache_reported,
            &mut finish,
            &mut reasoning_started,
            &mut budget,
            Some(&mut callback),
            1024,
        );
        assert!(result.is_err());
        assert!(full_text.is_empty());
        assert_eq!(emitted, 0);
    }

    #[test]
    fn tool_args_progress_fires_every_step_per_tool() {
        let mut marks: Vec<usize> = Vec::new();
        // 임계 미만은 조용하다.
        assert!(!tool_args_due(&mut marks, 0, 1));
        assert!(!tool_args_due(&mut marks, 0, TOOL_ARGS_STEP - 1));
        // 임계를 넘긴 첫 순간에 한 번.
        assert!(tool_args_due(&mut marks, 0, TOOL_ARGS_STEP));
        // 발행 직후에는 다시 조용해지고, 다음 8KB 에서 또 한 번.
        assert!(!tool_args_due(&mut marks, 0, TOOL_ARGS_STEP + 10));
        assert!(tool_args_due(&mut marks, 0, TOOL_ARGS_STEP * 2));
        // 도구마다 독립적으로 센다 — 0번의 누적이 1번의 발행을 삼키지 않는다.
        assert!(!tool_args_due(&mut marks, 1, 100));
        assert!(tool_args_due(&mut marks, 1, TOOL_ARGS_STEP));
        assert_eq!(marks.len(), 2);
    }

    fn stream(tool_acc: Vec<(String, String, String)>, finish: Option<&str>) -> ChatResponse {
        finish_stream(
            String::new(),
            tool_acc,
            finish,
            0,
            0,
            0,
            false,
            LimitHint::default(),
            String::new(),
        )
    }

    #[test]
    fn truncated_tool_call_is_dropped_and_reports_max_tokens() {
        // max_tokens 절단 — 인자 JSON이 중간에서 끊긴 tool call 은 실행 대상이 아니다.
        let resp = stream(
            vec![(
                "call_1".into(),
                "write_file".into(),
                r#"{"path":"index.html","content":"<html>..."#.into(),
            )],
            Some("length"),
        );
        assert!(resp.content.is_empty());
        assert_eq!(resp.stop_reason, StopReason::MaxTokens);
    }

    #[test]
    fn complete_tool_calls_survive_alongside_truncated_one() {
        let resp = stream(
            vec![
                (
                    "call_1".into(),
                    "todo_write".into(),
                    r#"{"items":[]}"#.into(),
                ),
                (
                    "call_2".into(),
                    "write_file".into(),
                    r#"{"path":"a.html","content":"잘린"#.into(),
                ),
            ],
            Some("length"),
        );
        let names: Vec<_> = resp
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["todo_write"]);
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn empty_args_tool_call_still_runs_with_empty_object() {
        let resp = stream(
            vec![("call_1".into(), "list_todos".into(), String::new())],
            Some("tool_calls"),
        );
        match &resp.content[..] {
            [ContentBlock::ToolUse { name, input, .. }] => {
                assert_eq!(name, "list_todos");
                assert_eq!(input, &json!({}));
            }
            other => panic!("unexpected content: {other:?}"),
        }
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn streaming_usage_keeps_cached_prompt_tokens() {
        let event = json!({
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 80,
                "prompt_tokens_details": {"cached_tokens": 900}
            }
        });
        let mut input = 0;
        let mut output = 0;
        let mut cached = 0;
        let mut reported = false;
        take_stream_usage(&event, &mut input, &mut output, &mut cached, &mut reported);
        assert_eq!((input, output, cached), (1200, 80, 900));
        assert!(reported);
    }

    #[test]
    fn api_error_discards_untrusted_body_before_persistent_logging() {
        let echoed_bearer = "unlabeled-echoed-bearer-7f8db8d8";
        let body = format!("upstream detail: {echoed_bearer}");

        for status in [429, 500] {
            let error = api_error(
                status,
                &body,
                &LimitHint {
                    retry_after_secs: Some(12),
                    ..LimitHint::default()
                },
                &CompatMode::ChatCompletions,
            );
            for persistent_log_message in [
                format!("{error}"),
                format!("{error:#}"),
                format!("{error:?}"),
            ] {
                if status != 429 {
                    assert!(persistent_log_message.contains(&format!("HTTP {status}")));
                }
                assert!(
                    !persistent_log_message.contains(echoed_bearer),
                    "persistent log leaked untrusted provider body: {persistent_log_message}"
                );
                assert!(
                    !persistent_log_message.contains("upstream detail"),
                    "persistent log retained provider response body: {persistent_log_message}"
                );
            }
        }
    }
}
