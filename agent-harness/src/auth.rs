use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{self, Config, ProviderConfig};

const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_AUTHORIZE: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_TOKEN: &str = "https://console.anthropic.com/v1/oauth/token";
const ANTHROPIC_REDIRECT: &str = "https://platform.claude.com/oauth/code/callback";

const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_AUTHORIZE: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN: &str = "https://auth.openai.com/oauth/token";
const OPENAI_REDIRECT: &str = "http://localhost:1455/auth/callback";
const OPENAI_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
const OPENAI_ORIGINATOR: &str = "codex_cli_rs";
const OPENAI_CALLBACK_PORT: u16 = 1455;

const GEMINI_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GEMINI_CLIENT_SECRET: &str = "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";
const GEMINI_AUTHORIZE: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GEMINI_TOKEN: &str = "https://oauth2.googleapis.com/token";
const GEMINI_REDIRECT: &str = "http://localhost:8085/oauth2callback";
const GEMINI_CALLBACK_PORT: u16 = 8085;

#[derive(Debug, Clone)]
pub struct Credential {
    pub token: String,
    pub oauth: bool,
    pub account_id: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthFile {
    #[serde(flatten)]
    providers: HashMap<String, TokenSet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenSet {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_at: i64,
    #[serde(default)]
    account_id: String,
}

pub fn preferred_models(name: &str) -> &'static [&'static str] {
    match name {
        "anthropic" => &[
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "claude-3-5-sonnet-latest",
            "claude-3-5-haiku-latest",
        ],
        "openai" => &[
            "gpt-5.6",
            "gpt-5.5",
            "gpt-5.1",
            "gpt-5",
            "gpt-5-codex",
            "gpt-4.1",
            "o3",
            "o4-mini",
            "gpt-4o",
        ],
        "gemini" => &[
            "gemini-3.7-flash",
            "gemini-3.1-pro",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.0-flash",
            "gemini-1.5-pro",
        ],
        "grok" => &[
            "grok-4.6",
            "grok-4.5",
            "grok-4",
            "grok-3",
            "grok-3-mini",
            "grok-2",
        ],
        "openrouter" => &[
            "anthropic/claude-sonnet-4.6",
            "openai/gpt-5",
            "google/gemini-2.5-pro",
            "x-ai/grok-4",
        ],
        "groq" => &[
            "llama-3.3-70b-versatile",
            "openai/gpt-oss-120b",
            "qwen/qwen3-32b",
        ],
        "deepseek" => &["deepseek-chat", "deepseek-reasoner"],
        "mistral" => &["mistral-large-latest", "mistral-small-latest"],
        "together" => &["meta-llama/Llama-3.3-70B-Instruct-Turbo"],
        "fireworks" => &["accounts/fireworks/models/llama-v3p3-70b-instruct"],
        "moonshot" => &["kimi-k2.5", "moonshot-v1-auto", "kimi-latest"],
        "glm" => &["glm-4.5", "glm-4-plus", "glm-4-flash"],
        "perplexity" => &["sonar-pro", "sonar"],
        "cohere" => &["command-r-plus", "command-r"],
        "qwen" => &["qwen-max", "qwen-plus", "qwen-turbo"],
        "local" => &["qwen3:8b", "llama3.2", "qwen2.5"],
        "opencode_zen" => &[
            "minimax-m3",
            "glm-5.1",
            "glm-5.2",
            "kimi-k2.7-code",
            "kimi-k2.6",
            "minimax-m2.7",
            "deepseek-v4-flash",
            "deepseek-v4-pro",
            "big-pickle",
            "mimo-v2.5-free",
        ],
        "opencode_go" => &[
            "kimi-k2.7-code",
            "glm-5.1",
            "glm-5.2",
            "kimi-k2.6",
            "deepseek-v4-flash",
            "deepseek-v4-pro",
            "mimo-v2.5",
            "hy3",
        ],
        _ => &[],
    }
}

pub fn pick_preferred(available: &[String], name: &str) -> (Option<String>, Option<String>) {
    let pref = preferred_models(name);
    let mut hits = Vec::new();
    for want in pref {
        if let Some(found) = available.iter().find(|id| model_matches(id, want))
            && !hits.iter().any(|h: &String| h == found)
        {
            hits.push(found.clone());
        }
    }
    if hits.is_empty() {
        return (available.first().cloned(), available.get(1).cloned());
    }
    let main = hits.first().cloned();
    let small = hits.get(1).cloned().or_else(|| main.clone());
    (main, small)
}

fn model_matches(id: &str, want: &str) -> bool {
    let id = id.rsplit('/').next().unwrap_or(id);
    id == want || id.starts_with(want) || want.starts_with(id)
}

pub fn auth_mode(name: &str, p: &ProviderConfig) -> &'static str {
    match p.auth.trim() {
        "oauth" => "oauth",
        "api_key" => "api_key",
        "none" => "none",
        _ => {
            if matches!(name, "anthropic" | "openai" | "gemini" | "grok") {
                "oauth"
            } else if p.api_key_env.is_empty() {
                "none"
            } else {
                "api_key"
            }
        }
    }
}

pub fn resolve_credential(cfg: &Config, name: &str) -> Result<Option<Credential>> {
    seed_legacy_account(cfg, name)?;
    let accs = crate::accounts::for_provider(name);
    if accs.is_empty() {
        return resolve_account_credential(cfg, name, name);
    }
    let id = crate::usage::select_account(&accs).unwrap_or_else(|| accs[0].id.clone());
    if let Some(c) = resolve_account_credential(cfg, name, &id)? {
        return Ok(Some(c));
    }
    // 로테이션이 키 없는 계정을 골랐더라도, 키 있는 계정/프로바이더 키로 이어준다.
    for a in &accs {
        if a.id == id {
            continue;
        }
        if let Some(c) = resolve_account_credential(cfg, name, &a.id)? {
            return Ok(Some(c));
        }
    }
    resolve_account_credential(cfg, name, name)
}

pub fn resolve_account_credential(
    cfg: &Config,
    provider: &str,
    account_id: &str,
) -> Result<Option<Credential>> {
    let p = cfg.provider(provider)?;
    if account_id == provider
        && let Some(v) = env_key_for(provider, &p.api_key_env)
    {
        return Ok(Some(key_cred(v)));
    }
    if let Some(v) = secret_value(account_id)?
        && !v.trim().is_empty()
    {
        return Ok(Some(key_cred(v)));
    }
    if account_id != provider
        && let Some(v) = secret_value(provider)?
        && !v.trim().is_empty()
        && crate::accounts::for_provider(provider).len() == 1
    {
        return Ok(Some(key_cred(v)));
    }
    if auth_mode(provider, p) == "oauth" {
        if let Some(set) = load_valid_set(account_id)? {
            return Ok(Some(oauth_cred(set)));
        }
        if account_id != provider
            && let Some(set) = load_valid_set(provider)?
        {
            return Ok(Some(oauth_cred(set)));
        }
    }
    Ok(None)
}

fn seed_legacy_account(cfg: &Config, name: &str) -> Result<()> {
    if !crate::accounts::for_provider(name).is_empty() {
        return Ok(());
    }
    if resolve_account_credential(cfg, name, name)?.is_some() {
        crate::accounts::ensure_legacy(name)?;
    }
    Ok(())
}

pub fn credential_tail(cfg: &Config, name: &str) -> Result<Option<(String, bool)>> {
    Ok(resolve_credential(cfg, name)?.map(|c| {
        let tail: String = c
            .token
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        (tail, c.oauth)
    }))
}

