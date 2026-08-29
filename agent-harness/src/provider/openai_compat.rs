use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{
    ChatRequest, ChatResponse, ContentBlock, LimitHint, Message, Role, StopReason, StreamEvent,
    limit_hint, map_stop_reason, rate_limit_error,
};

const CODEX_RESPONSES: &str = "https://chatgpt.com/backend-api/codex/responses";
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

/// 멀티바이트(한글 3바이트)가 청크 경계에서 잘려도 깨지지 않게
/// 바이트 버퍼에서 '\n' 위치까지만 안전하게 잘라낸다.
fn drain_line(buf: &mut Vec<u8>) -> Option<String> {
    let pos = buf.iter().position(|&b| b == b'\n')?;
    let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
    Some(String::from_utf8_lossy(&line_bytes).into_owned())
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
            "https://chatgpt.com/backend-api/codex".into(),
            Some(token),
            CompatMode::CodexResponses,
            account_id,
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
            CompatMode::CodexResponses => CODEX_RESPONSES.to_string(),
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
        let resp = builder
            .json(&body)
            .send()
            .await
            .context("OpenAI 호환 API 요청에 실패했습니다")?;
        let status = resp.status();
        let hint = limit_hint(resp.headers());
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &text, &hint, &self.mode));
        }
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
            .context("OpenAI 호환 API 요청에 실패했습니다")?;
        let status = resp.status();
        let hint = limit_hint(resp.headers());
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(api_error(status.as_u16(), &text, &hint, &self.mode));
        }

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
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
            let chunk = chunk.context("응답 스트림이 중간에 끊겼습니다")?;
            buf.extend_from_slice(&chunk);
            while let Some(line) = drain_line(&mut buf) {
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
                        on_event(StreamEvent::Text("\n[/모델 작업]\n"));
                    }
                    return Ok(finish_stream(
                        full_text,
                        tool_acc,
                        finish.as_deref(),
                        input_tokens,
                        output_tokens,
                        cached_tokens,
                        cache_reported,
                        hint.clone(),
                        stream_model.clone(),
                    ));
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if stream_model.is_empty()
                    && let Some(m) = v.get("model").and_then(|x| x.as_str())
                {
                    stream_model = m.to_string();
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
                if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str())
                    && !fr.is_empty()
                    && fr != "null"
                {
                    finish = Some(fr.to_string());
                }
                if let Some(delta) = choice.get("delta") {
                    if let Some(piece) = delta.get("content").and_then(|x| x.as_str())
                        && !piece.is_empty()
                    {
                        if reasoning_started {
                            on_event(StreamEvent::Text("\n[/모델 작업]\n"));
                            reasoning_started = false;
                        }
                        on_event(StreamEvent::Text(piece));
                        full_text.push_str(piece);
                    }
                    if let Some(piece) = delta
                        .get("reasoning_content")
                        .or_else(|| delta.get("reasoning"))
                        .and_then(|x| x.as_str())
                        && !piece.is_empty()
                    {
                        if !reasoning_started {
                            on_event(StreamEvent::Text("\n[모델 작업]\n"));
                            reasoning_started = true;
                        }
                        on_event(StreamEvent::Text(piece));
                    }
                    if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
                        for tc in tcs {
                            let idx =
                                tc.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                            while tool_acc.len() <= idx {
                                tool_acc.push((String::new(), String::new(), String::new()));
                            }
                            if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                                tool_acc[idx].0.push_str(id);
                            }
                            if let Some(name) =
                                tc.pointer("/function/name").and_then(|x| x.as_str())
                            {
                                tool_acc[idx].1.push_str(name);
                            }
                            if let Some(args) =
                                tc.pointer("/function/arguments").and_then(|x| x.as_str())
                            {
                                tool_acc[idx].2.push_str(args);
                                // 대형 인자(수 KB~수십 KB) 생성 구간은 텍스트가 한 조각도
                                // 흐르지 않는다 — 누적량을 진행 신호로 내보내 침묵을 없앤다.
                                let total = tool_acc[idx].2.len();
                                if tool_args_due(&mut args_marks, idx, total) {
                                    let name = if tool_acc[idx].1.is_empty() {
                                        "tool"
                                    } else {
                                        tool_acc[idx].1.as_str()
                                    };
                                    on_event(StreamEvent::ToolArgs {
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

        // 여기까지 도달했다는 것은 [DONE] 없이 연결이 닫혔다 뜻 — 미완료 응답이다.
        if finish.is_none() && full_text.is_empty() && tool_acc.is_empty() {
            anyhow::bail!("응답이 시작되기 전에 스트림이 종료되었습니다");
        }
        if finish.is_none() {
            anyhow::bail!(
                "응답이 완료되기 전에 스트림이 종료되었습니다 ({}자 수신)",
                full_text.chars().count()
            );
        }
        if reasoning_started {
            on_event(StreamEvent::Text("\n[/모델 작업]\n"));
        }

        Ok(finish_stream(
            full_text,
            tool_acc,
            finish.as_deref(),
            input_tokens,
            output_tokens,
            cached_tokens,
            cache_reported,
            hint,
            stream_model,
        ))
    }

    async fn chat_codex_stream<F>(&self, req: &ChatRequest, mut on_event: F) -> Result<ChatResponse>
    where
        F: FnMut(StreamEvent),
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
            .context("Codex API 요청에 실패했습니다")?;
        let stream_model = String::new();
        let status = resp.status();
        let hint = limit_hint(resp.headers());
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(api_error(status.as_u16(), &text, &hint, &self.mode));
        }

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut full_text = String::new();
        let mut tools: Vec<(String, String, String)> = Vec::new();
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;
        let mut cached_tokens = 0u32;
        let mut cache_reported = false;
        let mut finish = None::<String>;
        let mut finished = false;

        while let Some(chunk) = stream.next().await {
            if finished {
                break;
            }
            let chunk = chunk.context("Codex 응답 스트림이 중간에 끊겼습니다")?;
            buf.extend_from_slice(&chunk);
            while let Some(line) = drain_line(&mut buf) {
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
                    finished = true;
                    break;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                let output_limit_reached = apply_codex_event(
                    &v,
                    &mut full_text,
                    &mut tools,
                    &mut input_tokens,
                    &mut output_tokens,
                    &mut cached_tokens,
                    &mut cache_reported,
                    &mut finish,
                    Some(&mut on_event),
                    req.max_tokens,
                );
                if output_limit_reached {
                    tools.clear();
                    finished = true;
                    break;
                }
                if finish.is_some() {
                    finished = true;
                }
            }
        }

        if !finished && full_text.is_empty() && tools.is_empty() {
            anyhow::bail!("응답이 시작되기 전에 Codex 스트림이 종료되었습니다");
        }
        if !finished {
            anyhow::bail!(
                "응답이 완료되기 전에 Codex 스트림이 종료되었습니다 ({}자 수신)",
                full_text.chars().count()
            );
        }

        Ok(enforce_codex_output_limit(
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
        ))
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
    serde_json::from_str(trimmed).ok()
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
        if name.is_empty() {
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
    let v: Value =
        serde_json::from_str(text).context("OpenAI 호환 응답 JSON을 해석할 수 없습니다")?;
    let choice = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow!("choices 가 없습니다"))?;
    let message = choice.get("message").cloned().unwrap_or(json!({}));
    let mut content = Vec::new();
    if let Some(t) = message.get("content").and_then(|x| x.as_str())
        && !t.is_empty()
    {
        content.push(ContentBlock::Text {
            text: t.to_string(),
        });
    }
    if let Some(tcs) = message.get("tool_calls").and_then(|x| x.as_array()) {
        for tc in tcs {
            let id = tc
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let name = tc
                .pointer("/function/name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let args = tc
                .pointer("/function/arguments")
                .and_then(|x| x.as_str())
                .unwrap_or("{}");
            let Some(input) = parse_tool_args(args) else {
                continue;
            };
            content.push(ContentBlock::ToolUse { id, name, input });
        }
    }
    Ok(ChatResponse {
        model: v.get("model").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        content,
        stop_reason: map_openai_finish(choice.get("finish_reason").and_then(|x| x.as_str())),
        input_tokens: v
            .pointer("/usage/prompt_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        output_tokens: v
            .pointer("/usage/completion_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        cached_tokens: crate::provider::cached_tokens_from(&v),
        cache_reported: crate::provider::cached_tokens_entry(&v).is_some(),
        limit: LimitHint::default(),
    })
}

fn map_openai_finish(raw: Option<&str>) -> StopReason {
    match raw {
        Some("stop") => StopReason::EndTurn,
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        other => map_stop_reason(other),
    }
}

fn api_error(status: u16, body: &str, hint: &LimitHint, mode: &CompatMode) -> anyhow::Error {
    if status == 429 {
        return rate_limit_error(status, body, hint);
    }
    let snippet: String = body.chars().take(400).collect();
    if status == 401 {
        return match mode {
            CompatMode::CodexResponses => anyhow!(
                "ChatGPT 로그인이 거절됐습니다 (HTTP 401). settings 에서 OpenAI를 다시 연결하세요."
            ),
            CompatMode::ChatCompletions => {
                anyhow!("OpenAI 호환 API 인증 실패 (HTTP 401). API 키를 확인하세요.")
            }
        };
    }
    anyhow!("OpenAI 호환 API 오류 HTTP {status}: {snippet}")
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
    let v: Value = serde_json::from_str(text).context("Codex 응답 JSON을 해석할 수 없습니다")?;
    let mut full_text = String::new();
    let mut tools = Vec::new();
    let mut input_tokens = 0u32;
    let mut output_tokens = 0u32;
    let mut finish = None;
    if let Some(output) = v.get("output").or_else(|| v.pointer("/response/output")) {
        collect_codex_output(output, &mut full_text, &mut tools);
    }
    take_usage(&v, &mut input_tokens, &mut output_tokens);
    let cached_tokens = crate::provider::cached_tokens_from(&v);
    let cache_reported = crate::provider::cached_tokens_entry(&v).is_some();
    if let Some(status) = v.get("status").and_then(|x| x.as_str()) {
        finish = Some(status.to_string());
    }
    Ok(finish_stream(
        full_text,
        tools,
        finish.as_deref(),
        input_tokens,
        output_tokens,
        cached_tokens,
        cache_reported,
        LimitHint::default(),
        v.get("model").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
    ))
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
    mut on_event: Option<&mut F>,
    max_tokens: u32,
) -> bool
where
    F: FnMut(StreamEvent),
{
    let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    if (kind.ends_with("output_text.delta") || kind == "response.output_text.delta")
        && let Some(delta) = v.get("delta").and_then(|x| x.as_str())
        && !delta.is_empty()
    {
        full_text.push_str(delta);
        if let Some(cb) = on_event.as_mut() {
            cb(StreamEvent::Text(delta));
        }
    }
    if (kind.ends_with("output_item.done") || kind == "response.output_item.done")
        && let Some(item) = v.get("item")
    {
        collect_codex_item(item, full_text, tools);
    }
    let completed = kind.ends_with("completed") || kind == "response.completed";
    if completed {
        *finish = Some("completed".into());
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
    output_limit_reached
}

fn collect_codex_output(
    output: &Value,
    full_text: &mut String,
    tools: &mut Vec<(String, String, String)>,
) {
    let Some(arr) = output.as_array() else { return };
    for item in arr {
        collect_codex_item(item, full_text, tools);
    }
}

fn collect_codex_item(
    item: &Value,
    full_text: &mut String,
    tools: &mut Vec<(String, String, String)>,
) {
    let kind = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
    if kind == "function_call" {
        let id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let name = item
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let args = item
            .get("arguments")
            .and_then(|x| x.as_str())
            .unwrap_or("{}")
            .to_string();
        if !name.is_empty() {
            tools.push((id, name, args));
        }
        return;
    }
    if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
        for part in content {
            if let Some(t) = part.get("text").and_then(|x| x.as_str())
                && !t.is_empty()
                && !full_text.contains(t)
            {
                full_text.push_str(t);
            }
        }
    }
}

fn take_usage(v: &Value, input_tokens: &mut u32, output_tokens: &mut u32) {
    if let Some(n) = v
        .pointer("/usage/input_tokens")
        .or_else(|| v.pointer("/usage/prompt_tokens"))
        .and_then(|x| x.as_u64())
    {
        *input_tokens = n as u32;
    }
    if let Some(n) = v
        .pointer("/usage/output_tokens")
        .or_else(|| v.pointer("/usage/completion_tokens"))
        .and_then(|x| x.as_u64())
    {
        *output_tokens = n as u32;
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
    fn codex_stream_waits_for_authoritative_usage_before_limiting() {
        let event = json!({"type": "response.output_text.delta", "delta": "가나다"});
        let mut full_text = String::new();
        let mut tools = Vec::new();
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        let mut cached_tokens = 0;
        let mut cache_reported = false;
        let mut finish = None;
        fn ignore_stream_event(_: StreamEvent<'_>) {}
        let mut callback = ignore_stream_event;
        let limited = apply_codex_event(
            &event,
            &mut full_text,
            &mut tools,
            &mut input_tokens,
            &mut output_tokens,
            &mut cached_tokens,
            &mut cache_reported,
            &mut finish,
            Some(&mut callback),
            2,
        );
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
            Some(&mut callback),
            2,
        );
        assert!(limited);
        assert_eq!(output_tokens, 3);
        assert_eq!(finish.as_deref(), Some("max_tokens"));
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
}
