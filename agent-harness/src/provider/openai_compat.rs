use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{
    ChatRequest, ChatResponse, ContentBlock, Message, Role, StopReason, map_stop_reason,
};

pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
}

impl OpenAiCompatProvider {
    pub fn new(base_url: String, api_key: Option<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("HTTP 클라이언트를 만들 수 없습니다")?;
        Ok(Self {
            client,
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    fn url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(key) = &self.api_key {
            if !key.is_empty() {
                return req.header("Authorization", format!("Bearer {key}"));
            }
        }
        req
    }

    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let body = build_body(req, false);
        let builder = self.apply_auth(self.client.post(self.url()).header("content-type", "application/json"));
        let resp = builder
            .json(&body)
            .send()
            .await
            .context("OpenAI 호환 API 요청에 실패했습니다")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &text));
        }
        parse_completion(&text)
    }

    pub async fn chat_stream<F>(&self, req: &ChatRequest, mut on_text: F) -> Result<ChatResponse>
    where
        F: FnMut(&str),
    {
        let body = build_body(req, true);
        let builder = self.apply_auth(self.client.post(self.url()).header("content-type", "application/json"));
        let resp = builder
            .json(&body)
            .send()
            .await
            .context("OpenAI 호환 API 요청에 실패했습니다")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(api_error(status.as_u16(), &text));
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut full_text = String::new();
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;
        let mut finish = None::<String>;
        let mut tool_acc: Vec<(String, String, String)> = Vec::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("응답 스트림을 읽는 중 오류")?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let mut line = buf[..pos].to_string();
                buf = buf[pos + 1..].to_string();
                if line.ends_with('\r') {
                    line.pop();
                }
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(finish_stream(
                        full_text,
                        tool_acc,
                        finish.as_deref(),
                        input_tokens,
                        output_tokens,
                    ));
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                if let Some(n) = v.pointer("/usage/prompt_tokens").and_then(|x| x.as_u64()) {
                    input_tokens = n as u32;
                }
                if let Some(n) = v.pointer("/usage/completion_tokens").and_then(|x| x.as_u64()) {
                    output_tokens = n as u32;
                }
                let Some(choice) = v.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) else {
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
                            on_text(piece);
                            full_text.push_str(piece);
                        }
                    }
                    if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
                        for tc in tcs {
                            let idx = tc.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                            while tool_acc.len() <= idx {
                                tool_acc.push((String::new(), String::new(), String::new()));
                            }
                            if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                                tool_acc[idx].0.push_str(id);
                            }
                            if let Some(name) = tc.pointer("/function/name").and_then(|x| x.as_str()) {
                                tool_acc[idx].1.push_str(name);
                            }
                            if let Some(args) = tc.pointer("/function/arguments").and_then(|x| x.as_str())
                            {
                                tool_acc[idx].2.push_str(args);
                            }
                        }
                    }
                }
            }
        }

        Ok(finish_stream(
            full_text,
            tool_acc,
            finish.as_deref(),
            input_tokens,
            output_tokens,
        ))
    }
}

fn finish_stream(
    full_text: String,
    tool_acc: Vec<(String, String, String)>,
    finish: Option<&str>,
    input_tokens: u32,
    output_tokens: u32,
) -> ChatResponse {
    let mut content = Vec::new();
    if !full_text.is_empty() {
        content.push(ContentBlock::Text { text: full_text });
    }
    for (id, name, args) in tool_acc {
        if name.is_empty() {
            continue;
        }
        let input = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
        content.push(ContentBlock::ToolUse { id, name, input });
    }
    ChatResponse {
        content,
        stop_reason: map_openai_finish(finish),
        input_tokens,
        output_tokens,
    }
}

fn build_body(req: &ChatRequest, stream: bool) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": to_openai_messages(&req.system, &req.messages),
        "max_tokens": req.max_tokens,
        "stream": stream,
    });
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
    let v: Value = serde_json::from_str(text).context("OpenAI 호환 응답 JSON을 해석할 수 없습니다")?;
    let choice = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow!("choices 가 없습니다"))?;
    let message = choice.get("message").cloned().unwrap_or(json!({}));
    let mut content = Vec::new();
    if let Some(t) = message.get("content").and_then(|x| x.as_str()) {
        if !t.is_empty() {
            content.push(ContentBlock::Text { text: t.to_string() });
        }
    }
    if let Some(tcs) = message.get("tool_calls").and_then(|x| x.as_array()) {
        for tc in tcs {
            let id = tc.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let name = tc
                .pointer("/function/name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let args = tc
                .pointer("/function/arguments")
                .and_then(|x| x.as_str())
                .unwrap_or("{}");
            let input = serde_json::from_str(args).unwrap_or_else(|_| json!({}));
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

fn api_error(status: u16, body: &str) -> anyhow::Error {
    let snippet: String = body.chars().take(400).collect();
    if status == 401 {
        anyhow!("OpenAI 호환 API 인증 실패 (HTTP 401). API 키를 확인하세요.")
    } else {
        anyhow!("OpenAI 호환 API 오류 HTTP {status}: {snippet}")
    }
}
