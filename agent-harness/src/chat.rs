use std::io::{self, Write};

use anyhow::{Result, anyhow};

use crate::agent::{self, LocalAsk};
use crate::applog;
use crate::config::Config;
use crate::db::Db;
use crate::harness::{bind, classify, print_binding, run_pipeline};
use crate::obsidian;
use crate::provider::{ContentBlock, Message, Role};

#[derive(Clone)]
pub struct Session {
    pub cfg: Config,
    pub yes: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub class: Option<String>,
    pub session_id: Option<String>,
    pub messages: Vec<Message>,
    pub obsidian_on: bool,
    pub dirty: bool,
}

pub async fn cmd_chat(
    cfg: Config,
    yes: bool,
    provider: Option<String>,
    model: Option<String>,
    class: Option<String>,
    list: bool,
    resume: Option<String>,
) -> Result<()> {
    let db = Db::open(&Db::db_path()?)?;
    if list {
        return list_sessions(&db);
    }

    let mut session = open_session(cfg, yes, provider, model, class, resume, true)?;

    if session.session_id.is_none() {
        crate::ui::banner("대화");
        crate::ui::note("/help  명령 보기   ·   /quit  종료");
        crate::ui::print_footer();
    }

    loop {
        print!("{} ", crate::ui::gold("rafikx ›"));
        io::stdout().flush()?;
        let mut line = String::new();
        let n = io::stdin().read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('/') {
            match handle_slash(&mut session, line, true)? {
                Slash::Continue(notes) => {
                    for n in notes {
                        println!("{n}");
                    }
                }
                Slash::Quit => break,
                Slash::Agent(task) => {
                    run_turn(&mut session, &task, Some("dev"), false, None).await?;
                }
            }
            continue;
        }

        let class = session.class.clone();
        let obsidian_on = session.obsidian_on;
        run_turn(&mut session, line, class.as_deref(), obsidian_on, None).await?;
    }

    if let Some(id) = save_if_dirty(&mut session)? {
        println!("세션을 저장했습니다: {id}");
    }
    Ok(())
}

pub fn open_session(
    cfg: Config,
    yes: bool,
    provider: Option<String>,
    model: Option<String>,
    class: Option<String>,
    resume: Option<String>,
    announce: bool,
) -> Result<Session> {
    let db = Db::open(&Db::db_path()?)?;
    if let Some(id) = resume {
        let Some(row) = db.load_session(&id)? else {
            return Err(anyhow!("세션 '{id}' 를 찾지 못했습니다"));
        };
        let mut messages: Vec<Message> = serde_json::from_str(&row.messages_json)
            .map_err(|_| anyhow!("세션 메시지를 읽지 못했습니다"))?;
        agent::sanitize_tool_pairs(&mut messages);
        if announce {
            println!(
                "세션 재개: {}  ({})",
                row.id,
                row.title.unwrap_or_else(|| "(제목 없음)".into())
            );
        }
        return Ok(Session {
            cfg,
            yes,
            provider,
            model,
            class,
            session_id: Some(row.id),
            messages,
            obsidian_on: false,
            dirty: false,
        });
    }
    Ok(Session {
        cfg,
        yes,
        provider,
        model,
        class,
        session_id: None,
        messages: Vec::new(),
        obsidian_on: false,
        dirty: false,
    })
}

pub fn save_if_dirty(session: &mut Session) -> Result<Option<String>> {
    if !session.dirty {
        return Ok(None);
    }
    let db = Db::open(&Db::db_path()?)?;
    let id = persist(
        &db,
        session.session_id.as_deref(),
        &mut session.messages,
    )?;
    session.session_id = Some(id.clone());
    session.dirty = false;
    Ok(Some(id))
}

pub enum Slash {
    Continue(Vec<String>),
    Quit,
    Agent(String),
}

