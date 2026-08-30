pub mod anthropic;
pub mod openai_compat;

pub use anthropic::AnthropicProvider;
pub use openai_compat::OpenAiCompatProvider;

use anyhow::Result;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_RESPONSE_BODY_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_SSE_FRAME_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_RESPONSE_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_TOOL_ARGUMENT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_STREAM_STATE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_STREAM_METADATA_BYTES: usize = 256 * 1024;
pub(crate) const MAX_METADATA_FIELD_BYTES: usize = 4 * 1024;
pub(crate) const MAX_CONTENT_BLOCKS: usize = 256;
const STREAM_BLOCK_OVERHEAD_BYTES: usize = 128;
const MAX_RETRY_AFTER_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRequestErrorKind {
    Auth { status: u16 },
    RateLimited { retry_after: u64 },
    Server { status: u16 },
    Client { status: u16 },
    Timeout,
    Connect,
    BodyRead,
    Transport,
}

#[derive(Debug)]
pub(crate) struct ProviderRequestError {
    provider: &'static str,
    kind: ProviderRequestErrorKind,
}

impl ProviderRequestError {
    pub(crate) const fn new(provider: &'static str, kind: ProviderRequestErrorKind) -> Self {
        Self { provider, kind }
    }

    #[cfg(test)]
    pub(crate) const fn provider(&self) -> &'static str {
        self.provider
    }

    pub(crate) const fn kind(&self) -> ProviderRequestErrorKind {
        self.kind
    }
}

impl std::fmt::Display for ProviderRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ProviderRequestErrorKind::Auth { status } => {
                write!(
                    formatter,
                    "{} authentication failed (HTTP {status})",
                    self.provider
                )
            }
            ProviderRequestErrorKind::RateLimited { retry_after } => write!(
                formatter,
                "{} rate limited (retry after {retry_after}s)",
                self.provider
            ),
            ProviderRequestErrorKind::Server { status } => {
                write!(formatter, "{} server error (HTTP {status})", self.provider)
            }
            ProviderRequestErrorKind::Client { status } => {
                write!(
                    formatter,
                    "{} request rejected (HTTP {status})",
                    self.provider
                )
            }
            ProviderRequestErrorKind::Timeout => {
                write!(formatter, "{} request timed out", self.provider)
            }
            ProviderRequestErrorKind::Connect => {
                write!(formatter, "{} connection failed", self.provider)
            }
            ProviderRequestErrorKind::BodyRead => {
                write!(formatter, "{} response body read failed", self.provider)
            }
            ProviderRequestErrorKind::Transport => {
                write!(formatter, "{} transport failed", self.provider)
            }
        }
    }
}

impl std::error::Error for ProviderRequestError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestErrorPhase {
    Send,
    BodyRead,
}

pub(crate) fn request_error(
    provider: &'static str,
    error: &reqwest::Error,
    phase: RequestErrorPhase,
) -> anyhow::Error {
    let kind = match phase {
        RequestErrorPhase::BodyRead => ProviderRequestErrorKind::BodyRead,
        RequestErrorPhase::Send if error.is_timeout() => ProviderRequestErrorKind::Timeout,
        RequestErrorPhase::Send if error.is_connect() => ProviderRequestErrorKind::Connect,
        RequestErrorPhase::Send => ProviderRequestErrorKind::Transport,
    };
    ProviderRequestError::new(provider, kind).into()
}

pub(crate) fn status_error(provider: &'static str, status: u16, hint: &LimitHint) -> anyhow::Error {
    let kind = match status {
        401 | 403 => ProviderRequestErrorKind::Auth { status },
        429 => ProviderRequestErrorKind::RateLimited {
            retry_after: hint
                .retry_after_secs
                .unwrap_or(45)
                .min(MAX_RETRY_AFTER_SECS),
        },
        500..=599 => ProviderRequestErrorKind::Server { status },
        _ => ProviderRequestErrorKind::Client { status },
    };
    ProviderRequestError::new(provider, kind).into()
}

