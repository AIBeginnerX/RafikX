pub mod anthropic;
pub mod openai_compat;

pub use anthropic::AnthropicProvider;
pub use openai_compat::OpenAiCompatProvider;

use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct ChatResponse {
    #[allow(dead_code)]
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub input_tokens: u32,
    pub output_tokens: u32,
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
    s.contains("http 429")
        || s.contains("http 500")
        || s.contains("http 502")
        || s.contains("http 503")
        || s.contains("http 504")
        || s.contains("timeout")
        || s.contains("timed out")
}
