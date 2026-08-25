#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rafikx::agent::{ApprovalChoice, LocalAsk};
use rafikx::api;
use rafikx::chat::Session;
use rafikx::config::Config;
use rafikx::db::Db;
use rafikx::obsidian;
use rafikx::ui::{self, Live};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{oneshot, watch, Mutex as AsyncMutex};

struct Inner {
    sessions: Mutex<HashMap<String, Session>>,
    pending: Mutex<HashMap<String, oneshot::Sender<ApprovalChoice>>>,
    turn: AsyncMutex<()>,
    watch_stop: Mutex<Option<watch::Sender<bool>>>,
}

type Shared = Arc<Inner>;

#[derive(Serialize, Clone)]
struct LivePayload {
    kind: String,
    text: String,
}

#[derive(Serialize, Clone)]
struct ApprovalPayload {
    id: String,
    preview: String,
}

#[derive(Serialize)]
struct SessionDto {
    id: String,
    messages: Vec<api::ChatMessage>,
    obsidian_on: bool,
    class: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    mode: String,
}

#[derive(Serialize)]
struct SendResult {
    kind: String,
    notes: String,
    quit: bool,
    turn: Option<api::TurnResult>,
    messages: Vec<api::ChatMessage>,
    session_id: Option<String>,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn take_session(state: &Inner, sid: &str) -> Result<Session, String> {
    state
        .sessions
        .lock()
        .map_err(err)?
        .remove(sid)
        .ok_or_else(|| "세션이 없습니다. 새로 시작하세요.".into())
}

fn put_session(state: &Inner, sid: String, session: Session) -> Result<(), String> {
    state.sessions.lock().map_err(err)?.insert(sid, session);
    Ok(())
}

fn dto(id: String, session: &Session) -> SessionDto {
    SessionDto {
        id,
        messages: api::transcript(session),
        obsidian_on: session.obsidian_on,
        class: session.class.clone(),
        provider: session.provider.clone(),
        model: session.model.clone(),
        mode: session.mode.clone(),
    }
}

fn install_live(app: AppHandle) {
    ui::set_live(Some(Arc::new(move |ev: Live| {
        let (kind, text) = match ev {
            Live::Chunk(t) => ("chunk", t),
            Live::Assistant(t) => ("assistant", t),
            Live::System(t) => ("system", t),
            Live::Warn(t) => ("warn", t),
            Live::Status(t) => ("status", t),
            Live::Todo(items) => (
                "todo",
                items
                    .into_iter()
                    .map(|item| format!("{}:{}", item.status, item.content))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Live::Agent(agent) => (
                "agent",
                format!("{}:{}:{}:{}", agent.id, agent.role, agent.model, agent.status),
            ),
        };
        let _ = app.emit(
            "live",
            LivePayload {
                kind: kind.into(),
                text,
            },
        );
    })));
}

fn local_ask(app: AppHandle, inner: Shared) -> LocalAsk {
    Arc::new(move |preview: String| {
        let inner = inner.clone();
        let app = app.clone();
        Box::pin(async move {
            let id = Db::new_id();
            let (tx, rx) = oneshot::channel();
            if let Ok(mut p) = inner.pending.lock() {
                p.insert(id.clone(), tx);
            }
            let _ = app.emit(
                "approval",
                ApprovalPayload {
                    id: id.clone(),
                    preview,
                },
            );
            match tokio::time::timeout(Duration::from_secs(300), rx).await {
                Ok(Ok(choice)) => choice,
                _ => ApprovalChoice::No,
            }
        })
    })
}

#[tauri::command]
fn boot() -> Result<api::BootInfo, String> {
    api::boot().map_err(err)
}

#[tauri::command]
fn list_sessions() -> Result<Vec<api::SessionInfo>, String> {
    api::list_sessions().map_err(err)
}

#[tauri::command]
fn new_session(
    state: State<Shared>,
    provider: Option<String>,
    model: Option<String>,
    class: Option<String>,
    resume: Option<String>,
) -> Result<SessionDto, String> {
    let session = api::open_chat(provider, model, class, resume.clone(), false).map_err(err)?;
    let id = resume.unwrap_or_else(|| format!("draft-{}", Db::new_id()));
    let out = dto(id.clone(), &session);
    put_session(&state, id, session)?;
    Ok(out)
}

#[tauri::command]
async fn send(
    app: AppHandle,
    state: State<'_, Shared>,
    sid: String,
    prompt: String,
    obsidian: bool,
    class: Option<String>,
    mode: Option<String>,
) -> Result<SendResult, String> {
    let _guard = state.turn.lock().await;
    let inner = (*state).clone();
    let mut session = take_session(&inner, &sid)?;
    if let Some(c) = class {
        session.class = if c.trim().is_empty() {
            None
        } else {
            Some(c)
        };
    }
    if let Some(m) = mode {
        session.mode = if m.eq_ignore_ascii_case("plan") {
            "plan".into()
        } else {
            "build".into()
        };
    }
    session.obsidian_on = obsidian;

    let mut prompt = prompt.trim().to_string();
    if prompt.starts_with('/') && !prompt.starts_with("/agent ") {
        let slash = api::apply_slash(&mut session, &prompt).map_err(err)?;
        if let Some(task) = slash.agent_task {
            prompt = task;
            session.class = Some("dev".into());
        } else if slash.compact {
            let notes = api::compact_session(&mut session).await.map_err(err)?;
            let messages = api::transcript(&session);
            let session_id = session.session_id.clone();
            put_session(&inner, sid, session)?;
            return Ok(SendResult {
                kind: "compact".into(),
                notes,
                quit: false,
                turn: None,
                messages,
                session_id,
            });
        } else {
            let messages = api::transcript(&session);
            let session_id = session.session_id.clone();
            put_session(&inner, sid, session)?;
            return Ok(SendResult {
                kind: "slash".into(),
                notes: slash.notes,
                quit: slash.quit,
                turn: None,
                messages,
                session_id,
            });
        }
    } else if let Some(rest) = prompt.strip_prefix("/agent ") {
        prompt = rest.trim().to_string();
        session.class = Some("dev".into());
    }

    let ask = local_ask(app.clone(), inner.clone());
    install_live(app.clone());
    let result = api::run_turn(&mut session, &prompt, obsidian, Some(ask)).await;
    ui::set_live(None);
    let messages = api::transcript(&session);
    let session_id = session.session_id.clone();
    put_session(&inner, sid, session)?;
    let turn = result.map_err(err)?;
    Ok(SendResult {
        kind: "turn".into(),
        notes: String::new(),
        quit: false,
        turn: Some(turn),
        messages,
        session_id,
    })
}

#[tauri::command]
fn resolve_approval(state: State<Shared>, id: String, choice: String) -> Result<(), String> {
    let tx = state
        .pending
        .lock()
        .map_err(err)?
        .remove(&id)
        .ok_or_else(|| "승인 요청이 만료되었습니다.".to_string())?;
    let choice = match choice.as_str() {
        "yes" => ApprovalChoice::Yes,
        "always" => ApprovalChoice::Always,
        _ => ApprovalChoice::No,
    };
    let _ = tx.send(choice);
    Ok(())
}

#[tauri::command]
async fn save_key(app: AppHandle, provider: String, key: String) -> Result<String, String> {
    let out = api::save_key(&provider, &key).map_err(err)?;
    // 키가 정상이면 원격 모델 목록을 자동으로 불러와 카탈로그에 저장한다.
    let cfg = Config::load(None).ok();
    let pname = provider.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(cfg) = cfg {
            match rafikx::auth::list_remote_models(&cfg, &pname).await {
                Ok(models) if !models.is_empty() => {
                    let _ = rafikx::auth::save_catalog(&cfg, &pname, &models);
                    if let Some(m) = rafikx::auth::pick_preferred(&models, &pname).0 {
                        let _ = api::set_provider_model(&pname, &m);
                    }
                    let _ = app.emit(
                        "live",
                        LivePayload {
                            kind: "system".into(),
                            text: format!("[models] {pname} 사용 가능 {}개 — 기본 모델 자동 저장", models.len()),
                        },
                    );
                }
                _ => {}
            }
        }
    });
    Ok(out)
}