fn env_only(env_name: &str) -> Option<String> {
    if env_name.is_empty() {
        return None;
    }
    std::env::var(env_name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 설정에 적힌 이름 + 프로바이더별 별칭. 키 원문은 로그에 쓰지 말 것.
pub fn api_key_env_names(provider: &str, primary: &str) -> Vec<String> {
    let mut names = Vec::new();
    if !primary.is_empty() {
        names.push(primary.to_string());
    }
    match provider {
        "opencode_zen" => {
            for n in ["OPENCODE_API_KEY", "OPENCODE_ZEN_API_KEY"] {
                if !names.iter().any(|x| x == n) {
                    names.push(n.to_string());
                }
            }
        }
        "opencode_go" => {
            for n in ["OPENCODE_GO_API_KEY", "OPENCODE_API_KEY"] {
                if !names.iter().any(|x| x == n) {
                    names.push(n.to_string());
                }
            }
        }
        "minimax" => {
            for n in ["MINIMAX_API_KEY", "MINIMAXI_API_KEY"] {
                if !names.iter().any(|x| x == n) {
                    names.push(n.to_string());
                }
            }
        }
        "commandcode" => {
            for n in ["COMMANDCODE_API_KEY", "COMMAND_CODE_API_KEY"] {
                if !names.iter().any(|x| x == n) {
                    names.push(n.to_string());
                }
            }
        }
        _ => {}
    }
    names
}

fn env_key_for(provider: &str, primary: &str) -> Option<String> {
    for name in api_key_env_names(provider, primary) {
        if let Some(v) = env_only(&name) {
            return Some(v);
        }
    }
    None
}

pub fn env_hint(cfg: &Config, name: &str) -> String {
    let primary = cfg
        .provider(name)
        .map(|p| p.api_key_env.as_str())
        .unwrap_or("");
    api_key_env_names(name, primary).join(" / ")
}

fn key_cred(token: String) -> Credential {
    Credential {
        token,
        oauth: false,
        account_id: String::new(),
    }
}

fn oauth_cred(set: TokenSet) -> Credential {
    Credential {
        token: set.access_token,
        oauth: true,
        account_id: set.account_id,
    }
}

pub async fn connect_provider(cfg: &Config, name: &str) -> Result<()> {
    add_account(cfg, name).await
}

async fn connect_frontier(name: &str, account_id: &str) -> Result<()> {
    if name == "grok" {
        return connect_grok_key(account_id).await;
    }
    let import_ready = cli_import_available(name);
    let cli_bin = official_cli_bin(name);
    let mut items = Vec::new();
    let mut actions = Vec::new();
    if import_ready {
        items.push("이미 로그인한 공식 프로그램에서 가져오기".into());
        actions.push("import");
    }
    items.push("브라우저로 로그인".into());
    actions.push("oauth");
    items.push("API 키 붙여넣기".into());
    actions.push("key");
    if cli_bin.is_some() {
        items.push(format!(
            "{} 로 로그인 후 가져오기",
            official_cli_label(name)
        ));
        actions.push("cli");
    }
    println!();
    match name {
        "openai" => {
            println!("ChatGPT 구독 로그인은 Codex CLI와 같은 방식으로 합니다.");
            println!("이미 `codex login` 을 했다면 가져오기가 가장 확실합니다.");
        }
        "anthropic" => {
            println!("Claude 구독 로그인은 Claude Code와 같은 방식입니다.");
            println!("이미 Claude Code에 로그인돼 있으면 가져오기를 고르세요.");
        }
        "gemini" => {
            println!("Gemini 로그인은 Gemini CLI와 같은 방식입니다.");
            println!("이미 `gemini` 에 로그인돼 있으면 가져오기를 고르세요.");
        }
        _ => {}
    }
    let choice = crate::menu::prompt_choice(
        &format!("{} 연결 방법", provider_label(name)),
        &items,
        false,
        "공식 CLI를 프록시로 돌리지 않습니다. 로그인만 맞추고 질문은 RafikX가 직접 보냅니다.",
    )?;
    let n = choice.first().copied().unwrap_or(0);
    if n == 0 {
        return Err(anyhow!("연결을 취소했습니다"));
    }
    let Some(action) = actions.get(n - 1).copied() else {
        return Err(anyhow!("잘못된 번호입니다"));
    };
    match action {
        "import" => import_official_cli(name, account_id, true),
        "oauth" => match name {
            "anthropic" => oauth_anthropic(account_id).await,
            "openai" => oauth_openai(account_id).await,
            "gemini" => oauth_gemini(account_id).await,
            other => Err(anyhow!("'{other}' 는 OAuth 대상이 아닙니다")),
        },
        "key" => {
            let env_hint = match name {
                "anthropic" => "ANTHROPIC_API_KEY",
                "openai" => "OPENAI_API_KEY",
                "gemini" => "GEMINI_API_KEY",
                "opencode_zen" => "OPENCODE_API_KEY",
                "opencode_go" => "OPENCODE_GO_API_KEY",
                _ => "",
            };
            prompt_and_save_key(account_id, env_hint)
        }
        "cli" => {
            run_official_login(name)?;
            import_official_cli(name, account_id, true)
        }
        _ => Err(anyhow!("알 수 없는 연결 방법")),
    }
}

pub async fn add_account(cfg: &Config, name: &str) -> Result<()> {
    for note in maintain_accounts(cfg)? {
        crate::ui::note(&note);
    }
    let p = cfg.provider(name)?;
    let id = crate::accounts::next_id(name);
    let label = crate::accounts::default_label(name);
    let result = match auth_mode(name, p) {
        "oauth" => connect_frontier(name, &id).await,
        "none" => {
            println!("'{name}' 는 키가 필요 없습니다 (로컬).");
            Ok(())
        }
        _ => {
            if crate::accounts::for_provider(name).is_empty()
                && env_key_for(name, &p.api_key_env).is_some()
            {
                println!(
                    "환경변수 {} 로 연결합니다. 붙여넣기는 건너뜁니다.",
                    env_hint(cfg, name)
                );
                Ok(())
            } else if matches!(name, "opencode_zen" | "opencode_go") {
                connect_opencode_key(name, &id)
            } else {
                prompt_and_save_key(&id, &p.api_key_env)
            }
        }
    };
    match result {
        Ok(()) => {
            if let Some(dup) = duplicate_account_for(cfg, name, &id)?
                && dup != id
            {
                let _ = disconnect_account(&id);
                println!(
                    "같은 {} 계정이 이미 '{}' 로 등록되어 있습니다. 중복 등록은 하지 않았습니다.",
                    provider_label(name),
                    dup
                );
                return Ok(());
            }
            crate::accounts::upsert(crate::accounts::Account {
                id: id.clone(),
                provider: name.to_string(),
                label: label.clone(),
            })?;
            let shown = crate::accounts::get(&id).unwrap_or(crate::accounts::Account {
                id: id.clone(),
                provider: name.to_string(),
                label: label.clone(),
            });
            println!("계정 등록: {} ({label})", crate::accounts::display(&shown));
            Ok(())
        }
        Err(e) => {
            let _ = disconnect_account(&id);
            Err(e)
        }
    }
}

/// 로그인 실패·중단으로 credentials 없이 남은 계정, 중복 OAuth, 고아 토큰을 정리한다.
pub fn maintain_accounts(cfg: &Config) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    let orphans: Vec<crate::accounts::Account> = crate::accounts::all()
        .into_iter()
        .filter(|a| {
            cfg.provider(&a.provider)
                .ok()
                .map(|p| auth_mode(&a.provider, p) != "none")
                .unwrap_or(false)
                && !account_has_stored_credential(cfg, &a.provider, &a.id).unwrap_or(false)
        })
        .collect();
    for a in orphans {
        let _ = disconnect_account(&a.id);
        notes.push(format!(
            "로그인 없이 남은 계정 '{}' 를 지웠습니다",
            crate::accounts::display(&a)
        ));
    }
    notes.extend(dedupe_oauth_accounts(cfg)?);
    notes.extend(prune_orphan_auth_tokens()?);
    Ok(notes)
}

pub fn account_has_stored_credential(
    cfg: &Config,
    provider: &str,
    account_id: &str,
) -> Result<bool> {
    let p = cfg.provider(provider)?;
    if let Some(set) = load_token_set(account_id)?
        && !set.access_token.is_empty()
    {
        return Ok(true);
    }
    if let Some(v) = secret_value(account_id)?
        && !v.trim().is_empty()
    {
        return Ok(true);
    }
    if account_id == provider {
        if let Some(v) = env_only(&p.api_key_env)
            && !v.is_empty()
        {
            return Ok(true);
        }
        if let Some(v) = secret_value(provider)?
            && !v.trim().is_empty()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn duplicate_account_for(
    _cfg: &Config,
    provider: &str,
    account_id: &str,
) -> Result<Option<String>> {
    let Some(set) = load_token_set(account_id)? else {
        return Ok(None);
    };
    if set.access_token.is_empty() {
        return Ok(None);
    }
    for a in crate::accounts::for_provider(provider) {
        if a.id == account_id {
            continue;
        }
        let Some(other) = load_token_set(&a.id)? else {
            continue;
        };
        if !other.access_token.is_empty() && other.access_token == set.access_token {
            return Ok(Some(a.id));
        }
    }
    Ok(None)
}

fn load_token_set(name: &str) -> Result<Option<TokenSet>> {
    let file = load_auth()?;
    Ok(file.providers.get(name).cloned())
}

fn dedupe_oauth_accounts(cfg: &Config) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    let mut providers: Vec<String> = crate::accounts::all()
        .into_iter()
        .map(|a| a.provider)
        .collect();
    providers.sort();
    providers.dedup();
    for provider in providers {
        let Ok(p) = cfg.provider(&provider) else {
            continue;
        };
        if auth_mode(&provider, p) != "oauth" {
            continue;
        }
        let mut seen: HashMap<String, String> = HashMap::new();
        let accounts: Vec<_> = crate::accounts::for_provider(&provider);
        for a in accounts {
            let Some(set) = load_token_set(&a.id)? else {
                continue;
            };
            if set.access_token.is_empty() {
                continue;
            }
            if let Some(first) = seen.get(&set.access_token) {
                let _ = disconnect_account(&a.id);
                notes.push(format!("같은 로그인 중복 '{}' → '{}' 만 유지", a.id, first));
            } else {
                seen.insert(set.access_token.clone(), a.id.clone());
            }
        }
    }
    Ok(notes)
}

fn prune_orphan_auth_tokens() -> Result<Vec<String>> {
    let registered: HashSet<String> = crate::accounts::all().into_iter().map(|a| a.id).collect();
    let mut file = load_auth()?;
    let mut notes = Vec::new();
    let keys: Vec<String> = file.providers.keys().cloned().collect();
    for key in keys {
        if registered.contains(&key) {
            continue;
        }
        let provider = provider_of_account(&key).to_string();
        if key == provider && registered.contains(&provider) {
            continue;
        }
        if crate::accounts::for_provider(&provider).is_empty() {
            continue;
        }
        file.providers.remove(&key);
        notes.push(format!("계정 목록에 없는 토큰 '{key}' 를 지웠습니다"));
    }
    if !notes.is_empty() {
        save_auth(&file)?;
    }
    Ok(notes)
}

/// gemini 네이티브 모델 목록 — OpenAI 호환 엔드포인트가 실패할 때의 폴 fallback.
async fn list_gemini_native(
    client: &reqwest::Client,
    cred: &Option<Credential>,
) -> Result<Vec<String>> {
    let mut req = client.get("https://generativelanguage.googleapis.com/v1beta/models");
    if let Some(c) = cred {
        // API 키는 x-goog-api-key, OAuth 토큰은 Bearer — 네이티브 API 가 받는 헤더가 다르다.
        req = if c.oauth {
            req.header("Authorization", format!("Bearer {}", c.token))
        } else {
            req.header("x-goog-api-key", &c.token)
        };
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status}");
    }
    let v = resp.json::<serde_json::Value>().await.unwrap_or_default();
    let mut ids = Vec::new();
    if let Some(arr) = v.get("models").and_then(|m| m.as_array()) {
        for item in arr {
            if let Some(name) = item.get("name").and_then(|x| x.as_str()) {
                ids.push(name.strip_prefix("models/").unwrap_or(name).to_string());
            }
        }
    }
    Ok(ids)
}

/// ChatGPT/Codex OAuth 모델 목록 엔드포인트. client_version 쿼리가 없으면 400 이 온다.
fn codex_models_url() -> String {
    format!(
        "https://chatgpt.com/backend-api/codex/models?client_version={}",
        env!("CARGO_PKG_VERSION")
    )
}

pub async fn list_remote_models(cfg: &Config, name: &str) -> Result<Vec<String>> {
    let p = cfg.provider(name)?;
    let cred = resolve_credential(cfg, name)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()?;
    match p.kind.as_str() {
        "anthropic" => {
            let Some(c) = cred else {
                return Ok(Vec::new());
            };
            // /v1/models 는 기본 20개 페이징 — has_more 동안 끝까지 수집한다.
            let mut ids: Vec<String> = Vec::new();
            let mut next_page: Option<String> = None;
            for _ in 0..5 {
                let url = match &next_page {
                    Some(pg) => format!(
                        "https://api.anthropic.com/v1/models?limit=1000&page={}",
                        url_encode(pg)
                    ),
                    None => "https://api.anthropic.com/v1/models?limit=1000".to_string(),
                };
                let mut req = client.get(&url).header("anthropic-version", "2023-06-01");
                req = apply_anthropic_cred(req, &c);
                let resp = req.send().await?;
                let status = resp.status();
                if !status.is_success() {
                    anyhow::bail!("HTTP {status}");
                }
                let v = resp.json::<serde_json::Value>().await.unwrap_or_default();
                if let Ok(mut part) = parse_id_list(&v) {
                    ids.append(&mut part);
                }
                let has_more = v.get("has_more").and_then(|x| x.as_bool()).unwrap_or(false);
                next_page = v
                    .get("next_page")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
                if !has_more || next_page.is_none() {
                    break;
                }
            }
            ids.sort();
            ids.dedup();
            Ok(ids)
        }
        "openai_compat" => {
            let oauth_openai = name == "openai" && cred.as_ref().is_some_and(|c| c.oauth);
            let url = if oauth_openai {
                codex_models_url()
            } else if let Some(custom) = &p.models_url {
                // /models 가 없는 프로바이더용 직접 지정 엔드포인트 (예: commandcode).
                custom.clone()
            } else {
                let Some(base) = &p.base_url else {
                    return Ok(Vec::new());
                };
                format!("{}/models", base.trim_end_matches('/'))
            };
            let mut req = client.get(url);
            if let Some(c) = &cred {
                req = req.header("Authorization", format!("Bearer {}", c.token));
                if oauth_openai {
                    req = req
                        .header("originator", "codex_cli_rs")
                        .header("User-Agent", "codex_cli_rs");
                    if !c.account_id.is_empty() {
                        req = req.header("chatgpt-account-id", &c.account_id);
                    }
                }
            }
            let resp = req.send().await;
            let Ok(resp) = resp else {
                return Ok(Vec::new());
            };
            let status = resp.status();
            if !status.is_success() {
                // gemini 는 OpenAI 호환 /models 가 없거나 제한적이다 (실측 400) —
                // 네이티브 /v1beta/models 로 폴 fallback 한다. 다만 OAuth 토큰은
                // 네이티브 models API 스코프가 없어 항상 403 (실측) — 정적 카탈로그로 대체한다.
                if name == "gemini" {
                    if cred.as_ref().is_some_and(|c| c.oauth) {
                        return Ok(Vec::new());
                    }
                    return list_gemini_native(&client, &cred).await;
                }
                if oauth_openai && status.as_u16() == 400 {
                    // ChatGPT/Codex OAuth 는 /models 엔드포인트가 없다 — 정적 카탈로그 사용.
                    return Ok(Vec::new());
                }
                if status.as_u16() == 404 {
                    anyhow::bail!(
                        "HTTP {status} — /models 가 없는 서비스는 config 의 models_url 로 목록 위치를 지정하세요"
                    );
                }
                anyhow::bail!("HTTP {status}");
            }
            let v = resp.json::<serde_json::Value>().await.unwrap_or_default();
            let mut ids = parse_id_list(&v)?;
            if ids.is_empty() && oauth_openai {
                // codex 응답은 {"models":[{"slug":…}]} 형태일 수 있다.
                if let Some(arr) = v.get("models").and_then(|d| d.as_array()) {
                    for item in arr {
                        for k in ["slug", "id", "name"] {
                            if let Some(s) = item.get(k).and_then(|x| x.as_str()) {
                                ids.push(s.to_string());
                                break;
                            }
                        }
                    }
                }
            }
            ids.sort();
            ids.dedup();
            Ok(ids)
        }
        _ => Ok(Vec::new()),
    }
}

pub fn apply_anthropic_cred(
    req: reqwest::RequestBuilder,
    cred: &Credential,
) -> reqwest::RequestBuilder {
    if cred.oauth {
        req.header("Authorization", format!("Bearer {}", cred.token))
            .header("anthropic-beta", "oauth-2025-04-20")
    } else {
        req.header("x-api-key", &cred.token)
    }
}

fn parse_id_list(v: &serde_json::Value) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                out.push(id.to_string());
            }
        }
    }
    Ok(out)
}

