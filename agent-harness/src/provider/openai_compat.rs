use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{
    ChatRequest, ChatResponse, ContentBlock, LimitHint, Message, Role, StopReason, limit_hint,
    map_stop_reason, rate_limit_error,
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
        if let Some(key) = &self.api_key {
            if !key.is_empty() {
                req = req.header("Authorization", format!("Bearer {key}"));
            }
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
        parsed.limit = hint;
        Ok(parsed)
    }

    pub async fn chat_stream<F>(&self, req: &ChatRequest, mut on_text: F) -> Result<ChatResponse>
    where
        F: FnMut(&str),
    {
        if matches!(self.mode, CompatMode::CodexResponses) {
            return self.chat_codex_stream(req, on_text).await;
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
                        on_text("\n[/모델 작업]\n");
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
                    ));
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
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
                if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
                    if !fr.is_empty() && fr != "null" {
                        finish = Some(fr.to_string());
                    }
                }
                if let Some(delta) = choice.get("delta") {
                    if let Some(piece) = delta.get("content").and_then(|x| x.as_str()) {
                        if !piece.is_empty() {
                            if reasoning_started {
                                on_text("\n[/모델 작업]\n");
                                reasoning_started = false;
                            }
                            on_text(piece);
                            full_text.push_str(piece);
                        }
                    }
                    if let Some(piece) = delta
                        .get("reasoning_content")
                        .or_else(|| delta.get("reasoning"))
                        .and_then(|x| x.as_str())
                    {
                        if !piece.is_empty() {
                            if !reasoning_started {
                                on_text("\n[모델 작업]\n");
                                reasoning_started = true;
                            }
                            on_text(piece);
                        }
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
            on_text("\n[/모델 작업]\n");
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
        ))
    }

    async fn chat_codex_stream<F>(&self, req: &ChatRequest, mut on_text: F) -> Result<ChatResponse>
    where
        F: FnMut(&str),
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
                apply_codex_event(
                    &v,
                    &mut full_text,
                    &mut tools,
                    &mut input_tokens,
                    &mut output_tokens,
                    &mut cached_tokens,
                    &mut cache_reported,
                    &mut finish,
                    Some(&mut on_text),
                );
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

        Ok(finish_stream(
            full_text,
            tools,
            finish.as_deref(),
            input_tokens,
            output_tokens,
            cached_tokens,
            cache_reported,
            hint,
        ))
    }
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

fn finish_stream(
    full_text: String,
    tool_acc: Vec<(String, String, String)>,
    finish: Option<&str>,
    input_tokens: u32,
    output_tokens: u32,
    cached_tokens: u32,
    cache_reported: bool,
    limit: LimitHint,
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
    if let Some(t) = message.get("content").and_then(|x| x.as_str()) {
        if !t.is_empty() {
            content.push(ContentBlock::Text {
                text: t.to_string(),
            });
        }
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
    if req.max_tokens > 0 {
        body["max_output_tokens"] = json!(req.max_tokens);
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
    ))
}

fn apply_codex_event<F>(
    v: &Value,
    full_text: &mut String,
    tools: &mut Vec<(String, String, String)>,
    input_tokens: &mut u32,
    output_tokens: &mut u32,
    cached_tokens: &mut u32,
    cache_reported: &mut bool,
    finish: &mut Option<String>,
    mut on_text: Option<&mut F>,
) where
    F: FnMut(&str),
{
    let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    if kind.ends_with("output_text.delta") || kind == "response.output_text.delta" {
        if let Some(delta) = v.get("delta").and_then(|x| x.as_str()) {
            if !delta.is_empty() {
                full_text.push_str(delta);
                if let Some(cb) = on_text.as_mut() {
                    cb(delta);
                }
            }
        }
    }
    if kind.ends_with("output_item.done") || kind == "response.output_item.done" {
        if let Some(item) = v.get("item") {
            collect_codex_item(item, full_text, tools);
        }
    }
    if kind.ends_with("completed") || kind == "response.completed" {
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
            if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                if !t.is_empty() && !full_text.contains(t) {
                    full_text.push_str(t);
                }
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
