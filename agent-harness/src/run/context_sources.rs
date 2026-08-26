use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSourceKind {
    System,
    SessionHistory,
    ProjectRules,
    ProjectMemory,
    Obsidian,
    Plan,
    Lsp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSourceRecord {
    pub kind: ContextSourceKind,
    pub source_id: String,
    pub budget_tokens: u32,
    pub used_tokens: u32,
}