async fn oauth_anthropic(account_id: &str) -> Result<()> {
    let pkce = generate_pkce();
    let state = random_hex(16);
    // client_id 를 맨 앞에 둔다. Windows start 가 URL을 잘라도 최소한 id는 남는다.
    let url = format!(
        "{ANTHROPIC_AUTHORIZE}?client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        url_encode(ANTHROPIC_CLIENT_ID),
        url_encode(ANTHROPIC_REDIRECT),
        url_encode("org:create_api_key user:profile user:inference"),
        url_encode(&pkce.challenge),
        url_encode(&state),
    );
    println!("브라우저에서 Claude 계정으로 로그인하세요.");
    println!("로그인 후 화면에 보이는 code 값을 이 창에 붙여넣으세요. (code#state 형식도 됩니다)");
    println!("브라우저가 열려도 이 터미널은 그대로 두세요. 닫으면 인증이 끝내지 못합니다.");
    open_browser(&url)?;
    let line = prompt_line("code: ").await?;
    let raw = line.trim();
    if raw.is_empty() {
        return Err(anyhow!("코드가 비어 있습니다"));
    }
    let (code, st) = if let Some((c, s)) = raw.split_once('#') {
        (c.trim(), s.trim().to_string())
    } else {
        (raw, state.clone())
    };
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": st,
        "client_id": ANTHROPIC_CLIENT_ID,
        "redirect_uri": ANTHROPIC_REDIRECT,
        "code_verifier": pkce.verifier,
    });
    let client = http()?;
    let resp = client.post(ANTHROPIC_TOKEN).json(&body).send().await?;
    save_token_response(account_id, resp).await
}

async fn oauth_openai(account_id: &str) -> Result<()> {
    let pkce = generate_pkce();
    let state = random_hex(16);
    let listener = bind_localhost(OPENAI_CALLBACK_PORT, "OpenAI Codex")?;
    let url = openai_authorize_url(&pkce.challenge, &state);
    println!("브라우저에서 ChatGPT 계정으로 로그인하세요.");
    println!(
        "로그인 창이 '필수 매개변수가 없습니다' 라고 하면, 아래 주소 전체를 주소창에 붙이세요."
    );
    open_browser(&url)?;
    let cb = wait_for_callback(listener, Duration::from_secs(180))?;
    if !cb.state.is_empty() && cb.state != state {
        return Err(anyhow!("로그인 state 가 맞지 않습니다. 다시 시도하세요."));
    }
    let form = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        url_encode(&cb.code),
        url_encode(OPENAI_REDIRECT),
        url_encode(OPENAI_CLIENT_ID),
        url_encode(&pkce.verifier),
    );
    let client = http()?;
    let resp = client
        .post(OPENAI_TOKEN)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await?;
    save_token_response(account_id, resp).await
}

fn openai_authorize_url(challenge: &str, state: &str) -> String {
    format!(
        "{OPENAI_AUTHORIZE}?client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&state={}&originator={}",
        url_encode(OPENAI_CLIENT_ID),
        url_encode(OPENAI_REDIRECT),
        url_encode(OPENAI_SCOPE),
        url_encode(challenge),
        url_encode(state),
        url_encode(OPENAI_ORIGINATOR),
    )
}

