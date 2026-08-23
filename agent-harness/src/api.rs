//! JSON-friendly façade for the desktop shell. Same config, loop, tools, Obsidian as the CLI.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::accounts_ui;
use crate::agent::LocalAsk;
use crate::auth;
use crate::chat::{self, Session, Slash};
use crate::config::Config;
use crate::db::Db;
use crate::graph;
use crate::obsidian;
use crate::provider::{ContentBlock, Role};

#[derive(Debug, Clone, Serialize)]
pub struct BootInfo {
    pub version: String,
    pub config_path: String,
    pub data_dir: String,
    pub workspace: String,
    pub default_provider: String,
    pub harness: String,
    pub obsidian: ObsidianInfo,
    pub providers: Vec<ProviderInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObsidianInfo {
    pub enabled: bool,
    pub vault_path: String,
    pub vault_exists: bool,
    pub tokenizer: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub connected: bool,
    pub enabled: bool,
    pub is_default: bool,
    pub model: String,
    pub auth_url: Option<String>,
    pub env_hint: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnResult {
    pub run_id: String,
    pub label: String,
    pub status: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub session_id: Option<String>,
    pub graph: Vec<graph::GraphNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TurnRequest {
    pub prompt: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub class: Option<String>,
    pub obsidian: bool,
    pub session_id: Option<String>,
    pub yes: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SlashResult {
    pub notes: String,
    pub quit: bool,
    pub agent_task: Option<String>,
}

pub fn boot() -> Result<BootInfo> {
    crate::ui::init();
    let cfg = Config::load(None)?;
    Ok(boot_with(&cfg))
}

pub fn boot_with(cfg: &Config) -> BootInfo {
    let vault = crate::config::expand_tilde(&cfg.file.obsidian.vault_path);
    let names = auth::menu_provider_names(cfg);
    let providers = names.iter().map(|n| provider_info(cfg, n)).collect();
    BootInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        config_path: cfg.path.display().to_string(),
        data_dir: cfg.data_dir.display().to_string(),
        workspace: cfg.workspace.display().to_string(),
        default_provider: cfg.file.general.default_provider.clone(),
        harness: if cfg.file.harness.selection.eq_ignore_ascii_case("manual") {
            "수동".into()
        } else {
            "자동".into()
        },
        obsidian: ObsidianInfo {
            enabled: cfg.file.obsidian.enabled,
            vault_path: vault.display().to_string(),
            vault_exists: vault.exists(),
            tokenizer: cfg.file.obsidian.tokenizer.clone(),
        },
        providers,
    }
}

fn provider_info(cfg: &Config, name: &str) -> ProviderInfo {
    let p = cfg.provider(name).ok();
    ProviderInfo {
        id: name.to_string(),
        label: auth::provider_label(name),
        connected: auth::is_connected(cfg, name),
        enabled: auth::is_enabled(cfg, name),
        is_default: cfg.file.general.default_provider.eq_ignore_ascii_case(name),
        model: p.map(|x| x.model.clone()).unwrap_or_default(),
        auth_url: accounts_ui::auth_console_url(name).map(|s| s.to_string()),
        env_hint: auth::env_hint(cfg, name),
        kind: p.map(|x| x.kind.clone()).unwrap_or_default(),
    }
}

pub fn list_sessions() -> Result<Vec<SessionInfo>> {
    let db = Db::open(&Db::db_path()?)?;
    Ok(db
        .list_sessions(40)?
        .into_iter()
        .map(|s| SessionInfo {
            id: s.id,
            title: s.title.unwrap_or_else(|| "대화".into()),
            updated_at: s.updated_at,
        })
        .collect())
}

pub fn open_chat(
    provider: Option<String>,
    model: Option<String>,
    class: Option<String>,
    resume: Option<String>,
    yes: bool,
) -> Result<Session> {
    let cfg = Config::load(None)?;
    chat::open_session(cfg, yes, provider, model, class, resume, false)
}

pub fn transcript(session: &Session) -> Vec<ChatMessage> {
    session
        .messages
        .iter()
        .filter_map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };
            let mut text = String::new();
            for b in &m.content {
                match b {
                    ContentBlock::Text { text: t } => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                    ContentBlock::ToolUse { name, .. } => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&format!("[도구] {name}"));
                    }
                    ContentBlock::ToolResult { content, is_error, .. } => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        if *is_error {
                            text.push_str("[도구 오류] ");
                        }
                        text.push_str(content);
                    }
                }
            }
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                Some(ChatMessage {
                    role: role.into(),
                    text: text.to_string(),
                })
            }
        })
        .collect()
}

