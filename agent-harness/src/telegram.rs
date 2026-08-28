use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use teloxide::dispatching::Dispatcher;
use teloxide::dptree;
use teloxide::prelude::*;
use teloxide::types::{ChatId, InlineKeyboardButton, InlineKeyboardMarkup};
use teloxide::utils::command::BotCommands;
use tokio::sync::{Mutex, oneshot};

use crate::agent::{self, RemoteApproval};
use crate::applog;
use crate::config::Config;
use crate::db::Db;
use crate::graph;
use crate::harness::{self, Binding};
use crate::inspector;
use crate::lessons;
use crate::obsidian;
use crate::run::{RunContext, RunId};

const TELEGRAM_MAX: usize = 4096;
const START_MSG: &str = "작업 시작…";

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "RafikX 원격 명령")]
enum Command {
    #[command(description = "질문 (Harness)")]
    Ask(String),
    #[command(description = "노트 검색")]
    Obsidian(String),
    #[command(description = "최근 실행·오늘 토큰")]
    Status,
    #[command(description = "현재 실행 취소")]
    Cancel,
    #[command(description = "마지막 점검 요약")]
    Report,
    #[command(description = "교훈 등록")]
    Lesson(String),
    #[command(description = "도움말")]
    Start,
    #[command(description = "도움말")]
    Help,
}

/** 대기 중 원격 승인: 질의 ID → (채팅, 승인 응답 송신 채널). */
type PendingApprovals = Arc<Mutex<HashMap<String, (ChatId, oneshot::Sender<bool>)>>>;

struct App {
    cfg: Config,
    pending: PendingApprovals,
    active: Arc<Mutex<HashMap<ChatId, RunContext>>>,
    seq: AtomicU32,
}

pub async fn run(config_path: Option<&Path>, with_watch: bool) -> Result<()> {
    let cfg = Config::load(config_path)?;
    if !cfg.file.telegram.enabled {
        anyhow::bail!("config의 [telegram] enabled=false 입니다.");
    }
    let env_name = cfg.file.telegram.token_env.trim();
    if env_name.is_empty() {
        anyhow::bail!("[telegram] token_env 가 비어 있습니다");
    }
    let token = crate::auth::telegram_token(&cfg)?.ok_or_else(|| {
        anyhow!("{env_name} 환경변수 또는 secrets.toml 의 telegram 키가 없습니다. rafikx settings 에서 붙여넣으세요.")
    })?;
    if token.trim().is_empty() {
        anyhow::bail!("{env_name} 환경변수가 비어 있습니다");
    }
    let bot = Bot::new(token);
    // 토큰은 로그에 쓰지 않는다.
    applog::info("telegram 데몬 시작");
    println!("텔레그램 봇을 시작합니다. Ctrl+C 로 종료합니다.");
    if cfg.file.telegram.allowed_user_ids.is_empty() {
        println!("주의: allowed_user_ids 가 비어 있습니다. 아무도 명령을 쓸 수 없습니다.");
    }

    let app = Arc::new(App {
        cfg: cfg.clone(),
        pending: Arc::new(Mutex::new(HashMap::new())),
        active: Arc::new(Mutex::new(HashMap::new())),
        seq: AtomicU32::new(1),
    });

    if with_watch {
        let watch_cfg = cfg.clone();
        tokio::spawn(async move {
            if let Err(e) = obsidian::watch_vault(&watch_cfg).await {
                applog::error(&format!("obsidian watch: {e:#}"));
            }
        });
    }

    spawn_inspector_scheduler(bot.clone(), cfg.clone());
    spawn_anomaly_watcher(bot.clone(), cfg.clone());

    let handler = dptree::entry()
        .branch(Update::filter_callback_query().endpoint(on_callback))
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(on_command),
        )
        .branch(Update::filter_message().endpoint(on_text));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![app])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}