async fn oauth_gemini(account_id: &str) -> Result<()> {
    let pkce = generate_pkce();
    let state = random_hex(16);
    let listener = bind_localhost(GEMINI_CALLBACK_PORT, "Gemini")?;
    let scope = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile";
    let url = format!(
        "{GEMINI_AUTHORIZE}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&access_type=offline&prompt=consent&code_challenge={}&code_challenge_method=S256",
        url_encode(GEMINI_CLIENT_ID),
        url_encode(GEMINI_REDIRECT),
        url_encode(scope),
        url_encode(&state),
        url_encode(&pkce.challenge),
    );
    println!("브라우저에서 Google 계정으로 로그인하세요.");
    open_browser(&url)?;
    let cb = wait_for_callback(listener, Duration::from_secs(180))?;
    if !cb.state.is_empty() && cb.state != state {
        return Err(anyhow!("로그인 state 가 맞지 않습니다. 다시 시도하세요."));
    }
    let form = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&client_secret={}&code_verifier={}",
        url_encode(&cb.code),
        url_encode(GEMINI_REDIRECT),
        url_encode(GEMINI_CLIENT_ID),
        url_encode(GEMINI_CLIENT_SECRET),
        url_encode(&pkce.verifier),
    );
    let client = http()?;
    let resp = client
        .post(GEMINI_TOKEN)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await?;
    save_token_response(account_id, resp).await
}

fn bind_localhost(port: u16, who: &str) -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port)).with_context(|| {
        format!(
            "{who} 로그인 포트 {port} 가 이미 사용 중입니다. 같은 포트의 공식 CLI를 끄거나, 이미 로그인돼 있으면 '가져오기'를 고르세요."
        )
    })
}

async fn connect_grok_key(account_id: &str) -> Result<()> {
    println!("xAI Grok API는 공개 OAuth가 없어, 콘솔에서 키를 받아 연결합니다.");
    println!(
        "브라우저에서 키를 만든 뒤 붙여넣으세요. 키는 config가 아니라 비밀 파일에만 저장됩니다."
    );
    let _ = open_browser("https://console.x.ai");
    prompt_and_save_key(account_id, "XAI_API_KEY")
}

fn connect_opencode_key(name: &str, account_id: &str) -> Result<()> {
    let (label, env) = match name {
        "opencode_go" => ("OpenCode Go", "OPENCODE_GO_API_KEY"),
        _ => ("OpenCode Zen", "OPENCODE_API_KEY"),
    };
    println!("{label} 키는 https://opencode.ai/auth 에서 만듭니다.");
    println!("Zen은 종량제, Go는 구독입니다. 키는 Bearer 로 붙입니다.");
    println!("환경변수 {env} (Go는 OPENCODE_API_KEY 도 가능). 키는 secrets.toml 에만 저장됩니다.");
    let _ = open_browser("https://opencode.ai/auth");
    prompt_and_save_key(account_id, env)
}

/// TUI `/connect` 용. 출력 없음.
pub fn save_pasted_key(name: &str, key: &str) -> Result<String> {
    replace_or_save_key(name, key)
}

/// 이미 계정이 있으면 그 비밀만 갈아끼운다. 없으면 새로 등록.
pub fn replace_or_save_key(name: &str, key: &str) -> Result<String> {
    let key = crate::accounts_ui::sanitize_pasted_key(key);
    if key.is_empty() {
        return Err(anyhow!("키가 비어 있습니다"));
    }
    let accs = crate::accounts::for_provider(name);
    if let Some(first) = accs.first() {
        save_secret(&first.id, &key)?;
        return Ok(first.id.clone());
    }
    let id = crate::accounts::next_id(name);
    let label = crate::accounts::default_label(name);
    save_secret(&id, &key)?;
    crate::accounts::upsert(crate::accounts::Account {
        id: id.clone(),
        provider: name.to_string(),
        label,
    })?;
    Ok(id)
}

pub fn resolve_provider_alias(s: &str) -> Option<String> {
    let t = s.trim().to_lowercase().replace(['-', ' '], "_");
    let mapped = match t.as_str() {
        "zen" | "opencode" | "opencode_zen" | "opencodezen" => "opencode_zen",
        "opencode_go" | "opencodego" | "ocgo" => "opencode_go",
        "claude" | "anthropic" => "anthropic",
        "openai" | "codex" | "chatgpt" => "openai",
        "gemini" | "google" => "gemini",
        "grok" | "xai" => "grok",
        "local" | "ollama" => "local",
        other => other,
    };
    Some(mapped.to_string())
}

fn prompt_and_save_key(name: &str, env_hint: &str) -> Result<()> {
    let provider = provider_of_account(name);
    let label = provider_label(provider);
    let url = crate::accounts_ui::auth_console_url(provider);
    let Some(key) = crate::menu::prompt_api_key_box(&label, url, env_hint)? else {
        return Err(anyhow!("키가 비어 있습니다"));
    };
    save_secret(name, &key)?;
    println!("저장했습니다. (비밀 파일, 마지막 4자 ****{})", tail4(&key));
    Ok(())
}

async fn save_token_response(name: &str, resp: reqwest::Response) -> Result<()> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let snippet: String = text.chars().take(240).collect();
        return Err(anyhow!(
            "{name} 토큰 교환 실패 HTTP {}: {snippet}",
            status.as_u16()
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).context("토큰 응답이 JSON이 아닙니다")?;
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("access_token 이 없습니다"))?;
    let refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let expires_in = v.get("expires_in").and_then(|x| x.as_i64()).unwrap_or(3600);
    let id_token = v.get("id_token").and_then(|x| x.as_str());
    let account = chatgpt_account_id(
        access,
        id_token,
        v.get("account_id").and_then(|x| x.as_str()),
    );
    let now = now_secs();
    save_tokens(
        name,
        TokenSet {
            access_token: access.to_string(),
            refresh_token: refresh,
            expires_at: now + expires_in - 60,
            account_id: account,
        },
    )?;
    println!("{name} OAuth 연결 완료 (****{})", tail4(access));
    Ok(())
}

struct Callback {
    code: String,
    state: String,
}

fn wait_for_callback(listener: TcpListener, timeout: Duration) -> Result<Callback> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = (|| -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf)?;
            let req = String::from_utf8_lossy(&buf[..n]);
            let code = parse_query_param(&req, "code").unwrap_or_default();
            let state = parse_query_param(&req, "state").unwrap_or_default();
            let body = "<html><body><p>로그인 완료. 이 창을 닫아도 됩니다.</p></body></html>";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = tx.send(Callback { code, state });
            Ok(())
        })();
    });
    rx.recv_timeout(timeout)
        .map_err(|_| anyhow!("로그인 대기 시간이 지났습니다"))
        .and_then(|c| {
            if c.code.is_empty() {
                Err(anyhow!("브라우저에서 code 를 받지 못했습니다"))
            } else {
                Ok(c)
            }
        })
}

fn parse_query_param(req: &str, key: &str) -> Option<String> {
    let line = req.lines().next()?;
    let q = line.split('?').nth(1)?;
    let q = q.split(' ').next()?;
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == key
        {
            return Some(url_decode(v));
        }
    }
    None
}

fn load_valid_set(name: &str) -> Result<Option<TokenSet>> {
    let mut file = load_auth()?;
    let Some(set) = file.providers.get(name).cloned() else {
        return Ok(None);
    };
    if set.access_token.is_empty() {
        return Ok(None);
    }
    if set.expires_at > 0
        && now_secs() > set.expires_at
        && let Ok(Some(fresh)) = refresh_token_sync(provider_of_account(name), &set)
    {
        file.providers.insert(name.to_string(), fresh.clone());
        save_auth(&file)?;
        return Ok(Some(fresh));
    }
    Ok(Some(set))
}

pub fn provider_of_account(id: &str) -> &str {
    id.split("::").next().unwrap_or(id)
}

fn refresh_token_sync(name: &str, set: &TokenSet) -> Result<Option<TokenSet>> {
    if set.refresh_token.is_empty() {
        return Ok(None);
    }
    let rt = tokio::runtime::Handle::try_current();
    match rt {
        Ok(h) => tokio::task::block_in_place(|| h.block_on(refresh_token(name, set))),
        Err(_) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(refresh_token(name, set))
        }
    }
}

async fn refresh_token(name: &str, set: &TokenSet) -> Result<Option<TokenSet>> {
    let client = http()?;
    let resp = match name {
        "anthropic" => {
            client
                .post(ANTHROPIC_TOKEN)
                .json(&serde_json::json!({
                    "grant_type": "refresh_token",
                    "refresh_token": set.refresh_token,
                    "client_id": ANTHROPIC_CLIENT_ID,
                }))
                .send()
                .await?
        }
        "openai" => {
            client
                .post(OPENAI_TOKEN)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(format!(
                    "grant_type=refresh_token&refresh_token={}&client_id={}",
                    url_encode(&set.refresh_token),
                    url_encode(OPENAI_CLIENT_ID)
                ))
                .send()
                .await?
        }
        "gemini" => {
            client
                .post(GEMINI_TOKEN)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(format!(
                    "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
                    url_encode(&set.refresh_token),
                    url_encode(GEMINI_CLIENT_ID),
                    url_encode(GEMINI_CLIENT_SECRET)
                ))
                .send()
                .await?
        }
        _ => return Ok(None),
    };
    if !resp.status().is_success() {
        return Ok(None);
    }
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("refresh 에 access_token 없음"))?;
    let refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or(&set.refresh_token)
        .to_string();
    let expires_in = v.get("expires_in").and_then(|x| x.as_i64()).unwrap_or(3600);
    let id_token = v.get("id_token").and_then(|x| x.as_str());
    let mut account = chatgpt_account_id(
        access,
        id_token,
        v.get("account_id").and_then(|x| x.as_str()),
    );
    if account.is_empty() {
        account = set.account_id.clone();
    }
    Ok(Some(TokenSet {
        access_token: access.to_string(),
        refresh_token: refresh,
        expires_at: now_secs() + expires_in - 60,
        account_id: account,
    }))
}

fn auth_path() -> Result<PathBuf> {
    Ok(config::Config::data_dir()?.join("auth.json"))
}

fn secrets_path() -> Result<PathBuf> {
    Ok(config::Config::data_dir()?.join("secrets.toml"))
}