pub fn handle_slash(session: &mut Session, line: &str, read_stdin: bool) -> Result<Slash> {
    let db = Db::open(&Db::db_path()?)?;
    let mut parts = line.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    match cmd {
        "/quit" | "/exit" => Ok(Slash::Quit),
        "/help" => Ok(Slash::Continue(vec![help_text()])),
        "/save" => {
            let id = persist(
                &db,
                session.session_id.as_deref(),
                &mut session.messages,
            )?;
            session.session_id = Some(id.clone());
            session.dirty = false;
            Ok(Slash::Continue(vec![format!("저장됨: {id}")]))
        }
        "/clear" => {
            session.messages.clear();
            session.dirty = true;
            Ok(Slash::Continue(vec!["대화 맥락을 지웠습니다.".into()]))
        }
        "/model" => {
            let regs = crate::auth::registered_models(&session.cfg);
            if regs.is_empty() {
                return Ok(Slash::Continue(vec![
                    "등록된 모델이 없습니다. rafikx settings 에서 연결하세요.".into(),
                ]));
            }
            if rest.is_empty() {
                let mut notes = vec!["등록된 모델:".into()];
                for (i, r) in regs.iter().enumerate() {
                    notes.push(format!("  [{}] {} / {}", i + 1, r.provider, r.id));
                }
                notes.push("예: /model 2".into());
                if read_stdin {
                    print!("번호> ");
                    io::stdout().flush()?;
                    let mut line = String::new();
                    io::stdin().read_line(&mut line)?;
                    notes.push(apply_model_choice(&mut session.model, &regs, line.trim()));
                }
                Ok(Slash::Continue(notes))
            } else {
                Ok(Slash::Continue(vec![apply_model_choice(
                    &mut session.model,
                    &regs,
                    rest,
                )]))
            }
        }
        "/provider" => {
            let names = crate::auth::usable_names(&session.cfg);
            if names.is_empty() {
                return Ok(Slash::Continue(vec![
                    "연결된 서비스가 없습니다. rafikx settings 에서 연결하세요.".into(),
                ]));
            }
            if rest.is_empty() {
                let mut notes = vec!["연결된 서비스:".into()];
                for (i, n) in names.iter().enumerate() {
                    notes.push(format!("  [{}] {}", i + 1, crate::auth::provider_label(n)));
                }
                notes.push("예: /provider 1   (0=기본으로)".into());
                if read_stdin {
                    print!("번호> ");
                    io::stdout().flush()?;
                    let mut line = String::new();
                    io::stdin().read_line(&mut line)?;
                    notes.push(apply_provider_choice(
                        &mut session.provider,
                        &names,
                        line.trim(),
                    ));
                }
                Ok(Slash::Continue(notes))
            } else {
                Ok(Slash::Continue(vec![apply_provider_choice(
                    &mut session.provider,
                    &names,
                    rest,
                )]))
            }
        }
        "/class" => {
            if rest.is_empty() {
                session.class = None;
                Ok(Slash::Continue(vec!["분류 강제를 해제했습니다.".into()]))
            } else {
                session.class = Some(rest.to_string());
                Ok(Slash::Continue(vec![format!("분류 강제: {rest}")]))
            }
        }
        "/obsidian" => match rest {
            "on" => {
                session.obsidian_on = true;
                Ok(Slash::Continue(vec!["Obsidian 컨텍스트: 켜짐".into()]))
            }
            "off" => {
                session.obsidian_on = false;
                Ok(Slash::Continue(vec!["Obsidian 컨텍스트: 꺼짐".into()]))
            }
            _ => Ok(Slash::Continue(vec![
                "/obsidian on  또는  /obsidian off".into(),
            ])),
        },
        "/agent" => {
            if rest.is_empty() {
                Ok(Slash::Continue(vec!["/agent <지시> 형식으로 쓰세요.".into()]))
            } else {
                Ok(Slash::Agent(rest.to_string()))
            }
        }
        "/connect" | "/login" => connect_slash(&session.cfg, rest, read_stdin),
        _ => Ok(Slash::Continue(vec![
            "알 수 없는 명령입니다. /help".into(),
        ])),
    }
}

pub(crate) fn help_text() -> String {
    "/save  /quit  /clear  /model  /provider  /connect  /class <simple|medium|advanced|dev>\n\
     /obsidian on|off  /agent <지시>\n\
     /connect 에서 키 칸이 열립니다. Ctrl+V 붙여넣기. /connect zen\n\
     Enter 전송 · Ctrl+J 또는 Shift+Enter 줄바꿈 · ? 키 도움말"
        .into()
}

