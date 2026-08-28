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
    /// 공급자가 실제로 답한 모델 — 요청과 다륵면 호출부가 경고한다 (모델 미검증 방지).
    /// 공급자가 보고하지 않으면 빈 문자열.
    pub model: String,
    /// 공급자 응답에 캐시 사용량 필드가 실제 포함됐는지 여부.
    pub cache_reported: bool,
    pub limit: LimitHint,
}

/// 응답 JSON에서 캐시 히트 토큰을 끌어낸다 (Anthropic·OpenAI 호환 형식 모두).
pub fn cached_tokens_from(v: &serde_json::Value) -> u32 {
    cached_tokens_entry(v).unwrap_or(0)
}

pub fn cached_tokens_entry(v: &serde_json::Value) -> Option<u32> {
    for ptr in [
        "/usage/cache_read_input_tokens",
        "/usage/prompt_tokens_details/cached_tokens",
        "/usage/cached_content_token_count",
    ] {
        if let Some(n) = v.pointer(ptr).and_then(|x| x.as_u64()) {
            return Some(n as u32);
        }
    }
    None
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

/// 스트림 진행 이벤트. 텍스트가 한 조각도 없는 구간(대형 tool call 인자 생성)에서도
/// "모델이 무엇을 하는 중인가"를 소비자에게 알리기 위한 통로다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvent<'a> {
    /// 화면으로 흘러나가는 응답 조각 (본문·추론 텍스트).
    Text(&'a str),
    /// 도구 호출 인자를 누적 중이라는 진행 신호. 화면 출력이 아니라 상태 표시용이다.
    ToolArgs { name: &'a str, total_bytes: usize },
}

/// 이 이벤트가 화면으로 내보낸 문자 수. ToolArgs 는 진행 표시일 뿐 출력이 아니므로 0이다
/// — 이 구분이 없으면 "이미 출력이 나갔다"는 판정이 오염돼 폴백이 막힌다.
pub fn emitted_chars(event: &StreamEvent) -> usize {
    match event {
        StreamEvent::Text(s) => s.chars().count(),
        StreamEvent::ToolArgs { .. } => 0,
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

    pub async fn chat_stream<F>(&self, req: &ChatRequest, on_event: F) -> Result<ChatResponse>
    where
        F: FnMut(StreamEvent),
    {
        match self {
            DynProvider::Anthropic(p) => p.chat_stream(req, on_event).await,
            DynProvider::OpenAi(p) => p.chat_stream(req, on_event).await,
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
        || s.contains("스트림이 종료되었습니다")
        || s.contains("스트림이 중간에 끊겼습니다")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_text_events_count_as_screen_output() {
        assert_eq!(emitted_chars(&StreamEvent::Text("가나다")), 3);
        assert_eq!(emitted_chars(&StreamEvent::Text("")), 0);
        // 진행 신호는 화면에 답을 흘린 것이 아니다 — 재시도·폴백 판정을 막으면 안 된다.
        assert_eq!(
            emitted_chars(&StreamEvent::ToolArgs {
                name: "write_file",
                total_bytes: 16384,
            }),
            0
        );
    }
}