fn load_auth() -> Result<AuthFile> {
    let path = auth_path()?;
    if !path.exists() {
        return Ok(AuthFile::default());
    }
    let raw = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn save_auth(file: &AuthFile) -> Result<()> {
    let path = auth_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(file)?)?;
    config::set_owner_only_mode(&path);
    Ok(())
}

fn save_tokens(name: &str, set: TokenSet) -> Result<()> {
    let mut file = load_auth()?;
    file.providers.insert(name.to_string(), set);
    save_auth(&file)
}

fn load_secrets() -> Result<HashMap<String, String>> {
    let path = secrets_path()?;
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = fs::read_to_string(&path)?;
    let v: toml::Value = toml::from_str(&raw).unwrap_or(toml::Value::Table(Default::default()));
    let mut out = HashMap::new();
    if let Some(table) = v.as_table() {
        for (k, val) in table {
            if let Some(s) = val.as_str()
                && !s.trim().is_empty()
            {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    Ok(out)
}

fn save_secret(name: &str, key: &str) -> Result<()> {
    let path = secrets_path()?;
    let mut map = load_secrets()?;
    map.insert(name.to_string(), key.to_string());
    let mut buf = String::from("# RafikX 비밀. git/채팅에 올리지 마세요.\n");
    let mut keys: Vec<_> = map.keys().cloned().collect();
    keys.sort();
    for k in keys {
        if let Some(v) = map.get(&k) {
            buf.push_str(&format!("{k} = \"{}\"\n", toml_escape(v)));
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, buf)?;
    config::set_owner_only_mode(&path);
    Ok(())
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?)
}

async fn prompt_line(label: &str) -> Result<String> {
    let label = label.to_string();
    tokio::task::spawn_blocking(move || {
        print!("{label}");
        io::stdout().flush()?;
        let mut line = String::new();
        let n = io::stdin().read_line(&mut line)?;
        if n == 0 {
            return Err(anyhow!(
                "터미널 입력이 끊겼습니다. 창이 닫히면 code 를 붙여넣을 수 없습니다."
            ));
        }
        Ok(line)
    })
    .await
    .map_err(|e| anyhow!("입력 대기 실패: {e}"))?
}

fn write_internet_shortcut(url: &str) -> Result<PathBuf> {
    let path = config::Config::data_dir()?.join("oauth-login.url");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Internet Shortcut 은 cmd 가 `&` 를 자르지 않는다. Windows 로그인 주소를 이렇게 연다.
    let body = format!("[InternetShortcut]\r\nURL={url}\r\n");
    fs::write(&path, body)?;
    Ok(path)
}

fn open_browser(url: &str) -> Result<()> {
    println!("{url}");
    let shortcut = write_internet_shortcut(url).ok();
    if let Some(path) = &shortcut {
        println!("로그인 파일: {}", path.display());
        println!("브라우저가 잘못 열리면 위 파일을 더블클릭하거나, 주소 전체를 주소창에 붙이세요.");
    }
    println!("이 터미널 창은 닫지 마세요. 로그인 후 여기로 돌아와야 합니다.");

    #[cfg(windows)]
    {
        // cmd /c start 는 같은 콘솔 그룹에 CTRL_CLOSE 를 보내 창을 닫을 수 있다.
        // ShellExecute / explorer 만 써서 터미널은 그대로 둔다.
        if let Some(path) = &shortcut {
            if win_open::open(&path.to_string_lossy()) {
                return Ok(());
            }
            if spawn_detached("explorer", &[path.as_os_str()]) {
                return Ok(());
            }
        }
        if win_open::open(url) {
            return Ok(());
        }
    }
    #[cfg(not(windows))]
    {
        for bin in ["xdg-open", "open"] {
            if Command::new(bin)
                .arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .is_ok()
            {
                return Ok(());
            }
        }
    }
    println!("브라우저가 열리지 않으면 위 주소를 직접 여세요.");
    Ok(())
}

#[cfg(windows)]
fn spawn_detached(bin: &str, args: &[&std::ffi::OsStr]) -> bool {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP — 콘솔 CTRL_CLOSE 를 부모와 나누지 않음
        cmd.creation_flags(0x00000200);
    }
    cmd.spawn().is_ok()
}

#[cfg(windows)]
mod win_open {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut std::ffi::c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> isize;
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn open(path_or_url: &str) -> bool {
        let op = wide("open");
        let file = wide(path_or_url);
        const SW_SHOWNORMAL: i32 = 1;
        let n = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        n > 32
    }
}

struct Pkce {
    verifier: String,
    challenge: String,
}

fn generate_pkce() -> Pkce {
    let raw = random_bytes(32);
    let verifier = b64url(&raw);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = b64url(&digest);
    Pkce {
        verifier,
        challenge,
    }
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(out.len() as u64);
        h.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        out.extend_from_slice(&h.finish().to_le_bytes());
    }
    out.truncate(n);
    out
}

pub(crate) fn random_hex(n: usize) -> String {
    random_bytes(n).iter().map(|b| format!("{b:02x}")).collect()
}

fn b64url(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i];
        let b1 = if i + 1 < data.len() { data[i + 1] } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] } else { 0 };
        out.push(T[(b0 >> 2) as usize] as char);
        out.push(T[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        if i + 1 < data.len() {
            out.push(T[(((b1 & 15) << 2) | (b2 >> 6)) as usize] as char);
        }
        if i + 2 < data.len() {
            out.push(T[(b2 & 63) as usize] as char);
        }
        i += 3;
    }
    out
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn tail4(s: &str) -> String {
    s.chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

pub fn is_enabled(cfg: &Config, name: &str) -> bool {
    cfg.provider(name).map(|p| p.enabled).unwrap_or(true)
}

pub fn is_usable(cfg: &Config, name: &str) -> bool {
    is_enabled(cfg, name) && is_connected(cfg, name)
}

pub fn set_provider_enabled(cfg: &Config, name: &str, enabled: bool) -> Result<()> {
    cfg.provider(name)?;
    config::write_toml_key(
        &cfg.path,
        &format!("[providers.{name}]"),
        "enabled",
        if enabled { "true" } else { "false" },
    )?;
    if enabled {
        println!("'{name}' 사용을 다시 켰습니다.");
    } else {
        println!("'{name}' 사용을 중지했습니다. 로그인은 남아 있고, 질문은 보내지 않습니다.");
    }
    Ok(())
}

pub fn is_connected(cfg: &Config, name: &str) -> bool {
    let Ok(p) = cfg.provider(name) else {
        return false;
    };
    if auth_mode(name, p) == "none" {
        return true;
    }
    // 여러 계정 중 하나라도 유효한 자격증명이 있으면 "연결됨".
    // (키 없는 하위 계정이 로테이션에 선택됐다는 이유로 상태가 뒤집히면 안 된다.)
    let mut ids: Vec<String> = crate::accounts::for_provider(name)
        .into_iter()
        .map(|a| a.id)
        .collect();
    ids.push(name.to_string());
    ids.iter()
        .any(|id| matches!(resolve_account_credential(cfg, name, id), Ok(Some(_))))
}

pub fn has_cloud_credential(cfg: &Config) -> bool {
    cfg.file.providers.keys().any(|name| {
        let Ok(p) = cfg.provider(name) else {
            return false;
        };
        if auth_mode(name, p) == "none" {
            return false;
        }
        resolve_credential(cfg, name).ok().flatten().is_some()
    })
}

pub fn connected_names(cfg: &Config) -> Vec<String> {
    let mut names: Vec<String> = cfg
        .file
        .providers
        .keys()
        .filter(|n| is_connected(cfg, n))
        .cloned()
        .collect();
    names.sort_by_key(|n| provider_sort_key(n));
    names
}

pub fn usable_names(cfg: &Config) -> Vec<String> {
    connected_names(cfg)
        .into_iter()
        .filter(|n| is_enabled(cfg, n))
        .collect()
}

pub fn provider_sort_key(name: &str) -> (u8, String) {
    let rank = match name {
        "opencode_zen" => 0,
        "opencode_go" => 1,
        "anthropic" => 2,
        "openai" => 3,
        "gemini" => 4,
        "grok" => 5,
        "openrouter" => 6,
        "local" => 90,
        _ => 50,
    };
    (rank, name.to_string())
}

pub fn provider_label(name: &str) -> String {
    match name {
        "opencode_zen" => "OpenCode Zen  (키)".into(),
        "opencode_go" => "OpenCode Go  (키)".into(),
        "anthropic" => "Anthropic  (로그인)".into(),
        "openai" => "OpenAI Codex  (로그인)".into(),
        "gemini" => "Gemini  (로그인)".into(),
        "grok" => "Grok  (키 붙여넣기)".into(),
        "openrouter" => "OpenRouter".into(),
        "groq" => "Groq".into(),
        "deepseek" => "DeepSeek".into(),
        "mistral" => "Mistral".into(),
        "together" => "Together".into(),
        "fireworks" => "Fireworks".into(),
        "moonshot" => "Moonshot / Kimi".into(),
        "glm" => "GLM (Z.ai)".into(),
        "perplexity" => "Perplexity".into(),
        "cohere" => "Cohere".into(),
        "qwen" => "Qwen (DashScope)".into(),
        "minimax" => "MiniMax  (키)".into(),
        "commandcode" => "CommandCode  (키)".into(),
        "local" => "로컬 (Ollama, 키 없음)".into(),
        other => other.to_string(),
    }
}

pub fn menu_provider_names(cfg: &Config) -> Vec<String> {
    let mut names: Vec<String> = cfg.file.providers.keys().cloned().collect();
    names.sort_by_key(|n| provider_sort_key(n));
    names
}

#[derive(Debug, Clone)]
pub struct RegisteredModel {
    pub provider: String,
    pub id: String,
    pub small: bool,
}

pub fn catalog_models(cfg: &Config, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(p) = cfg.provider(name) {
        if !p.model.is_empty() {
            out.push(p.model.clone());
        }
        if let Some(s) = &p.small_model
            && !s.is_empty()
            && !out.iter().any(|x| x == s)
        {
            out.push(s.clone());
        }
    }
    for m in preferred_models(name) {
        if !out.iter().any(|x| x == m) {
            out.push((*m).to_string());
        }
    }
    // 원격에서 한 번이라도 불러온 모델 목록도 합친다 (연결 직후 자동 조회 결과).
    for m in cached_catalog(cfg, name) {
        if !out.iter().any(|x| x == &m) {
            out.push(m);
        }
    }
    out
}

fn catalogs_file(cfg: &Config) -> std::path::PathBuf {
    cfg.data_dir.join("catalogs.json")
}

fn catalogs_meta_file(cfg: &Config) -> std::path::PathBuf {
    cfg.data_dir.join("catalogs_meta.json")
}

/// 카탈로그 만료 기준 — 하루. 신규 모델 반영은 하루에 한 번, 백그라운드로 충분하다.
const CATALOG_STALE_SECS: i64 = 24 * 60 * 60;

/// 카탈로그를 마지막으로 건든 시각을 기록한다 (성공·실패 모두 — 실패가 프로세스마다
/// 재시도되는 네트워크 폭탄이 되지 않도록 하루에 한 번만 묶는다).
fn stamp_catalog(cfg: &Config, name: &str) -> Result<()> {
    #[derive(serde::Serialize, serde::Deserialize, Default)]
    struct Meta(std::collections::BTreeMap<String, i64>);
    let path = catalogs_meta_file(cfg);
    let mut m: Meta = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    m.0.insert(name.to_string(), now_secs());
    std::fs::create_dir_all(cfg.data_dir.clone())?;
    std::fs::write(&path, serde_json::to_string(&m)?)?;
    Ok(())
}

fn catalog_touched_at(cfg: &Config, name: &str) -> Option<i64> {
    #[derive(serde::Serialize, serde::Deserialize, Default)]
    struct Meta(std::collections::BTreeMap<String, i64>);
    std::fs::read_to_string(catalogs_meta_file(cfg))
        .ok()
        .and_then(|raw| serde_json::from_str::<Meta>(&raw).ok())
        .and_then(|mut m| m.0.remove(name))
}

/// 연결 중인 프로바이더 중 카탈로그를 하루 이상 안 건드린 곳이 하나라도 있는지.
/// 스탬프가 아예 없으면(등록 직후, 또는 옛 버전에서 받아둔 캐시) 갱신 대상으로 본다.
pub fn catalogs_stale(cfg: &Config) -> bool {
    usable_names(cfg).iter().any(|n| {
        catalog_touched_at(cfg, n)
            .map(|ts| now_secs() - ts >= CATALOG_STALE_SECS)
            .unwrap_or(true)
    })
}

/// 하루 1회 모델 카탈로그 자동 갱신 — ranks 주간 갱신과 같은 방식. 부트를 절대 막지 않는다.
/// 프로세스당 1회만 기회를 얻고, 데스크탑·텔레그램처럼 오래 사는 프로세스는 6시간마다
/// 만료분을 다시 돈다. 순수 백그라운드라 실패해도 본 작업에 소리도 없다.
pub fn spawn_catalog_refresh(cfg: &Config) {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let cfg = cfg.clone();
    let run = async move {
        // interval 의 첫 tick 은 즉시 발동한다 — 시작 시 1회 + 이후 6시간마다 검사.
        let mut ticker = tokio::time::interval(Duration::from_secs(6 * 60 * 60));
        loop {
            if catalogs_stale(&cfg) {
                let rows = refresh_catalogs(&cfg).await;
                let summary = refresh_summary(&rows).join(" | ");
                crate::applog::info(&format!("모델 카탈로그 자동 갱신: {summary}"));
            }
            ticker.tick().await;
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(run);
        }
        Err(_) => {
            let _ = thread::Builder::new()
                .name("rafikx-catalog-refresh".into())
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build();
                    if let Ok(rt) = rt {
                        rt.block_on(run);
                    }
                });
        }
    }
}