#[tauri::command]
fn disconnect_provider(name: String) -> Result<(), String> {
    api::disconnect_provider(&name).map_err(err)
}

#[tauri::command]
fn set_default_provider(name: String) -> Result<(), String> {
    api::set_default_provider(&name).map_err(err)
}

#[tauri::command]
fn set_provider_model(name: String, model: String) -> Result<(), String> {
    api::set_provider_model(&name, &model).map_err(err)
}

#[tauri::command]
fn add_custom_provider(name: String, base_url: String, model: String) -> Result<(), String> {
    api::add_custom_provider(&name, &base_url, &model).map_err(err)
}

#[tauri::command]
fn set_workspace(path: String) -> Result<(), String> {
    api::set_workspace(&path).map_err(err)
}

#[tauri::command]
fn set_appearance(mode: String) -> Result<String, String> {
    api::set_appearance(&mode).map_err(err)
}

#[tauri::command]
fn set_obsidian_vault(path: String) -> Result<(), String> {
    api::set_obsidian_vault(&path).map_err(err)
}

#[tauri::command]
fn set_obsidian_enabled(on: bool) -> Result<(), String> {
    api::set_obsidian_enabled(on).map_err(err)
}

#[tauri::command]
fn index_obsidian() -> Result<String, String> {
    api::index_obsidian().map_err(err)
}