fn connect_slash(cfg: &crate::config::Config, rest: &str, read_stdin: bool) -> Result<Slash> {
    if rest.is_empty() {
        let mut notes = vec![
            "연결: /connect zen  또는  /connect go. 키는 https://opencode.ai/auth".into(),
            format!("설정 파일  {}", cfg.path.display()),
        ];
        for n in crate::auth::menu_provider_names(cfg) {
            let mark = if crate::auth::is_connected(cfg, &n) {
                "연결됨"
            } else {
                "미연결"
            };
            notes.push(format!(
                "  {}  [{mark}]  {}",
                crate::auth::provider_label(&n),
                crate::auth::env_hint(cfg, &n)
            ));
        }
        return Ok(Slash::Continue(notes));
    }
    let alias = crate::auth::resolve_provider_alias(rest).unwrap_or_else(|| rest.to_string());
    if !cfg.file.providers.contains_key(&alias) {
        return Ok(Slash::Continue(vec![format!("'{rest}' 서비스가 config에 없습니다.")]));
    }
    if !read_stdin {
        return Ok(Slash::Continue(vec![format!(
            "TUI에서 /connect {alias} 로 키를 붙이거나, 환경변수 {} 를 넣으세요.",
            crate::auth::env_hint(cfg, &alias)
        )]));
    }
    let p = cfg.provider(&alias)?;
    if crate::auth::auth_mode(&alias, p) == "none" {
        return Ok(Slash::Continue(vec!["로컬은 키가 필요 없습니다.".into()]));
    }
    println!("키는 secrets.toml 에만 저장됩니다. {}", crate::auth::env_hint(cfg, &alias));
    print!("{} 키: ", alias);
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    match crate::auth::save_pasted_key(&alias, line.trim()) {
        Ok(_) => Ok(Slash::Continue(vec![format!(
            "{} 연결됨",
            crate::auth::provider_label(&alias)
        )])),
        Err(e) => Ok(Slash::Continue(vec![format!("연결 실패: {e:#}")])),
    }
}

fn apply_model_choice(
    model: &mut Option<String>,
    regs: &[crate::auth::RegisteredModel],
    rest: &str,
) -> String {
    if rest.is_empty() {
        return format!("현재 모델: {}", model.as_deref().unwrap_or("(하네스 자동)"));
    }
    if let Some(nums) = crate::menu::parse_numbers(rest, regs.len(), false, true) {
        if nums.first() == Some(&0) {
            *model = None;
            return "모델을 하네스 자동으로 돌렸습니다.".into();
        }
        if let Some(i) = nums.first() {
            if let Some(r) = regs.get(i - 1) {
                *model = Some(r.id.clone());
                return format!("모델: {} / {}", r.provider, r.id);
            }
        }
    }
    *model = Some(rest.to_string());
    format!("모델: {rest}")
}

fn apply_provider_choice(provider: &mut Option<String>, names: &[String], rest: &str) -> String {
    if rest.is_empty() {
        return format!(
            "현재 프로바이더: {}",
            provider.as_deref().unwrap_or("(config 기본)")
        );
    }
    if let Some(nums) = crate::menu::parse_numbers(rest, names.len(), false, true) {
        if nums.first() == Some(&0) {
            *provider = None;
            return "프로바이더를 기본으로 돌렸습니다.".into();
        }
        if let Some(i) = nums.first() {
            if let Some(n) = names.get(i - 1) {
                *provider = Some(n.clone());
                return format!("프로바이더: {n}");
            }
        }
    }
    let alias = crate::auth::resolve_provider_alias(rest).unwrap_or_else(|| rest.to_string());
    if names.iter().any(|n| n == &alias) {
        *provider = Some(alias.clone());
        return format!("프로바이더: {alias}");
    }
    let labels: Vec<String> = names.iter().map(|n| crate::auth::provider_label(n)).collect();
    let hits = crate::menu::match_items(rest, &labels);
    if hits.len() == 1 {
        if let Some(n) = names.get(hits[0] - 1) {
            *provider = Some(n.clone());
            return format!("프로바이더: {n}");
        }
    }
    *provider = Some(rest.to_string());
    format!("프로바이더: {rest}")
}