/// 원격 모델 목록을 캐시에 저장한다 (/model · Harness 선택이 이 목록을 쓴다).
/// 마지막 건든 시각도 함께 기록해 자동 갱신의 만료 판단에 쓴다.
pub fn save_catalog(cfg: &Config, name: &str, list: &[String]) -> Result<()> {
    #[derive(serde::Serialize, serde::Deserialize, Default)]
    struct Catalogs(std::collections::BTreeMap<String, Vec<String>>);
    let path = catalogs_file(cfg);
    let mut c: Catalogs = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    c.0.insert(name.to_string(), list.to_vec());
    std::fs::create_dir_all(cfg.data_dir.clone())?;
    std::fs::write(&path, serde_json::to_string(&c)?)?;
    stamp_catalog(cfg, name)
}

fn cached_catalog(cfg: &Config, name: &str) -> Vec<String> {
    #[derive(serde::Serialize, serde::Deserialize, Default)]
    struct Catalogs(std::collections::BTreeMap<String, Vec<String>>);
    std::fs::read_to_string(catalogs_file(cfg))
        .ok()
        .and_then(|raw| serde_json::from_str::<Catalogs>(&raw).ok())
        .and_then(|mut c| c.0.remove(name))
        .unwrap_or_default()
}

/// 원격 모델 목록을 조회해 캐시에 저장한다 — 등록 직후 자동 검색(로그인·models 명령)의
/// 한 통로. 조회 실패·빈 목록이면 아무것도 건드리지 않고 None.
pub async fn fetch_and_save_catalog(cfg: &Config, name: &str) -> Option<usize> {
    let list = match list_remote_models(cfg, name).await {
        Ok(list) if !list.is_empty() => list,
        _ => return None,
    };
    let n = list.len();
    save_catalog(cfg, name, &list).ok()?;
    Some(n)
}

/// 연결 하나의 카탈로그 갱신 결과 — 저장한 모델 수, 또는 실패 사유 요약.
#[derive(Debug, Clone)]
pub struct CatalogRefresh {
    pub provider: String,
    pub result: Result<usize, String>,
}

/// 오류 메시지는 한 줄 요약으로만 남긴다 (전체 체인은 화면을 덮는다).
fn short_error(e: &anyhow::Error) -> String {
    let msg = format!("{e:#}");
    let one = msg.split('\n').next().unwrap_or("").trim().to_string();
    if one.chars().count() > 120 {
        one.chars().take(120).collect()
    } else {
        one
    }
}

/// 연결된 모든 서비스의 모델 목록을 동시에 다시 조회해 캐시에 저장한다.
/// 빈 목록은 저장하지 않는다 — 일시적 실패로 기존 캐시를 지워 버리면 퇴보다.
/// 저장은 조회가 다 끈 뒤 순서대로 — catalogs.json 이 파일 통째 read-modify-write 라
/// 동시에 쓰면 나중 쓰기가 먼저 쓰기를 덮어 항목이 사라진다.
pub async fn refresh_catalogs(cfg: &Config) -> Vec<CatalogRefresh> {
    let names = usable_names(cfg);
    let fetched = futures_util::future::join_all(names.iter().map(|name| async move {
        // auto_assign_roles 와 같은 12초 상한 — 한 연결이 늦어도 전체가 잠기지 않는다.
        let fetched = tokio::time::timeout(
            Duration::from_secs(12),
            list_remote_models(cfg, name.as_str()),
        )
        .await;
        (name.clone(), fetched)
    }))
    .await;
    let mut rows = Vec::new();
    for (name, fetched) in fetched {
        let result = match fetched {
            Err(_) => Err("시간 초과 (12초)".to_string()),
            Ok(Err(e)) => Err(short_error(&e)),
            Ok(Ok(list)) if list.is_empty() => Ok(0),
            Ok(Ok(list)) => match save_catalog(cfg, &name, &list) {
                Ok(()) => Ok(list.len()),
                Err(e) => Err(short_error(&e)),
            },
        };
        // 어느 결과든 시각을 남긴다 — 실패·빈 목록도 하루에 한 번만 다시 시도하도록.
        // (save_catalog 가 이미 찍은 경우 같은 초에 한 번 더 찍히지만 무해하다.)
        let _ = stamp_catalog(cfg, &name);
        rows.push(CatalogRefresh {
            provider: name,
            result,
        });
    }
    rows
}

/// 갱신 결과를 사람이 읽는 줄로 조립한다 (프로바이더별 한 줄 + 총계 한 줄).
pub fn refresh_summary(rows: &[CatalogRefresh]) -> Vec<String> {
    if rows.is_empty() {
        return vec!["연결된 서비스가 없습니다. /connect 로 먼저 연결하세요.".into()];
    }
    let mut out = Vec::new();
    let mut total = 0usize;
    let mut failed = 0usize;
    for r in rows {
        match &r.result {
            Ok(n) => {
                total += n;
                out.push(format!("{}  {}개", r.provider, n));
            }
            Err(e) => {
                failed += 1;
                out.push(format!("{}  실패: {}", r.provider, e));
            }
        }
    }
    let mut tail = format!("총 {}곳 · 모델 {}개", rows.len(), total);
    if failed > 0 {
        tail.push_str(&format!(" · 실패 {failed}곳"));
    }
    out.push(tail);
    out
}

