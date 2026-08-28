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
pub struct HarnessManual {
    pub class: String,
    /// 수동 지정값 ("" = 자동)
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootInfo {
    pub version: String,
    pub config_path: String,
    pub data_dir: String,
    pub workspace: String,
    pub default_provider: String,
    pub harness: String,
    /// auto | manual
    pub harness_mode: String,
    /// 분류별 수동 지정 현황
    pub harness_manual: Vec<HarnessManual>,
    /// 순위표 기준일·교차검증 표기
    pub ranks_status: String,
    /// 사이드바 Harness 요약 (분류 → 프로파일 → 모델)
    pub harness_rows: Vec<HarnessRow>,
    /// 컴포저 칩용 기본 모델 표기
    pub default_model: String,
    /// 데스크탑 배경 모드: light | dark | auto
    pub appearance: String,
    /// Harness 엔진: rafikx (기본) | claude | deepseek | qwen | kimi | pi (legacy self)
    pub engine: String,
    pub obsidian: ObsidianInfo,
    pub providers: Vec<ProviderInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessRow {
    pub class: String,
    pub profile: String,
    pub provider: String,
    pub model: String,
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
    /// 이 답변에 걸린 시간 (밀리초)
    pub elapsed_ms: u64,
    pub session_id: Option<String>,
    pub graph: Vec<graph::GraphNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<crate::lifecycle::LifecycleState>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lifecycle: Vec<crate::lifecycle::LifecycleEvent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context_sources: Vec<crate::run::ContextSourceRecord>,
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
    /// /ulw — 자율 루프 목표 (데스크탑은 이 필드를 받아 루프를 실행한다)
    pub ulw_goal: Option<String>,
    /// /ulw-resume — Some(id) 또는 Some("")=최근 실행, None=해당 없음
    pub ulw_resume: Option<String>,
    /// true 면 호출자가 compact_session 을 실행해야 한다.
    pub compact: bool,
    /// true 면 호출자가 assign_roles 를 실행해야 한다 (/engine multi).
    pub assign: bool,
    /// true 면 호출자가 refresh_models 를 실행해야 한다 (/model refresh).
    pub model_fetch: bool,
}

pub fn boot() -> Result<BootInfo> {
    crate::ui::init();
    // 주 1회 모델 순위 갱신은 백그라운드로 — 부트를 막지 않는다.
    crate::ranks::spawn_weekly_refresh();
    let cfg = Config::load(None)?;
    // 하루 1회 모델 카탈로그 자동 갱신 — 역시 백그라운드.
    crate::auth::spawn_catalog_refresh(&cfg);
    for note in crate::auth::auto_import_cli_logins(&cfg) {
        crate::ui::note(&note);
    }
    Ok(boot_with(&cfg))
}

pub fn boot_with(cfg: &Config) -> BootInfo {
    let vault = crate::config::expand_tilde(&cfg.file.obsidian.vault_path);
    let names = auth::menu_provider_names(cfg);
    let providers: Vec<ProviderInfo> = names.iter().map(|n| provider_info(cfg, n)).collect();
    let harness_rows = [
        crate::harness::TaskClass::Simple,
        crate::harness::TaskClass::Medium,
        crate::harness::TaskClass::Advanced,
        crate::harness::TaskClass::Dev,
    ]
    .iter()
    .map(|c| match crate::harness::bind(cfg, *c, None, None) {
        Ok(b) => HarnessRow {
            class: b.class.as_str().into(),
            profile: b.profile_name.clone(),
            provider: b.provider_name.clone(),
            model: b.model.clone(),
        },
        Err(_) => HarnessRow {
            class: c.as_str().into(),
            profile: crate::harness::profile_name_for(cfg, *c).to_string(),
            provider: String::new(),
            model: "(미연결)".into(),
        },
    })
    .collect();
    let default_model = providers
        .iter()
        .find(|p| p.is_default && p.connected)
        .or_else(|| providers.iter().find(|p| p.connected))
        .map(|p| format!("{} · {}", p.label, p.model))
        .unwrap_or_else(|| "연결 없음".into());
    let harness_manual = [
        crate::harness::TaskClass::Simple,
        crate::harness::TaskClass::Medium,
        crate::harness::TaskClass::Advanced,
        crate::harness::TaskClass::Dev,
    ]
    .iter()
    .map(|c| {
        let h = &cfg.file.harness;
        let value = match c {
            crate::harness::TaskClass::Simple => h.manual_simple.clone(),
            crate::harness::TaskClass::Medium => h.manual_medium.clone(),
            crate::harness::TaskClass::Advanced => h.manual_design.clone(),
            crate::harness::TaskClass::Dev => h.manual_debug.clone(),
        }
        .unwrap_or_default();
        HarnessManual {
            class: c.as_str().into(),
            value,
        }
    })
    .collect();
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
        harness_mode: if cfg.file.harness.selection.eq_ignore_ascii_case("manual") {
            "manual".into()
        } else {
            "auto".into()
        },
        harness_manual,
        ranks_status: crate::ranks::status_line(),
        appearance: cfg.file.ui.appearance.clone(),
        engine: {
            // 옛 값(dk·self) 흡수는 engine::normalize 한 곳에서 처리한다.
            // self 는 설정에 적힌 그대로 보여준다 (legacy 표시 유지).
            let (name, legacy_self) = crate::engine::normalize(&cfg.file.general.engine);
            if legacy_self { "self".into() } else { name }
        },
        harness_rows,
        default_model,
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
                    ContentBlock::ToolResult {
                        content, is_error, ..
                    } => {
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
    run_turn_observed(session, prompt, obsidian, local_ask, None).await
}

pub async fn run_turn_observed(
    session: &mut Session,
    prompt: &str,
    obsidian: bool,
    local_ask: Option<LocalAsk>,
    observer: Option<chat::RunObserver>,
) -> Result<TurnResult> {
    session.cfg = Config::load(None)?;
    let class = session.class.clone();
    let info = chat::run_turn_observed(
        session,
        prompt,
        class.as_deref(),
        obsidian,
        local_ask,
        observer,
    )
    .await?;
    if session.dirty {
        let _ = chat::save_if_dirty(session);
    }
    Ok(turn_result_of(session, info))
}

/// TurnInfo → TurnResult 변환 (ulw 루프처럼 chat:: 을 직접 부르는 경로도 같은 조립을 쓴다).
pub fn turn_result_of(session: &Session, info: chat::TurnInfo) -> TurnResult {
    let nodes = if info.run_id.is_empty() {
        Vec::new()
    } else {
        graph::for_run(&info.run_id).unwrap_or_default()
    };
    TurnResult {
        run_id: info.run_id,
        label: info.label,
        status: info.status,
        tokens_in: info.tokens_in,
        tokens_out: info.tokens_out,
        elapsed_ms: info.elapsed_ms,
        session_id: session.session_id.clone(),
        graph: nodes,
        lifecycle_state: info.lifecycle_state,
        lifecycle: info.lifecycle,
        context_sources: info.context_sources,
    }
}

pub fn apply_slash(session: &mut Session, line: &str) -> Result<SlashResult> {
    match chat::handle_slash(session, line, false)? {
        Slash::Continue(notes) | Slash::New(notes) => Ok(SlashResult {
            notes: notes.join("\n"),
            quit: false,
            agent_task: None,
            ulw_goal: None,
            ulw_resume: None,
            compact: false,
            assign: false,
            model_fetch: false,
        }),
        Slash::Quit => Ok(SlashResult {
            notes: "세션을 닫습니다.".into(),
            quit: true,
            agent_task: None,
            ulw_goal: None,
            ulw_resume: None,
            compact: false,
            assign: false,
            model_fetch: false,
        }),
        Slash::Ulw { goal } => Ok(SlashResult {
            notes: String::new(),
            quit: false,
            agent_task: None,
            ulw_goal: Some(goal),
            ulw_resume: None,
            compact: false,
            assign: false,
            model_fetch: false,
        }),
        Slash::UlwResume { run_id } => Ok(SlashResult {
            notes: String::new(),
            quit: false,
            agent_task: None,
            ulw_goal: None,
            ulw_resume: Some(run_id.unwrap_or_default()),
            compact: false,
            assign: false,
            model_fetch: false,
        }),
        Slash::Agent(task) => Ok(SlashResult {
            notes: String::new(),
            quit: false,
            agent_task: Some(task),
            ulw_goal: None,
            ulw_resume: None,
            compact: false,
            assign: false,
            model_fetch: false,
        }),
        Slash::Compact => Ok(SlashResult {
            notes: String::new(),
            quit: false,
            agent_task: None,
            ulw_goal: None,
            ulw_resume: None,
            compact: true,
            assign: false,
            model_fetch: false,
        }),
        Slash::AssignRoles => Ok(SlashResult {
            notes: String::new(),
            quit: false,
            agent_task: None,
            ulw_goal: None,
            ulw_resume: None,
            compact: false,
            assign: true,
            model_fetch: false,
        }),
        // 조회(fetch)는 비동기라 호출자에게 넘기고, 검색어만 온 경우는
        // 여기서 걸러진 목록을 바로 돌려준다 (RPC 쪽엔 피커가 없다).
        Slash::ModelFetch { query, fetch } => {
            let notes = if fetch {
                String::new()
            } else {
                let regs = crate::auth::registered_models(&session.cfg);
                chat::model_list_notes(&regs, &query).join("\n")
            };
            Ok(SlashResult {
                notes,
                quit: false,
                agent_task: None,
            ulw_goal: None,
            ulw_resume: None,
                compact: false,
                assign: false,
                model_fetch: fetch,
            })
        }
    }
}

/// /engine multi 공용 실행 — 등록 연결의 모델을 조회해 역할별로 배정한다.
pub async fn assign_roles(session: &mut Session) -> Result<String> {
    let notes = crate::harness::auto_assign_roles(&session.cfg).await?;
    if let Ok(cfg) = session.cfg.reload() {
        session.cfg = cfg;
    }
    session.sticky = None;
    Ok(notes.join("\n"))
}

/// /model refresh 공용 실행 — 연결된 서비스의 원격 모델 목록을 다시 조회해 캐시에 저장한다.
/// 요약과 갱신된 모델 목록을 함께 돌려준다.
pub async fn refresh_models(session: &Session) -> Result<String> {
    let rows = auth::refresh_catalogs(&session.cfg).await;
    let mut lines = auth::refresh_summary(&rows);
    let regs = auth::registered_models(&session.cfg);
    lines.extend(chat::model_list_notes(&regs, ""));
    Ok(lines.join("\n"))
}

/// /compact 공용 실행 — 세션 메시지를 요약 하나로 압축한다.
pub async fn compact_session(session: &mut Session) -> Result<String> {
    let len = chat::compact_session(session).await?;
    if session.dirty {
        let _ = chat::save_if_dirty(session);
    }
    Ok(format!("대화를 {len}자 요약으로 압축했습니다."))
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
    accounts_ui::write_provider_model(&cfg, name, model)?;
    crate::chat::persist_last_choice(&cfg, name, model);
    Ok(())
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

/// 데스크탑 배경 모드 저장 (light | dark | auto). 잘못된 값은 auto 로 정규화.
pub fn set_appearance(mode: &str) -> Result<String> {
    let m = match mode.trim().to_ascii_lowercase().as_str() {
        "light" => "light",
        "dark" => "dark",
        _ => "auto",
    };
    let cfg = Config::load(None)?;
    crate::config::write_toml_key(
        &cfg.path,
        "[ui]",
        "appearance",
        &crate::config::toml_string(m),
    )?;
    Ok(m.to_string())
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

/// 연결된 프로바이더의 원격 모델 목록 (키 등록 후 실제 사용 가능한 모델 검색).
pub async fn remote_models(provider: &str) -> Result<Vec<String>> {
    let cfg = Config::load(None)?;
    auth::list_remote_models(&cfg, provider).await
}

/// Harness 엔진 저장 (rafikx | claude | deepseek | qwen | kimi | pi | minimax, legacy self).
pub fn set_engine(name: &str) -> Result<String> {
    let cfg = Config::load(None)?;
    set_engine_for(&cfg, name)
}

pub(crate) fn set_engine_for(cfg: &Config, name: &str) -> Result<String> {
    let e = name.trim().to_ascii_lowercase();
    if !crate::chat::is_valid_engine(&e) {
        anyhow::bail!(
            "엔진은 {}|self 중 하나여야 합니다",
            crate::engine::names_joined()
        );
    }
    crate::config::write_toml_key(
        &cfg.path,
        "[general]",
        "engine",
        &crate::config::toml_string(&e),
    )?;
    let note = match e.as_str() {
        "self" => " — Self-Harness 자기개선 루프 (실패 채굴→Harness 수정 제안→회귀 검증 후 승격)"
            .to_string(),
        other => crate::engine::resolve(other)
            .map(|spec| format!(" — {}", spec.summary))
            .unwrap_or_default(),
    };
    Ok(format!("Harness 엔진: {e}{note}"))
}

/// 실행 분야 저장 (harness | loop | graph). 미지원 값은 거부한다.
pub fn set_discipline(name: &str) -> Result<String> {
    let cfg = Config::load(None)?;
    set_discipline_for(&cfg, name)
}

pub(crate) fn set_discipline_for(cfg: &Config, name: &str) -> Result<String> {
    let raw = name.trim().to_ascii_lowercase();
    let d = crate::engine::normalize_discipline(&raw);
    if d.as_str() != raw {
        anyhow::bail!(
            "분야는 {} 중 하나여야 합니다",
            crate::engine::discipline_names_joined()
        );
    }
    crate::config::write_toml_key(
        &cfg.path,
        "[general]",
        "discipline",
        &crate::config::toml_string(d.as_str()),
    )?;
    Ok(format!("실행 분야: {} — {}", d.as_str(), d.summary()))
}

/// 팀 모드 저장 (single | multi). 미지원 값은 거부한다.
pub fn set_team(name: &str) -> Result<String> {
    let cfg = Config::load(None)?;
    set_team_for(&cfg, name)
}

pub(crate) fn set_team_for(cfg: &Config, name: &str) -> Result<String> {
    let raw = name.trim().to_ascii_lowercase();
    let t = crate::engine::normalize_team(&raw);
    if t.as_str() != raw {
        anyhow::bail!(
            "팀 모드는 {} 중 하나여야 합니다",
            crate::engine::team_names_joined()
        );
    }
    crate::config::write_toml_key(
        &cfg.path,
        "[harness]",
        "team",
        &crate::config::toml_string(t.as_str()),
    )?;
    Ok(format!("팀 모드: {} — {}", t.as_str(), t.summary()))
}

/// Self-Harness 메타 레이어 토글 저장 — 어떤 엔진 위에도 자기개선 루프를 겹친다.
pub fn set_self_meta(on: bool) -> Result<String> {
    let cfg = Config::load(None)?;
    set_self_meta_for(&cfg, on)
}

pub(crate) fn set_self_meta_for(cfg: &Config, on: bool) -> Result<String> {
    // [self_harness] 섹션이 없는 옛 config 면 upsert 가 섹션째 만들어 붙인다.
    crate::config::write_toml_key(
        &cfg.path,
        "[self_harness]",
        "meta",
        if on { "true" } else { "false" },
    )?;
    Ok(if on {
        "Self-Harness 메타: on — 모든 엔진 위에 자기개선 루프를 겹칩니다.".into()
    } else {
        "Self-Harness 메타: off".into()
    })
}

/// Harness 선정 모드 저장 (auto | manual).
pub fn set_harness_selection(mode: &str) -> Result<String> {
    let cfg = Config::load(None)?;
    crate::harness::set_selection_mode(&cfg, mode)?;
    Ok(if mode.eq_ignore_ascii_case("manual") {
        "manual".into()
    } else {
        "auto".into()
    })
}

/// 분류별 수동 모델 지정. 빈 값이면 자동으로 되돌린다.
pub fn set_harness_model(class: &str, spec: &str) -> Result<String> {
    let cfg = Config::load(None)?;
    let tc = crate::harness::TaskClass::parse(class)
        .ok_or_else(|| anyhow::anyhow!("분류는 simple|medium|advanced|dev 중 하나여야 합니다"))?;
    crate::harness::set_manual_model(&cfg, tc, spec)
}

pub fn detect_workspace() -> String {
    std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}