/// 이상 감시자 (F: Inspector 강화) — 지표 이상을 주기 없이 즉시 알린다.
/// 코드 계산만 하므로 모델 호출 없이 가볍다. [inspector] anomaly_minutes = 0 이면 끔.
fn spawn_anomaly_watcher(bot: Bot, cfg: Config) {
    let minutes = cfg.file.inspector.anomaly_minutes;
    if minutes == 0 {
        return;
    }
    tokio::spawn(async move {
        // tokio interval 의 첫 tick 은 즉시 발동한다 — 시작 직후가 아니라 첫 주기부터 본다.
        let mut ticker = tokio::time::interval(Duration::from_secs(minutes * 60));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Ok(path) = Db::db_path() else { continue };
            let Ok(db) = Db::open(&path) else { continue };
            match crate::anomaly::check(&cfg, &db) {
                Ok(report) => {
                    let msg = crate::anomaly::render_message(&report);
                    if !msg.is_empty() {
                        notify_owner(&cfg, &msg).await;
                    }
                }
                Err(e) => applog::error(&format!("anomaly 감시: {e:#}")),
            }
        }
    });
}

fn spawn_inspector_scheduler(bot: Bot, cfg: Config) {
    let hours = cfg.file.inspector.auto_interval_hours;
    if hours <= 0.0 || !hours.is_finite() {
        return;
    }
    let secs = (hours * 3600.0).max(1.0);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs_f64(secs));
        ticker.tick().await; // 즉시 실행은 건너뛰고 주기만 지킨다
        loop {
            ticker.tick().await;
            match inspector::generate_report(&cfg, 200, None).await {
                Ok((summary, _)) => {
                    if cfg.file.inspector.notify_telegram {
                        let path = Db::open(&Db::db_path().unwrap_or_default())
                            .ok()
                            .and_then(|db| db.last_report().ok().flatten())
                            .map(|r| r.body_path)
                            .unwrap_or_default();
                        let text = format!("{summary}\n\n전문: {path}");
                        for uid in &cfg.file.telegram.allowed_user_ids {
                            let _ = send_chunks(&bot, ChatId(*uid), &text).await;
                        }
                    }
                }
                Err(e) => applog::error(&format!("inspector 스케줄: {e:#}")),
            }
        }
    });
}

async fn on_command(bot: Bot, msg: Message, cmd: Command, app: Arc<App>) -> ResponseResult<()> {
    if !msg_allowed(&msg, &app) {
        return Ok(());
    }
    match cmd {
        Command::Start | Command::Help => {
            send_chunks(&bot, msg.chat.id, &help_text()).await?;
        }
        Command::Ask(q) => {
            let q = q.trim().to_string();
            if q.is_empty() {
                send_chunks(&bot, msg.chat.id, "질문을 적어 주세요. 예: /ask 안녕").await?;
            } else {
                run_ask(&bot, &msg, &app, &q).await?;
            }
        }
        Command::Obsidian(q) => {
            let q = q.trim().to_string();
            if q.is_empty() {
                send_chunks(
                    &bot,
                    msg.chat.id,
                    "검색어를 적어 주세요. 예: /obsidian 키워드",
                )
                .await?;
            } else {
                send_chunks(&bot, msg.chat.id, START_MSG).await?;
                let text = obsidian::search_text(&app.cfg, &q)
                    .unwrap_or_else(|e| format!("검색 실패: {e}"));
                send_chunks(&bot, msg.chat.id, &text).await?;
            }
        }
        Command::Status => {
            send_chunks(&bot, msg.chat.id, START_MSG).await?;
            let text = status_text().unwrap_or_else(|e| format!("상태 조회 실패: {e}"));
            send_chunks(&bot, msg.chat.id, &text).await?;
        }
        Command::Cancel => {
            let run = app.active.lock().await.get(&msg.chat.id).cloned();
            let text = if let Some(run) = run {
                reject_pending_for_chat(&app, msg.chat.id).await;
                if run.cancel("Telegram /cancel") {
                    "현재 실행에 취소를 요청했습니다."
                } else {
                    "이미 취소 중이거나 종료된 실행입니다."
                }
            } else {
                "현재 이 대화에서 실행 중인 작업이 없습니다."
            };
            send_chunks(&bot, msg.chat.id, text).await?;
        }
        Command::Report => {
            send_chunks(&bot, msg.chat.id, START_MSG).await?;
            let text = report_text().unwrap_or_else(|e| format!("리포트 조회 실패: {e}"));
            send_chunks(&bot, msg.chat.id, &text).await?;
        }
        Command::Lesson(text) => {
            let text = text.trim().to_string();
            if text.is_empty() {
                send_chunks(
                    &bot,
                    msg.chat.id,
                    "교훈 문장을 적어 주세요. 예: /lesson 수정 전 read_file",
                )
                .await?;
            } else {
                send_chunks(&bot, msg.chat.id, START_MSG).await?;
                let out = lessons::add_text(&app.cfg, &text)
                    .unwrap_or_else(|e| format!("교훈 저장 실패: {e}"));
                send_chunks(&bot, msg.chat.id, &out).await?;
            }
        }
    }
    Ok(())
}