#[tauri::command]
fn search_obsidian(query: String) -> Result<String, String> {
    api::search_obsidian(&query).map_err(err)
}

#[tauri::command]
fn graph_latest() -> Result<Option<(String, Vec<rafikx::graph::GraphNode>)>, String> {
    api::graph_latest().map_err(err)
}

#[tauri::command]
fn catalog_models(provider: String) -> Result<Vec<String>, String> {
    api::catalog_models(&provider).map_err(err)
}

#[tauri::command]
async fn remote_models(provider: String) -> Result<Vec<String>, String> {
    api::remote_models(&provider).await.map_err(err)
}

#[tauri::command]
fn set_engine(name: String) -> Result<String, String> {
    api::set_engine(&name).map_err(err)
}

#[tauri::command]
fn set_harness_selection(mode: String) -> Result<String, String> {
    api::set_harness_selection(&mode).map_err(err)
}

#[tauri::command]
fn set_harness_model(class: String, spec: String) -> Result<String, String> {
    api::set_harness_model(&class, &spec).map_err(err)
}

#[tauri::command]
fn detect_workspace() -> String {
    api::detect_workspace()
}

#[tauri::command]
async fn pick_folder() -> Option<String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("폴더 선택")
            .pick_folder()
            .map(|p| p.display().to_string())
    })
    .await
    .ok()
    .flatten()
}

#[tauri::command]
async fn start_watch(app: AppHandle, state: State<'_, Shared>) -> Result<String, String> {
    if let Some(prev) = state.watch_stop.lock().map_err(err)?.as_ref() {
        let _ = prev.send(true);
    }
    let (tx, rx) = watch::channel(false);
    *state.watch_stop.lock().map_err(err)? = Some(tx);
    let cfg = Config::load(None).map_err(err)?;
    let vault = cfg.file.obsidian.vault_path.clone();
    tauri::async_runtime::spawn(async move {
        match obsidian::watch_vault_until(&cfg, rx).await {
            Ok(_) => {
                let _ = app.emit(
                    "obsidian",
                    LivePayload {
                        kind: "watch".into(),
                        text: "Vault 감시 종료".into(),
                    },
                );
            }
            Err(e) => {
                let _ = app.emit(
                    "obsidian",
                    LivePayload {
                        kind: "error".into(),
                        text: e.to_string(),
                    },
                );
            }
        }
    });
    Ok(format!("Vault 감시 시작 · {vault}"))
}

#[tauri::command]
fn stop_watch(state: State<Shared>) -> Result<(), String> {
    if let Some(tx) = state.watch_stop.lock().map_err(err)?.as_ref() {
        let _ = tx.send(true);
    }
    Ok(())
}

fn main() {
    let inner = Arc::new(Inner {
        sessions: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        turn: AsyncMutex::new(()),
        watch_stop: Mutex::new(None),
    });

    tauri::Builder::default()
        .manage(inner)
        .invoke_handler(tauri::generate_handler![
            boot,
            list_sessions,
            new_session,
            send,
            resolve_approval,
            save_key,
            disconnect_provider,
            set_default_provider,
            set_provider_model,
            add_custom_provider,
            set_workspace,
            set_appearance,
            set_obsidian_vault,
            set_obsidian_enabled,
            index_obsidian,
            search_obsidian,
            graph_latest,
            catalog_models,
            remote_models,
            set_harness_selection,
            set_engine,
            set_harness_model,
            detect_workspace,
            pick_folder,
            start_watch,
            stop_watch
        ])
        .run(tauri::generate_context!())
        .expect("RafikX desktop failed to start");
}