pub async fn run_turn(
    session: &mut Session,
    prompt: &str,
    forced_class: Option<&str>,
    obsidian_on: bool,
    local_ask: Option<LocalAsk>,
) -> Result<TurnInfo> {
    let class = classify(&session.cfg, prompt, obsidian_on, forced_class).await?;
    let binding = bind(
        &session.cfg,
        class,
        session.provider.as_deref(),
        session.model.as_deref(),
    )?;
    print_binding(&binding);

    let mut task = prompt.to_string();
    if obsidian_on {
        if !session.cfg.file.obsidian.enabled {
            crate::ui::live_line("[Obsidian] 꺼져 있습니다. rafikx settings 에서 켜세요.");
        } else {
            match obsidian::ask_context(&session.cfg, prompt) {
                Ok(ctx) => {
                    if ctx.sources.is_empty() {
                        crate::ui::live_line("[Obsidian] 검색 결과 없음");
                    } else {
                        crate::ui::live_line("[Obsidian] 출처:");
                        for s in &ctx.sources {
                            crate::ui::live_line(&format!("  - {s}"));
                        }
                    }
                    task = format!("{}\n\n(질문)\n{prompt}", ctx.block);
                }
                Err(e) => crate::ui::live_warn(&format!("Obsidian 컨텍스트를 넣지 못했습니다: {e}")),
            }
        }
    }

    let db = Db::open(&Db::db_path()?)?;
    let run_id = db.start_run(
        "chat",
        prompt,
        Some(binding.class.as_str()),
        Some(&binding.profile_name),
        Some(&binding.provider_name),
        Some(&binding.model),
    )?;
    let _g = crate::graph::scope(&run_id);
    crate::graph::trace_start(
        binding.class.as_str(),
        &binding.profile_name,
        &binding.provider_name,
        &binding.model,
        obsidian_on,
    );
    applog::info(&format!(
        "chat class={} profile={} provider={} model={}",
        binding.class.as_str(),
        binding.profile_name,
        binding.provider_name,
        binding.model
    ));

    let mut resume = session.messages.clone();
    resume.push(Message::user_text(&task));
    agent::sanitize_tool_pairs(&mut resume);

    let info_label = format!(
        "{} → {}  ·  {}",
        binding.class.as_str(),
        binding.profile_name,
        binding.model
    );

    match run_pipeline(
        &session.cfg,
        &binding,
        &task,
        session.yes,
        session.provider.as_deref(),
        Some(resume),
        None,
        local_ask,
    )
    .await
    {
        Ok(outcome) => {
            agent::record_finish(&db, &run_id, &outcome)?;
            crate::graph::node("persist", &outcome.status, "", Some("bind"));
            crate::ui::live_status(&format!(
                "[run] class={} profile={} status={} iter={} tokens in={} out={}",
                binding.class.as_str(),
                binding.profile_name,
                outcome.status,
                outcome.iterations,
                outcome.input_tokens,
                outcome.output_tokens
            ));
            if !outcome.messages.is_empty() {
                session.messages = outcome.messages.clone();
            } else {
                session.messages.push(Message::user_text(prompt));
            }
            agent::sanitize_tool_pairs(&mut session.messages);
            session.dirty = true;
            crate::lessons::maybe_spawn(&session.cfg, prompt, &outcome);
            crate::ui::print_footer();
            Ok(TurnInfo {
                run_id,
                label: info_label,
                status: outcome.status,
                tokens_in: outcome.input_tokens,
                tokens_out: outcome.output_tokens,
            })
        }
        Err(e) => {
            let _ = db.finish_run(&run_id, "fail", 0, 0, 0, Some(&e.to_string()));
            crate::graph::node("persist", "fail", &e.to_string(), Some("bind"));
            let fail = agent::AgentOutcome {
                status: "fail".into(),
                iterations: 0,
                input_tokens: 0,
                output_tokens: 0,
                error: Some(e.to_string()),
                messages: vec![],
                changed_files: vec![],
                tool_errors: vec![],
                deny_reasons: vec![],
                verify_fail: None,
            };
            crate::lessons::maybe_spawn(&session.cfg, prompt, &fail);
            crate::ui::print_footer();
            Err(e)
        }
    }
}

pub struct TurnInfo {
    pub run_id: String,
    pub label: String,
    pub status: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
}

fn persist(db: &Db, id: Option<&str>, messages: &mut [Message]) -> Result<String> {
    let mut owned = messages.to_vec();
    agent::sanitize_tool_pairs(&mut owned);
    let json = serde_json::to_string(&owned)?;
    let title = session_title(&owned);
    db.save_session(id, &title, &json)
}

fn session_title(messages: &[Message]) -> String {
    for m in messages {
        if m.role != Role::User {
            continue;
        }
        for b in &m.content {
            if let ContentBlock::Text { text } = b {
                let t: String = text.chars().take(40).collect();
                if !t.trim().is_empty() {
                    return t;
                }
            }
        }
    }
    "대화".into()
}

fn list_sessions(db: &Db) -> Result<()> {
    let rows = db.list_sessions(50)?;
    if rows.is_empty() {
        println!("저장된 세션이 없습니다.");
        return Ok(());
    }
    println!("id                         title");
    for r in rows {
        println!(
            "{:<26} {}",
            r.id,
            r.title.unwrap_or_else(|| "(제목 없음)".into())
        );
    }
    Ok(())
}