async fn on_text(bot: Bot, msg: Message, app: Arc<App>) -> ResponseResult<()> {
    if !msg_allowed(&msg, &app) {
        return Ok(());
    }
    let Some(text) = msg.text() else {
        return Ok(());
    };
    if let Some(rest) = text.strip_prefix("/ulw-resume") {
        let id = rest.trim();
        let id = if id.is_empty() { None } else { Some(id.to_string()) };
        spawn_ulw(bot.clone(), msg.chat.id, app.clone(), None, id);
        return Ok(());
    }
    if let Some(rest) = text.strip_prefix("/ulw") {
        let goal = rest.trim();
        if goal.is_empty() {
            send_chunks(&bot, msg.chat.id, "/ulw <목표> — 증거가 모일 때까지 자율 실행. /ulw-resume [id] 로 재개.").await?;
            return Ok(());
        }
        spawn_ulw(bot.clone(), msg.chat.id, app.clone(), Some(goal.to_string()), None);
        return Ok(());
    }
    if text.starts_with("/quota") {
        let lines = crate::usage::quota_lines(&app.cfg);
        send_chunks(&bot, msg.chat.id, &lines.join("\n")).await?;
        return Ok(());
    }
    if text.starts_with('/') {
        send_chunks(&bot, msg.chat.id, &help_text()).await?;
        return Ok(());
    }
    run_ask(&bot, &msg, &app, text).await
}

/// ulw 루프를 백그라운드 태스크로 실행한다 (F4b) — 데몬이 다른 메시지에 응답할 수 있게.
/// 승인은 인라인 버튼(LocalAsk 어댑터), 완료·중단 알림은 ulw_finish 의 notify_owner 가 담당.
fn spawn_ulw(bot: Bot, chat_id: ChatId, app: Arc<App>, goal: Option<String>, resume_id: Option<String>) {
    tokio::spawn(async move {
        let cfg = app.cfg.clone();
        if !cfg.file.telegram.allow_agent {
            let _ = send_chunks(
                &bot,
                chat_id,
                "ulw 는 파일 편집·명령 실행이 필요합니다. config [telegram] allow_agent=true 일 때만 원격 실행됩니다.",
            )
            .await;
            return;
        }
        let mut session = match crate::chat::open_session(cfg.clone(), false, None, None, None, None, true) {
            Ok(s) => s,
            Err(e) => {
                let _ = send_chunks(&bot, chat_id, &format!("ulw 세션 시작 실패: {e}")).await;
                return;
            }
        };
        let local_ask = make_local_ask(bot.clone(), chat_id, &app);
        let _ = send_chunks(&bot, chat_id, "[ulw] 루프를 시작합니다. 승인 요청은 버튼으로 옵니다.").await;
        let result = match (goal, resume_id) {
            (Some(goal), _) => match crate::ulw::UlwState::start(&cfg.workspace, &goal) {
                Ok(state) => {
                    crate::chat::ulw_loop_observed(&mut session, &goal, state, None, Some(local_ask)).await
                }
                Err(e) => Err(e),
            },
            (None, id) => {
                crate::chat::ulw_resume_observed(&mut session, id, None, Some(local_ask)).await
            }
        };
        if let Err(e) = result {
            let _ = send_chunks(&bot, chat_id, &format!("[ulw] 오류: {e}")).await;
        }
    });
}

async fn on_callback(bot: Bot, q: CallbackQuery, app: Arc<App>) -> ResponseResult<()> {
    let uid = i64::try_from(q.from.id.0).unwrap_or(q.from.id.0 as i64);
    if !user_allowed(uid, &app.cfg.file.telegram.allowed_user_ids) {
        return Ok(());
    }
    let Some(data) = q.data.as_deref() else {
        bot.answer_callback_query(q.id).await?;
        return Ok(());
    };
    let (ok, id) = if let Some(rest) = data.strip_prefix("ok:") {
        (true, rest)
    } else if let Some(rest) = data.strip_prefix("no:") {
        (false, rest)
    } else {
        bot.answer_callback_query(q.id).await?;
        return Ok(());
    };
    if let Some((_, tx)) = app.pending.lock().await.remove(id) {
        let _ = tx.send(ok);
    }
    bot.answer_callback_query(q.id).await?;
    Ok(())
}