pub fn registered_models(cfg: &Config) -> Vec<RegisteredModel> {
    let mut out = Vec::new();
    for name in usable_names(cfg) {
        let Ok(p) = cfg.provider(&name) else { continue };
        let small = p.small_model.clone().unwrap_or_default();
        for id in catalog_models(cfg, &name) {
            let is_small = !small.is_empty() && (id == small || crate::ranks::is_cheap_id(&id));
            if !out
                .iter()
                .any(|r: &RegisteredModel| r.provider == name && r.id == id)
            {
                out.push(RegisteredModel {
                    provider: name.clone(),
                    id,
                    small: is_small,
                });
            }
        }
    }
    // 콤보 (F8) — [combos] 를 "combo" 가상 프로바이더 항목으로 노출한다.
    for name in cfg.file.combos.keys() {
        out.push(RegisteredModel {
            provider: "combo".into(),
            id: name.clone(),
            small: false,
        });
    }
    out

}

pub fn disconnect_provider(name: &str) -> Result<()> {
    let gone = crate::accounts::remove_provider(name)?;
    delete_secret(name)?;
    let mut file = load_auth()?;
    file.providers.remove(name);
    for a in &gone {
        delete_secret(&a.id)?;
        file.providers.remove(&a.id);
    }
    save_auth(&file)?;
    println!("'{name}' 연결을 모두 해제했습니다.");
    Ok(())
}

pub fn disconnect_account(id: &str) -> Result<()> {
    delete_secret(id)?;
    let mut file = load_auth()?;
    file.providers.remove(id);
    save_auth(&file)?;
    let _ = crate::accounts::remove(id);
    println!("계정 '{id}' 를 해제했습니다.");
    Ok(())
}

pub fn account_connected(cfg: &Config, provider: &str, account_id: &str) -> bool {
    resolve_account_credential(cfg, provider, account_id)
        .ok()
        .flatten()
        .is_some()
}

pub fn list_account_rows(
    cfg: &Config,
    provider: &str,
) -> Vec<(crate::accounts::Account, bool, String)> {
    let _ = seed_legacy_account(cfg, provider);
    crate::accounts::for_provider(provider)
        .into_iter()
        .map(|a| {
            let live = account_connected(cfg, provider, &a.id);
            let tail = resolve_account_credential(cfg, provider, &a.id)
                .ok()
                .flatten()
                .map(|c| tail4(&c.token))
                .unwrap_or_default();
            (a, live, tail)
        })
        .collect()
}

pub fn store_secret(name: &str, key: &str) -> Result<()> {
    save_secret(name, key)
}

pub fn secret_value(name: &str) -> Result<Option<String>> {
    Ok(load_secrets()?.get(name).cloned())
}

pub fn telegram_token(cfg: &Config) -> Result<Option<String>> {
    let env_name = cfg.file.telegram.token_env.trim();
    if !env_name.is_empty()
        && let Ok(v) = std::env::var(env_name)
        && !v.trim().is_empty()
    {
        return Ok(Some(v));
    }
    secret_value("telegram")
}

fn delete_secret(name: &str) -> Result<()> {
    let path = secrets_path()?;
    if !path.exists() {
        return Ok(());
    }
    let mut map = load_secrets()?;
    map.remove(name);
    let mut buf = String::from("# RafikX 비밀. git/채팅에 올리지 마세요.\n");
    let mut keys: Vec<_> = map.keys().cloned().collect();
    keys.sort();
    for k in keys {
        if let Some(v) = map.get(&k) {
            buf.push_str(&format!("{k} = \"{}\"\n", toml_escape(v)));
        }
    }
    fs::write(&path, buf)?;
    config::set_owner_only_mode(&path);
    Ok(())
}

pub fn ensure_secrets_template() {
    let Ok(path) = secrets_path() else { return };
    if path.exists() {
        return;
    }
    let _ = fs::write(
        &path,
        "# RafikX 비밀. git/채팅에 올리지 마세요.\n\
         # 예: openrouter = \"sk-or-...\"\n",
    );
    config::set_owner_only_mode(&path);
}

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

fn cli_import_available(name: &str) -> bool {
    cli_credential_paths(name).iter().any(|p| p.exists())
}

fn official_cli_label(name: &str) -> &'static str {
    match name {
        "openai" => "Codex CLI",
        "gemini" => "Gemini CLI",
        "anthropic" => "Claude Code",
        _ => "공식 CLI",
    }
}

fn official_cli_bin(name: &str) -> Option<String> {
    let bin = match name {
        "openai" => "codex",
        "gemini" => "gemini",
        _ => return None,
    };
    if command_exists(bin) {
        Some(bin.to_string())
    } else {
        None
    }
}

