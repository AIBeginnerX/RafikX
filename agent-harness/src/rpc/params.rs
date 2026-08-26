use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct InitializeParams {
    pub protocol_version: Option<String>,
    pub client_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SessionOpenParams {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub class: Option<String>,
    pub resume: Option<String>,
    #[serde(default)]
    pub yes: bool,
}

#[derive(Debug, Deserialize)]
pub struct SessionParams {
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct TurnParams {
    pub session_id: String,
    pub prompt: String,
    #[serde(default)]
    pub obsidian: bool,
    pub class: Option<String>,
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RunParams {
    pub run_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ApprovalParams {
    pub approval_id: String,
    pub decision: String,
}