async fn run_ask(bot: &Bot, msg: &Message, app: &Arc<App>, prompt: &str) -> ResponseResult<()> {
    send_chunks(bot, msg.chat.id, START_MSG).await?;
    let text = match ask_pipeline(bot, msg, app, prompt).await {
        Ok(t) => t,
        Err(e) => format!("오류: {e}"),
    };
    send_chunks(bot, msg.chat.id, &text).await
}

async fn ask_pipeline(bot: &Bot, msg: &Message, app: &Arc<App>, prompt: &str) -> Result<String> {
    let cfg = &app.cfg;
    let class = harness::classify_gated(cfg, prompt, false, None).await?.class;
    let mut binding = harness::bind(cfg, class, None, None)?;
    if let Some(w) = harness::apply_engine_pin(cfg, &mut binding, None, None) {
        crate::ui::live_warn(&w);
    }
    if !cfg.file.telegram.allow_agent {
        strip_remote_tools(&mut binding);
    }
    let remote = if cfg.file.telegram.allow_agent {
        Some(make_remote(bot.clone(), msg.chat.id, app))
    } else {
        None
    };

    let run_id = {
        let db = Db::open(&Db::db_path()?)?;
        db.start_run(
            "telegram",
            prompt,
            Some(binding.class.as_str()),
            Some(&binding.profile_name),
            Some(&binding.provider_name),
            Some(&binding.model),
        )?
    };
    let _g = graph::scope(&run_id);
    let run_context = RunContext::for_config(RunId::new(run_id.clone()), Arc::new(cfg.clone()))
        .with_live_sink(crate::ui::current_live_sink());
    app.active
        .lock()
        .await
        .insert(msg.chat.id, run_context.clone());
    graph::trace_start_in(
        &run_context,
        binding.class.as_str(),
        &binding.profile_name,
        &binding.provider_name,
        &binding.model,
        false,
    );

    let result = harness::run_pipeline_with_context(
        cfg,
        &binding,
        prompt,
        false,
        None,
        None,
        remote,
        None,
        run_context.clone(),
    )
    .await;
    app.active.lock().await.remove(&msg.chat.id);
    match result {
        Ok(outcome) => {
            if let Ok(db) = Db::open(&Db::db_path()?) {
                let _ = agent::record_finish(&db, &run_id, &outcome);
            }
            graph::node_in(&run_context, "persist", &outcome.status, "", Some("bind"));
            lessons::maybe_spawn(cfg, prompt, &outcome);
            let mut text = agent::assistant_text(&outcome.messages);
            if text.trim().is_empty() {
                text = format!("상태: {}", outcome.status);
            }
            Ok(text)
        }
        Err(e) => {
            if let Ok(db) = Db::open(&Db::db_path()?) {
                let _ = db.finish_run(&run_id, "fail", 0, 0, 0, Some(&e.to_string()));
            }
            graph::node_in(
                &run_context,
                "persist",
                "fail",
                &e.to_string(),
                Some("bind"),
            );
            let fail = agent::AgentOutcome {
                status: "fail".into(),
                error: Some(e.to_string()),
                ..Default::default()
            };
            lessons::maybe_spawn(cfg, prompt, &fail);
            Err(e)
        }
    }
}

fn strip_remote_tools(binding: &mut Binding) {
    binding.tools.clear();
    binding.plan_first = false;
    binding.verify = false;
}

fn make_remote(bot: Bot, chat_id: ChatId, app: &Arc<App>) -> RemoteApproval {
    let pending = app.pending.clone();
    let app = app.clone();
    let timeout = Duration::from_secs(app.cfg.file.telegram.approval_timeout_secs.max(1));
    RemoteApproval {
        timeout,
        ask: Arc::new(move |preview: String| {
            let bot = bot.clone();
            let pending = pending.clone();
            let app = app.clone();
            Box::pin(async move {
                let id = next_approval_id(&app);
                let (tx, rx) = oneshot::channel();
                pending.lock().await.insert(id.clone(), (chat_id, tx));
                let kb = InlineKeyboardMarkup::new([[
                    InlineKeyboardButton::callback("✅ 승인", format!("ok:{id}")),
                    InlineKeyboardButton::callback("❌ 거부", format!("no:{id}")),
                ]]);
                let body = truncate_preview(&preview);
                if bot
                    .send_message(chat_id, body)
                    .reply_markup(kb)
                    .await
                    .is_err()
                {
                    pending.lock().await.remove(&id);
                    return false;
                }
                rx.await.unwrap_or(false)
            })
        }),
    }
}