fn command_exists(bin: &str) -> bool {
    #[cfg(windows)]
    {
        Command::new("where")
            .arg(bin)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        Command::new("which")
            .arg(bin)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn run_official_login(name: &str) -> Result<()> {
    let bin = official_cli_bin(name).ok_or_else(|| anyhow!("공식 CLI가 없습니다"))?;
    println!("{bin} login 을 실행합니다. 브라우저가 열리면 로그인하세요.");
    let status = Command::new(&bin).arg("login").status()?;
    if !status.success() {
        return Err(anyhow!("{bin} login 이 실패했습니다"));
    }
    Ok(())
}

fn cli_credential_paths(name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match name {
        "openai" => {
            if let Ok(home) = std::env::var("CODEX_HOME") {
                out.push(PathBuf::from(home).join("auth.json"));
            }
            if let Some(h) = home_dir() {
                out.push(h.join(".codex").join("auth.json"));
            }
        }
        "anthropic" => {
            if let Some(h) = home_dir() {
                out.push(h.join(".claude").join(".credentials.json"));
                out.push(h.join(".config").join("claude").join(".credentials.json"));
            }
        }
        "gemini" => {
            if let Ok(dir) = std::env::var("GEMINI_CONFIG_DIR") {
                out.push(PathBuf::from(dir).join("oauth_creds.json"));
            }
            if let Some(h) = home_dir() {
                out.push(h.join(".gemini").join("oauth_creds.json"));
            }
        }
        _ => {}
    }
    out
}

/// 부팅 때 공식 CLI(Claude Code · Codex · Gemini)의 로컬 로그인 파일이 있고
/// 아직 연결되지 않은 OAuth 프로바이더라면 조용히 가져온다. 프로세스당 1회만 시도.
pub fn auto_import_cli_logins(cfg: &Config) -> Vec<String> {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if ONCE.set(()).is_err() {
        return Vec::new();
    }
    let mut notes = Vec::new();
    for name in ["anthropic", "openai", "gemini"] {
        let Some(p) = cfg.file.providers.get(name) else {
            continue;
        };
        if is_connected(cfg, name) || auth_mode(name, p) != "oauth" {
            continue;
        }
        if !cli_import_available(name) {
            continue;
        }
        match import_official_cli(name, name, false) {
            Ok(_) => notes.push(format!(
                "{}: 로컬 로그인 정보를 {} 에서 자동으로 가져왔습니다.",
                provider_label(name),
                official_cli_label(name)
            )),
            Err(e) => crate::applog::info(&format!("auto-import {name}: {e}")),
        }
    }
    notes
}

fn import_official_cli(name: &str, account_id: &str, announce: bool) -> Result<()> {
    let paths = cli_credential_paths(name);
    for path in &paths {
        if !path.exists() {
            continue;
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("{} 을 읽을 수 없습니다", path.display()))?;
        let v: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("{} 이 JSON이 아닙니다", path.display()))?;
        if let Some(set) = parse_cli_tokens(name, &v) {
            save_tokens(account_id, set)?;
            if announce {
                crate::ui::note(&format!(
                    "{} 에서 가져왔습니다: {}",
                    official_cli_label(name),
                    path.display()
                ));
            }
            return Ok(());
        }
    }
    Err(anyhow!(
        "{} 로그인 파일을 찾지 못했습니다. 공식 프로그램에서 먼저 로그인하거나 브라우저 로그인을 고르세요.",
        official_cli_label(name)
    ))
}

fn parse_cli_tokens(name: &str, v: &serde_json::Value) -> Option<TokenSet> {
    match name {
        "openai" => {
            let tokens = v.get("tokens").unwrap_or(v);
            let access = tokens.get("access_token")?.as_str()?.to_string();
            if access.is_empty() {
                return None;
            }
            let refresh = tokens
                .get("refresh_token")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let id_token = tokens.get("id_token").and_then(|x| x.as_str());
            let account = chatgpt_account_id(
                &access,
                id_token,
                tokens.get("account_id").and_then(|x| x.as_str()),
            );
            Some(TokenSet {
                expires_at: jwt_exp(&access).unwrap_or(now_secs() + 3300),
                access_token: access,
                refresh_token: refresh,
                account_id: account,
            })
        }
        "anthropic" => {
            let oauth = v.get("claudeAiOauth").unwrap_or(v);
            let access = oauth
                .get("accessToken")
                .or_else(|| oauth.get("access_token"))?
                .as_str()?
                .to_string();
            if access.is_empty() {
                return None;
            }
            let refresh = oauth
                .get("refreshToken")
                .or_else(|| oauth.get("refresh_token"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let expires_at = oauth
                .get("expiresAt")
                .or_else(|| oauth.get("expires_at"))
                .and_then(|x| x.as_i64())
                .map(|n| if n > 10_000_000_000 { n / 1000 } else { n })
                .unwrap_or(0);
            Some(TokenSet {
                access_token: access,
                refresh_token: refresh,
                expires_at,
                account_id: String::new(),
            })
        }
        "gemini" => {
            let access = v.get("access_token")?.as_str()?.to_string();
            if access.is_empty() {
                return None;
            }
            let refresh = v
                .get("refresh_token")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let expires_at = v
                .get("expiry_date")
                .or_else(|| v.get("expiry"))
                .and_then(|x| x.as_i64())
                .map(|n| if n > 10_000_000_000 { n / 1000 } else { n })
                .unwrap_or(0);
            Some(TokenSet {
                access_token: access,
                refresh_token: refresh,
                expires_at,
                account_id: String::new(),
            })
        }
        _ => None,
    }
}

fn chatgpt_account_id(access: &str, id_token: Option<&str>, explicit: Option<&str>) -> String {
    if let Some(s) = explicit
        && !s.is_empty()
    {
        return s.to_string();
    }
    for t in [Some(access), id_token].into_iter().flatten() {
        if let Some(id) = jwt_chatgpt_account(t) {
            return id;
        }
    }
    String::new()
}

fn jwt_chatgpt_account(token: &str) -> Option<String> {
    let v = jwt_payload(token)?;
    if let Some(id) = v
        .get("https://api.openai.com/auth")
        .and_then(|x| x.get("chatgpt_account_id"))
        .and_then(|x| x.as_str())
        && !id.is_empty()
    {
        return Some(id.to_string());
    }
    v.get("chatgpt_account_id")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn jwt_exp(token: &str) -> Option<i64> {
    jwt_payload(token)?
        .get("exp")
        .and_then(|x| x.as_i64())
        .map(|n| n - 60)
}

fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    let mid = token.split('.').nth(1)?;
    let bytes = b64url_decode(mid)?;
    serde_json::from_slice(&bytes).ok()
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut t = s.replace('-', "+").replace('_', "/");
    while !t.len().is_multiple_of(4) {
        t.push('=');
    }
    let b = t.as_bytes();
    let val = |c: u8| -> Option<u8> {
        if c == b'=' {
            return Some(0);
        }
        T.iter().position(|&x| x == c).map(|i| i as u8)
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < b.len() {
        let a = val(b[i])?;
        let c1 = val(b[i + 1])?;
        let c2 = val(b[i + 2])?;
        let c3 = val(b[i + 3])?;
        out.push((a << 2) | (c1 >> 4));
        if b[i + 2] != b'=' {
            out.push((c1 << 4) | (c2 >> 2));
        }
        if b[i + 3] != b'=' {
            out.push((c2 << 6) | c3);
        }
        i += 4;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 카탈로그 스탬프·만료 판단 테스트용 — auth=none 로컬 프로바이더 하나뿐인 설정.
    /// data_dir 을 temp 로 독립시켜 실제 사용자 데이터를 건드리지 않는다.
    fn catalog_test_cfg(tag: &str) -> (Config, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rafikx-catalog-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut file: crate::config::ConfigFile =
            toml::from_str(crate::config::DEFAULT_CONFIG).unwrap();
        file.providers.clear();
        file.providers.insert(
            "local".into(),
            crate::config::ProviderConfig {
                kind: "openai_compat".into(),
                auth: "none".into(),
                api_key_env: String::new(),
                model: "qwen3:8b".into(),
                small_model: None,
                base_url: None,
                supports_tools: false,
                models_url: None,
                model_auto: false,
                context_window: None,
                enabled: true,
            },
        );
        let cfg = Config {
            path: dir.join("config.toml"),
            data_dir: dir.clone(),
            workspace: dir.clone(),
            file,
            loaded_at: None,
        };
        (cfg, dir)
    }

    #[test]
    fn catalog_stamp_roundtrip_and_staleness() {
        let (cfg, dir) = catalog_test_cfg("roundtrip");

        // 스탬프가 없으면 (등록 직후, 받은 적 없음) 갱신 대상이다.
        assert!(catalogs_stale(&cfg));

        // 저장하면 스탬프가 함께 찍혀 하루 안으로 본다.
        save_catalog(&cfg, "local", &["qwen3:8b".to_string()]).unwrap();
        assert!(!catalogs_stale(&cfg));
        assert_eq!(cached_catalog(&cfg, "local"), vec!["qwen3:8b".to_string()]);
        assert!(catalog_touched_at(&cfg, "local").is_some());

        // 하루 이전 스탬프는 다시 갱신 대상.
        let path = catalogs_meta_file(&cfg);
        let old = now_secs() - 25 * 60 * 60;
        fs::write(&path, format!(r#"{{"local":{old}}}"#)).unwrap();
        assert!(catalogs_stale(&cfg));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_meta_keys_are_per_provider() {
        let (cfg, dir) = catalog_test_cfg("perkey");
        save_catalog(&cfg, "local", &["a".to_string(), "b".to_string()]).unwrap();
        assert!(catalog_touched_at(&cfg, "other-provider").is_none());
        assert!(catalog_touched_at(&cfg, "local").is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_models_url_carries_client_version() {
        let url = codex_models_url();
        assert!(url.starts_with("https://chatgpt.com/backend-api/codex/models?"));
        assert!(url.contains(&format!("client_version={}", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn pkce_challenge_is_base64url() {
        let p = generate_pkce();
        assert!(p.verifier.len() >= 32);
        assert!(
            p.challenge
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert!(!p.challenge.contains('='));
    }

    #[test]
    fn picks_preferred_in_order() {
        let avail = vec![
            "claude-haiku-4-5".into(),
            "claude-sonnet-4-6".into(),
            "other".into(),
        ];
        let (main, small) = pick_preferred(&avail, "anthropic");
        assert_eq!(main.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(small.as_deref(), Some("claude-haiku-4-5"));
    }

    #[test]
    fn internet_shortcut_keeps_query() {
        let url = "https://claude.ai/oauth/authorize?client_id=abc&response_type=code";
        let body = format!("[InternetShortcut]\r\nURL={url}\r\n");
        assert!(body.contains("client_id=abc&response_type=code"));
        assert!(body.contains("[InternetShortcut]"));
    }

    #[test]
    fn openai_authorize_has_required_params() {
        let url = openai_authorize_url("challenge", "state-1");
        assert!(url.contains("client_id="));
        assert!(url.contains("redirect_uri="));
        assert!(url.contains("localhost%3A1455"));
        assert!(url.contains("state=state-1"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("originator=codex_cli_rs"));
        assert!(url.contains("offline_access"));
        assert!(!url.contains("originator=rafikx"));
    }

    #[test]
    fn parses_codex_auth_json() {
        let v = serde_json::json!({
            "tokens": {
                "access_token": "tok-abc",
                "refresh_token": "ref-1",
                "account_id": "acct-9"
            }
        });
        let set = parse_cli_tokens("openai", &v).expect("parse");
        assert_eq!(set.access_token, "tok-abc");
        assert_eq!(set.refresh_token, "ref-1");
        assert_eq!(set.account_id, "acct-9");
    }

    #[test]
    fn jwt_account_from_payload() {
        // {"https://api.openai.com/auth":{"chatgpt_account_id":"org-1"}}
        let payload = b64url(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"org-1"}}"#);
        let token = format!("x.{payload}.y");
        assert_eq!(jwt_chatgpt_account(&token).as_deref(), Some("org-1"));
    }

    #[test]
    fn provider_of_account_parses_suffix() {
        assert_eq!(provider_of_account("anthropic::5"), "anthropic");
        assert_eq!(provider_of_account("openai"), "openai");
    }

    #[test]
    fn opencode_aliases_and_env_names() {
        assert_eq!(
            resolve_provider_alias("zen").as_deref(),
            Some("opencode_zen")
        );
        assert_eq!(
            resolve_provider_alias("opencode-go").as_deref(),
            Some("opencode_go")
        );
        let zen = api_key_env_names("opencode_zen", "OPENCODE_API_KEY");
        assert!(zen.iter().any(|n| n == "OPENCODE_API_KEY"));
        let go = api_key_env_names("opencode_go", "OPENCODE_GO_API_KEY");
        assert!(go.iter().any(|n| n == "OPENCODE_GO_API_KEY"));
        assert!(go.iter().any(|n| n == "OPENCODE_API_KEY"));
    }

    #[test]
    fn refresh_summary_lists_counts_failures_and_total() {
        let rows = vec![
            CatalogRefresh {
                provider: "minimax".into(),
                result: Ok(12),
            },
            CatalogRefresh {
                provider: "anthropic".into(),
                result: Err("HTTP 401".into()),
            },
            // 조회는 됐지만 목록이 비면 0개 — 캐시는 건드리지 않는다.
            CatalogRefresh {
                provider: "openai".into(),
                result: Ok(0),
            },
        ];
        let out = refresh_summary(&rows);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], "minimax  12개");
        assert_eq!(out[1], "anthropic  실패: HTTP 401");
        assert_eq!(out[2], "openai  0개");
        assert_eq!(out[3], "총 3곳 · 모델 12개 · 실패 1곳");
    }

    #[test]
    fn refresh_summary_without_failures_omits_failure_tail() {
        let rows = vec![CatalogRefresh {
            provider: "glm".into(),
            result: Ok(3),
        }];
        let out = refresh_summary(&rows);
        assert_eq!(out, vec!["glm  3개", "총 1곳 · 모델 3개"]);
    }

    #[test]
    fn refresh_summary_empty_input_guides_connect() {
        let out = refresh_summary(&[]);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("/connect"));
    }

    #[test]
    fn short_error_trims_to_one_line_within_120_chars() {
        let long = "x".repeat(300);
        let e = anyhow::anyhow!("{long}\n두 번째 줄");
        let s = short_error(&e);
        assert_eq!(s.chars().count(), 120);
        assert!(!s.contains('\n'));
    }
}