pub async fn run_turn(
    session: &mut Session,
    prompt: &str,
    obsidian: bool,
    local_ask: Option<LocalAsk>,
) -> Result<TurnResult> {
    session.cfg = Config::load(None)?;
    let class = session.class.clone();
    let info = chat::run_turn(session, prompt, class.as_deref(), obsidian, local_ask).await?;
    let nodes = if info.run_id.is_empty() {
        Vec::new()
    } else {
        graph::for_run(&info.run_id).unwrap_or_default()
    };
    if session.dirty {
        let _ = chat::save_if_dirty(session);
    }
    Ok(TurnResult {
        run_id: info.run_id,
        label: info.label,
        status: info.status,
        tokens_in: info.tokens_in,
        tokens_out: info.tokens_out,
        session_id: session.session_id.clone(),
        graph: nodes,
    })
}

pub fn apply_slash(session: &mut Session, line: &str) -> Result<SlashResult> {
    match chat::handle_slash(session, line, false)? {
        Slash::Continue(notes) => Ok(SlashResult {
            notes: notes.join("\n"),
            quit: false,
            agent_task: None,
        }),
        Slash::Quit => Ok(SlashResult {
            notes: "세션을 닫습니다.".into(),
            quit: true,
            agent_task: None,
        }),
        Slash::Agent(task) => Ok(SlashResult {
            notes: String::new(),
            quit: false,
            agent_task: Some(task),
        }),
    }
}

pub fn save_key(provider: &str, key: &str) -> Result<String> {
    let id = auth::replace_or_save_key(provider, key)?;
    let cfg = Config::load(None)?;
    let _ = accounts_ui::set_default_provider(&cfg, provider);
    Ok(id)
}

pub fn disconnect_provider(name: &str) -> Result<()> {
    auth::disconnect_provider(name)
}

pub fn set_default_provider(name: &str) -> Result<()> {
    let cfg = Config::load(None)?;
    accounts_ui::set_default_provider(&cfg, name)
}

pub fn set_provider_model(name: &str, model: &str) -> Result<()> {
    let cfg = Config::load(None)?;
    accounts_ui::write_provider_model(&cfg, name, model)
}

pub fn add_custom_provider(name: &str, base_url: &str, model: &str) -> Result<()> {
    let cfg = Config::load(None)?;
    accounts_ui::append_custom_openai(&cfg, name, base_url, model)
}

pub fn set_workspace(path: &str) -> Result<()> {
    let cfg = Config::load(None)?;
    crate::config::write_toml_key(
        &cfg.path,
        "[general]",
        "workspace",
        &crate::config::toml_string(path),
    )
}

pub fn set_obsidian_vault(path: &str) -> Result<()> {
    let cfg = Config::load(None)?;
    crate::config::write_toml_key(
        &cfg.path,
        "[obsidian]",
        "vault_path",
        &crate::config::toml_string(path),
    )
}

pub fn set_obsidian_enabled(on: bool) -> Result<()> {
    let cfg = Config::load(None)?;
    crate::config::write_toml_key(
        &cfg.path,
        "[obsidian]",
        "enabled",
        if on { "true" } else { "false" },
    )
}

pub fn index_obsidian() -> Result<String> {
    let cfg = Config::load(None)?;
    let s = obsidian::index_vault(&cfg)?;
    Ok(format!(
        "갱신 {} · 건너뜀 {} · 삭제 {}",
        s.updated, s.skipped, s.deleted
    ))
}

pub fn search_obsidian(query: &str) -> Result<String> {
    let cfg = Config::load(None)?;
    obsidian::search_text(&cfg, query)
}

pub fn graph_latest() -> Result<Option<(String, Vec<graph::GraphNode>)>> {
    graph::latest()
}

pub fn graph_for(run_id: &str) -> Result<Vec<graph::GraphNode>> {
    graph::for_run(run_id)
}

pub fn catalog_models(provider: &str) -> Result<Vec<String>> {
    let cfg = Config::load(None)?;
    Ok(auth::catalog_models(&cfg, provider))
}

pub fn detect_workspace() -> String {
    std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}
