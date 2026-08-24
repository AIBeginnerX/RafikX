pub mod anthropic;
pub mod openai_compat;

pub use anthropic::AnthropicProvider;
pub use openai_compat::OpenAiCompatProvider;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    pub stream: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LimitHint {
    pub retry_after_secs: Option<u64>,
    pub remaining: Option<u32>,
    pub reset_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    #[allow(dead_code)]
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// 프롬프트 캐시 히트 토큰 (제공자가 보고할 때만 0보다 크다).
    pub cached_tokens: u32,
    pub limit: LimitHint,
}

/// 응답 JSON에서 캐시 히트 토큰을 끌어낸다 (Anthropic·OpenAI 호환 형식 모두).
pub fn cached_tokens_from(v: &serde_json::Value) -> u32 {
    for ptr in [
        "/usage/cache_read_input_tokens",
        "/usage/prompt_tokens_details/cached_tokens",
        "/usage/cached_content_token_count",
    ] {
        if let Some(n) = v.pointer(ptr).and_then(|x| x.as_u64()) {
            return n as u32;
        }
    }
    0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other,
}

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

pub fn map_stop_reason(raw: Option<&str>) -> StopReason {
    match raw {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        _ => StopReason::Other,
    }
}

pub enum DynProvider {
    Anthropic(AnthropicProvider),
    OpenAi(OpenAiCompatProvider),
}

impl DynProvider {
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        match self {
            DynProvider::Anthropic(p) => p.chat(req).await,
            DynProvider::OpenAi(p) => p.chat(req).await,
        }
    }

    pub async fn chat_stream<F>(&self, req: &ChatRequest, on_text: F) -> Result<ChatResponse>
    where
        F: FnMut(&str),
    {
        match self {
            DynProvider::Anthropic(p) => p.chat_stream(req, on_text).await,
            DynProvider::OpenAi(p) => p.chat_stream(req, on_text).await,
        }
    }
}

pub fn is_retryable(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}").to_lowercase();
    is_rate_limited(err)
        || s.contains("http 500")
        || s.contains("http 502")
        || s.contains("http 503")
        || s.contains("http 504")
        || s.contains("timeout")
        || s.contains("timed out")
}

pub fn is_rate_limited(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}").to_lowercase();
    s.contains("http 429")
        || s.contains("rate_limited")
        || s.contains("rate limit")
        || s.contains("too many requests")
        || s.contains("overloaded")
}

pub fn limit_hint(headers: &reqwest::header::HeaderMap) -> LimitHint {
    let mut h = LimitHint::default();
    if let Some(v) = header_u64(headers, "retry-after") {
        h.retry_after_secs = Some(v);
    }
    if let Some(v) = header_u32(headers, "anthropic-ratelimit-requests-remaining")
        .or_else(|| header_u32(headers, "x-ratelimit-remaining-requests"))
        .or_else(|| header_u32(headers, "x-ratelimit-remaining"))
    {
        h.remaining = Some(v);
    }
    if let Some(v) = header_u64(headers, "anthropic-ratelimit-requests-reset")
        .or_else(|| header_u64(headers, "x-ratelimit-reset-requests"))
        .or_else(|| header_u64(headers, "x-ratelimit-reset"))
    {
        if v > 1_000_000_000 {
            h.reset_at = Some(v as i64);
        } else {
            h.retry_after_secs = h.retry_after_secs.or(Some(v));
            h.reset_at = Some(crate::usage::now_secs() + v as i64);
        }
    }
    h
}

fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse().ok())
}

fn header_u32(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u32> {
    header_u64(headers, name).map(|n| n as u32)
}

pub fn rate_limit_error(status: u16, body: &str, hint: &LimitHint) -> anyhow::Error {
    let snippet: String = body.chars().take(200).collect();
    let wait = hint.retry_after_secs.unwrap_or(45);
    anyhow!("HTTP {status} rate_limited retry_after={wait} {snippet}")
}