/// ulw 루프용 로컬 승인 어댑터 (F4b) — 인라인 버튼 승인을 LocalAsk 형태로 제공한다.
/// 원격에서는 Always 가 없다 (자동 승인 금지 규칙 — effective_yes 강제 차단과 같은 철학).
fn make_local_ask(bot: Bot, chat_id: ChatId, app: &Arc<App>) -> crate::agent::LocalAsk {
    let pending = app.pending.clone();
    let app = app.clone();
    std::sync::Arc::new(move |preview: String| {
        let bot = bot.clone();
        let pending = pending.clone();
        let app = app.clone();
        Box::pin(async move {
            let id = next_approval_id(&app);
            let (tx, rx) = oneshot::channel();
            pending.lock().await.insert(id.clone(), (chat_id, tx));
            let kb = InlineKeyboardMarkup::new([[
                InlineKeyboardButton::callback("✅ 승인", format!("ok:{id}")),
                InlineKeyboardButton::callback("❌ 거부", format!("no:{id}")),
            ]]);
            let body = truncate_preview(&preview);
            if bot
                .send_message(chat_id, body)
                .reply_markup(kb)
                .await
                .is_err()
            {
                pending.lock().await.remove(&id);
                return crate::agent::ApprovalChoice::No;
            }
            match rx.await {
                Ok(true) => crate::agent::ApprovalChoice::Yes,
                _ => crate::agent::ApprovalChoice::No,
            }
        })
    })
}

async fn reject_pending_for_chat(app: &App, chat_id: ChatId) {
    let mut pending = app.pending.lock().await;
    let ids: Vec<String> = pending
        .iter()
        .filter(|(_, (pending_chat, _))| *pending_chat == chat_id)
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        if let Some((_, sender)) = pending.remove(&id) {
            let _ = sender.send(false);
        }
    }
}

fn next_approval_id(app: &App) -> String {
    let n = app.seq.fetch_add(1, Ordering::Relaxed);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms:x}{n:x}")
}

fn truncate_preview(s: &str) -> String {
    let n = s.chars().count();
    if n <= 3500 {
        return s.to_string();
    }
    let mut t: String = s.chars().take(3500).collect();
    t.push_str("\n…");
    t
}

fn status_text() -> Result<String> {
    let db = Db::open(&Db::db_path()?)?;
    let runs = db.recent_runs(5)?;
    let (tin, tout) = db.tokens_since(local_midnight_unix())?;
    let mut s = String::from("최근 실행 5건:\n");
    if runs.is_empty() {
        s.push_str("(없음)\n");
    } else {
        for r in &runs {
            s.push_str(&format!(
                "- {} {} {} in={} out={}\n",
                r.status,
                r.class.as_deref().unwrap_or("-"),
                r.subagent.as_deref().unwrap_or("-"),
                r.input_tokens,
                r.output_tokens
            ));
        }
    }
    s.push_str(&format!("오늘 토큰 in={tin} out={tout}"));
    Ok(s)
}

fn report_text() -> Result<String> {
    let db = Db::open(&Db::db_path()?)?;
    let Some(row) = db.last_report()? else {
        return Ok("저장된 리포트가 없습니다. 먼저 inspect 를 실행하세요.".into());
    };
    Ok(format!("{}\n\n전문: {}", row.summary, row.body_path))
}

fn help_text() -> String {
    "RafikX 명령:\n/ask 질문\n/cancel\n/obsidian 검색어\n/status\n/report\n/lesson 교훈 문장\n일반 글은 /ask 와 같습니다."
        .into()
}

fn msg_allowed(msg: &Message, app: &App) -> bool {
    let Some(user) = msg.from.as_ref() else {
        return false;
    };
    let uid = i64::try_from(user.id.0).unwrap_or(user.id.0 as i64);
    user_allowed(uid, &app.cfg.file.telegram.allowed_user_ids)
}

pub fn user_allowed(user_id: i64, allowed: &[i64]) -> bool {
    allowed.contains(&user_id)
}