pub(crate) fn safe_summary(error: &anyhow::Error) -> String {
    if let Some(error) = error.downcast_ref::<ProviderRequestError>() {
        return error.to_string();
    }
    if let Some(error) = error.downcast_ref::<ProtocolError>() {
        return error.to_string();
    }
    "provider operation failed".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SseDelimiter {
    Line,
    Event,
}

pub(crate) struct SseDecoder {
    pending: Vec<u8>,
    delimiter: SseDelimiter,
}

impl SseDecoder {
    pub(crate) const fn new(delimiter: SseDelimiter) -> Self {
        Self {
            pending: Vec::new(),
            delimiter,
        }
    }

    pub(crate) fn push(&mut self, byte: u8, provider: &'static str) -> Result<Option<Vec<u8>>> {
        if self.delimiter == SseDelimiter::Line && byte == b'\n' {
            return Ok(Some(std::mem::take(&mut self.pending)));
        }
        if self.pending.len() >= MAX_SSE_FRAME_BYTES {
            return Err(protocol_error(provider, ProtocolErrorKind::LimitExceeded));
        }
        self.pending.push(byte);
        if self.delimiter == SseDelimiter::Event {
            let delimiter_len = if self.pending.ends_with(b"\r\n\r\n") {
                4
            } else if self.pending.ends_with(b"\n\n") {
                2
            } else {
                0
            };
            if delimiter_len > 0 {
                let mut frame = std::mem::take(&mut self.pending);
                frame.truncate(frame.len() - delimiter_len);
                return Ok(Some(frame));
            }
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[derive(Default)]
pub(crate) struct StreamBudget {
    text_bytes: usize,
    tool_argument_bytes: usize,
    metadata_bytes: usize,
    retained_bytes: usize,
    content_blocks: usize,
}

impl StreamBudget {
    fn checked_total(
        current: usize,
        added: usize,
        limit: usize,
        provider: &'static str,
    ) -> Result<usize> {
        let next = current
            .checked_add(added)
            .ok_or_else(|| protocol_error(provider, ProtocolErrorKind::LimitExceeded))?;
        if next > limit {
            return Err(protocol_error(provider, ProtocolErrorKind::LimitExceeded));
        }
        Ok(next)
    }

    fn reserve_retained(&mut self, added: usize, provider: &'static str) -> Result<()> {
        self.retained_bytes =
            Self::checked_total(self.retained_bytes, added, MAX_STREAM_STATE_BYTES, provider)?;
        Ok(())
    }

    pub(crate) fn append_text(
        &mut self,
        target: &mut String,
        piece: &str,
        retained: bool,
        provider: &'static str,
    ) -> Result<()> {
        let next = Self::checked_total(
            self.text_bytes,
            piece.len(),
            MAX_RESPONSE_TEXT_BYTES,
            provider,
        )?;
        if retained {
            self.reserve_retained(piece.len(), provider)?;
        }
        target
            .try_reserve_exact(piece.len())
            .map_err(|_| protocol_error(provider, ProtocolErrorKind::LimitExceeded))?;
        self.text_bytes = next;
        target.push_str(piece);
        Ok(())
    }

    pub(crate) fn reserve_ephemeral_text(
        &mut self,
        piece: &str,
        provider: &'static str,
    ) -> Result<()> {
        self.text_bytes = Self::checked_total(
            self.text_bytes,
            piece.len(),
            MAX_RESPONSE_TEXT_BYTES,
            provider,
        )?;
        Ok(())
    }

    pub(crate) fn append_tool_arguments(
        &mut self,
        target: &mut String,
        piece: &str,
        provider: &'static str,
    ) -> Result<()> {
        self.reserve_tool_arguments(piece.len(), provider)?;
        target
            .try_reserve_exact(piece.len())
            .map_err(|_| protocol_error(provider, ProtocolErrorKind::LimitExceeded))?;
        target.push_str(piece);
        Ok(())
    }

    pub(crate) fn reserve_tool_arguments(
        &mut self,
        added: usize,
        provider: &'static str,
    ) -> Result<()> {
        let next = Self::checked_total(
            self.tool_argument_bytes,
            added,
            MAX_TOOL_ARGUMENT_BYTES,
            provider,
        )?;
        self.reserve_retained(added, provider)?;
        self.tool_argument_bytes = next;
        Ok(())
    }

    pub(crate) fn append_metadata(
        &mut self,
        target: &mut String,
        piece: &str,
        provider: &'static str,
    ) -> Result<()> {
        let field_bytes = Self::checked_total(
            target.len(),
            piece.len(),
            MAX_METADATA_FIELD_BYTES,
            provider,
        )?;
        let next = Self::checked_total(
            self.metadata_bytes,
            piece.len(),
            MAX_STREAM_METADATA_BYTES,
            provider,
        )?;
        self.reserve_retained(piece.len(), provider)?;
        target
            .try_reserve_exact(piece.len())
            .map_err(|_| protocol_error(provider, ProtocolErrorKind::LimitExceeded))?;
        self.metadata_bytes = next;
        target.push_str(piece);
        debug_assert_eq!(target.len(), field_bytes);
        Ok(())
    }

    pub(crate) fn reserve_blocks(&mut self, added: usize, provider: &'static str) -> Result<()> {
        let next = Self::checked_total(self.content_blocks, added, MAX_CONTENT_BLOCKS, provider)?;
        let retained = added
            .checked_mul(STREAM_BLOCK_OVERHEAD_BYTES)
            .ok_or_else(|| protocol_error(provider, ProtocolErrorKind::LimitExceeded))?;
        self.reserve_retained(retained, provider)?;
        self.content_blocks = next;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolErrorKind {
    EmptyResponse,
    InvalidTool,
    InvalidJson,
    InvalidSequence,
    TruncatedStream,
    UpstreamError,
    LimitExceeded,
}

#[derive(Debug)]
pub(crate) struct ProtocolError {
    provider: &'static str,
    kind: ProtocolErrorKind,
}

impl ProtocolError {
    pub(crate) const fn new(provider: &'static str, kind: ProtocolErrorKind) -> Self {
        Self { provider, kind }
    }

    pub(crate) const fn kind(&self) -> ProtocolErrorKind {
        self.kind
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let detail = match self.kind {
            ProtocolErrorKind::EmptyResponse => "empty response",
            ProtocolErrorKind::InvalidTool => "invalid tool call",
            ProtocolErrorKind::InvalidJson => "malformed JSON",
            ProtocolErrorKind::InvalidSequence => "invalid event sequence",
            ProtocolErrorKind::TruncatedStream => "truncated stream",
            ProtocolErrorKind::UpstreamError => "upstream error event",
            ProtocolErrorKind::LimitExceeded => "response size limit exceeded",
        };
        write!(formatter, "{} protocol error: {detail}", self.provider)
    }
}

impl std::error::Error for ProtocolError {}

pub(crate) fn protocol_error(provider: &'static str, kind: ProtocolErrorKind) -> anyhow::Error {
    ProtocolError::new(provider, kind).into()
}

pub(crate) async fn read_bounded_body(
    response: reqwest::Response,
    provider: &'static str,
) -> Result<String> {
    if response.content_length().is_some_and(|length| {
        usize::try_from(length).map_or(true, |length| length > MAX_RESPONSE_BODY_BYTES)
    }) {
        return Err(protocol_error(provider, ProtocolErrorKind::LimitExceeded));
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|error| request_error(provider, &error, RequestErrorPhase::BodyRead))?;
        let next = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| protocol_error(provider, ProtocolErrorKind::LimitExceeded))?;
        if next > MAX_RESPONSE_BODY_BYTES {
            return Err(protocol_error(provider, ProtocolErrorKind::LimitExceeded));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| protocol_error(provider, ProtocolErrorKind::InvalidJson))
}

pub(crate) fn validate_chat_response(
    response: &ChatResponse,
    provider: &'static str,
) -> Result<()> {
    let mut meaningful = false;
    for block in &response.content {
        match block {
            ContentBlock::Text { text } => meaningful |= !text.trim().is_empty(),
            ContentBlock::ToolUse { id, name, input } => {
                if id.trim().is_empty() || name.trim().is_empty() || !input.is_object() {
                    return Err(protocol_error(provider, ProtocolErrorKind::InvalidTool));
                }
                meaningful = true;
            }
            ContentBlock::ToolResult { .. } => {}
        }
    }
    if !meaningful {
        return Err(protocol_error(provider, ProtocolErrorKind::EmptyResponse));
    }
    Ok(())
}

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
            return Some(saturating_token_count(n));
        }
    }
    None
}

pub(crate) fn saturating_token_count(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
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
        Some("end_turn" | "stop_sequence") => StopReason::EndTurn,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticStreamEvent<'a> {
    ReasoningText(&'a str),
    ContentCandidate(&'a str),
    ToolArgs { name: &'a str, total_bytes: usize },
}

impl<'a> SemanticStreamEvent<'a> {
    pub(crate) const fn public(self) -> StreamEvent<'a> {
        match self {
            Self::ReasoningText(text) | Self::ContentCandidate(text) => StreamEvent::Text(text),
            Self::ToolArgs { name, total_bytes } => StreamEvent::ToolArgs { name, total_bytes },
        }
    }

    pub(crate) fn displayed_chars(self) -> usize {
        match self {
            Self::ReasoningText(text) => text.chars().count(),
            Self::ContentCandidate(_) | Self::ToolArgs { .. } => 0,
        }
    }
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

    pub(crate) async fn chat_semantic_stream<F>(
        &self,
        req: &ChatRequest,
        on_event: F,
    ) -> Result<ChatResponse>
    where
        F: FnMut(SemanticStreamEvent),
    {
        match self {
            DynProvider::Anthropic(p) => p.chat_semantic_stream(req, on_event).await,
            DynProvider::OpenAi(p) => p.chat_semantic_stream(req, on_event).await,
        }
    }
}

pub fn is_retryable(err: &anyhow::Error) -> bool {
    if let Some(error) = err.downcast_ref::<ProtocolError>() {
        return match error.kind() {
            ProtocolErrorKind::EmptyResponse
            | ProtocolErrorKind::InvalidTool
            | ProtocolErrorKind::InvalidJson
            | ProtocolErrorKind::InvalidSequence
            | ProtocolErrorKind::TruncatedStream
            | ProtocolErrorKind::UpstreamError => true,
            ProtocolErrorKind::LimitExceeded => false,
        };
    }
    if let Some(error) = err.downcast_ref::<ProviderRequestError>() {
        return match error.kind() {
            ProviderRequestErrorKind::RateLimited { .. }
            | ProviderRequestErrorKind::Server { .. }
            | ProviderRequestErrorKind::Timeout
            | ProviderRequestErrorKind::Connect
            | ProviderRequestErrorKind::BodyRead
            | ProviderRequestErrorKind::Transport => true,
            ProviderRequestErrorKind::Auth { .. } | ProviderRequestErrorKind::Client { .. } => {
                false
            }
        };
    }
    false
}

pub fn is_rate_limited(err: &anyhow::Error) -> bool {
    if let Some(error) = err.downcast_ref::<ProviderRequestError>() {
        return matches!(error.kind(), ProviderRequestErrorKind::RateLimited { .. });
    }
    if err.downcast_ref::<ProtocolError>().is_some() {
        return false;
    }
    false
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
    header_u64(headers, name).map(saturating_token_count)
}

pub fn rate_limit_error(status: u16, _body: &str, hint: &LimitHint) -> anyhow::Error {
    status_error("provider", status, hint)
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

    #[test]
    fn semantic_text_kinds_preserve_the_public_stream_contract() {
        assert_eq!(
            SemanticStreamEvent::ReasoningText("생각").public(),
            StreamEvent::Text("생각")
        );
        assert_eq!(
            SemanticStreamEvent::ContentCandidate("답").public(),
            StreamEvent::Text("답")
        );
        assert_eq!(
            SemanticStreamEvent::ToolArgs {
                name: "write_file",
                total_bytes: 8192,
            }
            .public(),
            StreamEvent::ToolArgs {
                name: "write_file",
                total_bytes: 8192,
            }
        );
        assert_eq!(
            SemanticStreamEvent::ReasoningText("생각").displayed_chars(),
            2
        );
        assert_eq!(
            SemanticStreamEvent::ContentCandidate("아직 후보").displayed_chars(),
            0
        );
    }

    #[test]
    fn decoder_handles_one_huge_chunk_of_small_frames_without_pending_growth() {
        let chunk = b"data: x\n".repeat(150_000);
        assert!(chunk.len() > MAX_SSE_FRAME_BYTES);
        let mut decoder = SseDecoder::new(SseDelimiter::Line);
        let mut frames = 0usize;
        for &byte in &chunk {
            if decoder.push(byte, "test").unwrap().is_some() {
                frames += 1;
            }
            assert!(decoder.pending_len() <= MAX_SSE_FRAME_BYTES);
        }
        assert_eq!(frames, 150_000);
        assert_eq!(decoder.pending_len(), 0);
    }

    #[test]
    fn stream_budget_caps_many_tools_blocks_and_total_retained_state() {
        let mut budget = StreamBudget::default();
        let piece = "x".repeat(MAX_TOOL_ARGUMENT_BYTES / MAX_CONTENT_BLOCKS);
        let mut arguments = Vec::new();
        for _ in 0..MAX_CONTENT_BLOCKS {
            budget.reserve_blocks(1, "test").unwrap();
            let mut argument = String::new();
            budget
                .append_tool_arguments(&mut argument, &piece, "test")
                .unwrap();
            arguments.push(argument);
        }
        assert_eq!(
            arguments.iter().map(String::len).sum::<usize>(),
            MAX_TOOL_ARGUMENT_BYTES
        );
        assert!(
            budget
                .append_tool_arguments(&mut arguments[0], "x", "test")
                .is_err()
        );
        assert!(budget.reserve_blocks(1, "test").is_err());

        let mut total = StreamBudget::default();
        let mut text = String::new();
        let mut tool = String::new();
        total
            .append_text(
                &mut text,
                &"x".repeat(MAX_RESPONSE_TEXT_BYTES),
                true,
                "test",
            )
            .unwrap();
        total
            .append_tool_arguments(&mut tool, &"x".repeat(MAX_TOOL_ARGUMENT_BYTES), "test")
            .unwrap();
        let mut metadata = String::new();
        assert!(total.append_metadata(&mut metadata, "x", "test").is_err());
    }

    #[test]
    fn rate_limit_error_never_persists_provider_body() {
        let body = "unlabelled upstream credential 7f8db8d8";
        let error = rate_limit_error(
            429,
            body,
            &LimitHint {
                retry_after_secs: Some(7),
                ..LimitHint::default()
            },
        );
        assert_eq!(format!("{error}"), "provider rate limited (retry after 7s)");
        assert!(!format!("{error:?}").contains(body));
    }

    #[test]
    fn typed_request_status_routing_is_exhaustive_and_retry_is_bounded() {
        let cases = [
            (401, ProviderRequestErrorKind::Auth { status: 401 }, false),
            (403, ProviderRequestErrorKind::Auth { status: 403 }, false),
            (
                429,
                ProviderRequestErrorKind::RateLimited {
                    retry_after: MAX_RETRY_AFTER_SECS,
                },
                true,
            ),
            (500, ProviderRequestErrorKind::Server { status: 500 }, true),
            (418, ProviderRequestErrorKind::Client { status: 418 }, false),
        ];
        for (status, expected, retryable) in cases {
            let error = status_error(
                "test",
                status,
                &LimitHint {
                    retry_after_secs: Some(u64::MAX),
                    ..LimitHint::default()
                },
            );
            let typed = error.downcast_ref::<ProviderRequestError>().unwrap();
            assert_eq!(typed.provider(), "test");
            assert_eq!(typed.kind(), expected);
            assert_eq!(is_retryable(&error), retryable);
            assert_eq!(is_rate_limited(&error), status == 429);
            assert_eq!(safe_summary(&error), error.to_string());
        }

        for kind in [
            ProviderRequestErrorKind::Timeout,
            ProviderRequestErrorKind::Connect,
            ProviderRequestErrorKind::BodyRead,
            ProviderRequestErrorKind::Transport,
        ] {
            assert!(is_retryable(
                &ProviderRequestError::new("test", kind).into()
            ));
        }
    }

    #[tokio::test]
    async fn transport_error_discards_secret_url_and_source() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let secret = "private-query-token-7f8db8d8";
        let raw = reqwest::Client::new()
            .get(format!("http://{address}/hidden?token={secret}"))
            .send()
            .await
            .unwrap_err();
        let error = request_error("OpenAI", &raw, RequestErrorPhase::Send);
        assert_eq!(
            error
                .downcast_ref::<ProviderRequestError>()
                .map(ProviderRequestError::kind),
            Some(ProviderRequestErrorKind::Connect)
        );
        for rendered in [
            format!("{error}"),
            format!("{error:#}"),
            format!("{error:?}"),
            safe_summary(&error),
        ] {
            assert!(!rendered.contains(secret));
            assert!(!rendered.contains("hidden"));
            assert!(!rendered.contains(address.to_string().as_str()));
        }
    }

    #[test]
    fn typed_protocol_retry_classification_is_exhaustive() {
        for kind in [
            ProtocolErrorKind::EmptyResponse,
            ProtocolErrorKind::InvalidTool,
            ProtocolErrorKind::InvalidJson,
            ProtocolErrorKind::InvalidSequence,
            ProtocolErrorKind::TruncatedStream,
            ProtocolErrorKind::UpstreamError,
        ] {
            assert!(is_retryable(&protocol_error("test", kind)), "{kind:?}");
        }
        assert!(!is_retryable(&protocol_error(
            "test",
            ProtocolErrorKind::LimitExceeded
        )));
        assert!(!is_rate_limited(&protocol_error(
            "test",
            ProtocolErrorKind::UpstreamError
        )));
    }

    #[test]
    fn untyped_error_text_never_controls_retry_routing() {
        for message in [
            "HTTP 503 server error",
            "HTTP 429 rate_limited",
            "request timeout",
            "too many requests overloaded",
        ] {
            let error = anyhow::anyhow!(message);
            assert!(!is_retryable(&error), "{message}");
            assert!(!is_rate_limited(&error), "{message}");
        }
    }

    fn response_with(content: Vec<ContentBlock>) -> ChatResponse {
        ChatResponse {
            content,
            stop_reason: StopReason::EndTurn,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            model: String::new(),
            cache_reported: false,
            limit: LimitHint::default(),
        }
    }

    #[test]
    fn meaningful_response_rejects_whitespace_and_tool_results() {
        for content in [
            vec![ContentBlock::Text {
                text: " \n\t ".into(),
            }],
            vec![ContentBlock::ToolResult {
                tool_use_id: "call".into(),
                content: "result".into(),
                is_error: false,
            }],
        ] {
            let error = validate_chat_response(&response_with(content), "test").unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<ProtocolError>()
                    .map(ProtocolError::kind),
                Some(ProtocolErrorKind::EmptyResponse)
            );
        }
    }

    #[test]
    fn meaningful_response_rejects_invalid_tool_variants() {
        for block in [
            ContentBlock::ToolUse {
                id: " ".into(),
                name: "tool".into(),
                input: serde_json::json!({}),
            },
            ContentBlock::ToolUse {
                id: "call".into(),
                name: "\t".into(),
                input: serde_json::json!({}),
            },
            ContentBlock::ToolUse {
                id: "call".into(),
                name: "tool".into(),
                input: serde_json::json!([]),
            },
        ] {
            let error = validate_chat_response(&response_with(vec![block]), "test").unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<ProtocolError>()
                    .map(ProtocolError::kind),
                Some(ProtocolErrorKind::InvalidTool)
            );
        }
    }

    #[test]
    fn meaningful_response_accepts_text_or_object_tool() {
        for content in [
            vec![ContentBlock::Text {
                text: " answer ".into(),
            }],
            vec![ContentBlock::ToolUse {
                id: "call".into(),
                name: "tool".into(),
                input: serde_json::json!({"path": "a.txt"}),
            }],
        ] {
            validate_chat_response(&response_with(content), "test").unwrap();
        }
    }
}