pub fn split_telegram_text(s: &str) -> Vec<String> {
    if s.is_empty() {
        return vec!["(빈 응답)".into()];
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut n = 0usize;
    for ch in s.chars() {
        if n >= TELEGRAM_MAX {
            out.push(std::mem::take(&mut buf));
            n = 0;
        }
        buf.push(ch);
        n += 1;
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

async fn send_chunks(bot: &Bot, chat: ChatId, text: &str) -> ResponseResult<()> {
    for chunk in split_telegram_text(text) {
        if chunk.trim().is_empty() {
            continue;
        }
        bot.send_message(chat, chunk).await?;
    }
    Ok(())
}

fn local_midnight_unix() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let offset = local_utc_offset_secs();
    let local = now + offset;
    let midnight_local = local - local.rem_euclid(86_400);
    midnight_local - offset
}

fn local_utc_offset_secs() -> i64 {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct SystemTime {
            year: u16,
            month: u16,
            day_of_week: u16,
            day: u16,
            hour: u16,
            minute: u16,
            second: u16,
            millis: u16,
        }
        #[repr(C)]
        struct FileTime {
            lo: u32,
            hi: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetSystemTime(lp: *mut SystemTime);
            fn GetLocalTime(lp: *mut SystemTime);
            fn SystemTimeToFileTime(lp: *const SystemTime, ft: *mut FileTime) -> i32;
        }
        unsafe {
            let mut utc = std::mem::zeroed();
            let mut local = std::mem::zeroed();
            GetSystemTime(&mut utc);
            GetLocalTime(&mut local);
            let mut utc_ft = FileTime { lo: 0, hi: 0 };
            let mut local_ft = FileTime { lo: 0, hi: 0 };
            if SystemTimeToFileTime(&utc, &mut utc_ft) == 0
                || SystemTimeToFileTime(&local, &mut local_ft) == 0
            {
                return 0;
            }
            let to_i64 = |ft: FileTime| ((ft.hi as i64) << 32) | ft.lo as i64;
            (to_i64(local_ft) - to_i64(utc_ft)) / 10_000_000
        }
    }
    #[cfg(unix)]
    {
        #[repr(C)]
        struct Tm {
            tm_sec: i32,
            tm_min: i32,
            tm_hour: i32,
            tm_mday: i32,
            tm_mon: i32,
            tm_year: i32,
            tm_wday: i32,
            tm_yday: i32,
            tm_isdst: i32,
            tm_gmtoff: i64,
            tm_zone: *const i8,
        }
        unsafe extern "C" {
            fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        unsafe {
            let mut tm: Tm = std::mem::zeroed();
            if localtime_r(&now, &mut tm).is_null() {
                0
            } else {
                tm.tm_gmtoff
            }
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_at_4096() {
        let s: String = "한".repeat(5000);
        let parts = split_telegram_text(&s);
        assert!(parts.iter().all(|p| p.chars().count() <= 4096));
        assert_eq!(parts.concat().chars().count(), 5000);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].chars().count(), 4096);
        assert_eq!(parts[1].chars().count(), 904);
    }

    #[test]
    fn split_empty_placeholder() {
        assert_eq!(split_telegram_text(""), vec!["(빈 응답)".to_string()]);
        let short = split_telegram_text("안녕");
        assert_eq!(short, vec!["안녕".to_string()]);
    }

    #[test]
    fn whitelist_silence() {
        assert!(user_allowed(1, &[1, 2]));
        assert!(!user_allowed(9, &[1, 2]));
        assert!(!user_allowed(1, &[]));
    }

    #[test]
    fn callback_ids_fit_64_bytes() {
        let id = format!("{:x}{:x}", 1_700_000_000_000u64, 99u32);
        assert!(format!("ok:{id}").len() <= 64);
        assert!(format!("no:{id}").len() <= 64);
    }
}

/// ulw 루프 종료·중단 알림 (F4) — 허용된 첫 번째 사용자에게 알림을 전달한다.
/// 알림 실패는 루프를 막지 않는다 (부가 기능이므로 조용히 무시).
pub async fn notify_owner(cfg: &Config, text: &str) {
    let Ok(Some(token)) = crate::auth::telegram_token(cfg) else {
        return;
    };
    let Some(chat_id) = cfg.file.telegram.allowed_user_ids.first().copied() else {
        return;
    };
    let bot = Bot::new(token);
    for chunk in split_telegram_text(text) {
        let _ = bot.send_message(ChatId(chat_id), chunk).await;
    }
}
