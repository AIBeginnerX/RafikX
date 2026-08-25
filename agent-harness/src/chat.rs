use std::io::{self, Write};

use anyhow::{Result, anyhow};

use crate::agent::{self, LocalAsk};
use crate::applog;
use crate::config::Config;
use crate::db::Db;
use crate::harness::{bind, classify, print_binding, run_pipeline};
use crate::obsidian;
use crate::provider::{ChatRequest, ContentBlock, Message, Role};

/// plan 모드에서 허용되는 읽기 전용 도구만 남긴다.
const MAX_ATTACH_CHARS: usize = 80_000;

#[derive(Clone, Debug, Default)]
pub struct CompletionSummary {
    pub changed_files: Vec<String>,
    pub iterations: u32,
    pub completed_todos: usize,
    pub total_todos: usize,
    pub tool_errors: usize,
    pub improvement: String,
}

impl CompletionSummary {
    fn from_outcome(outcome: &agent::AgentOutcome, model: &str) -> Self {
        let todos = crate::tools_more::current_todos();
        let progress = crate::tools_more::todo_progress(&todos);
        let improvement = if !outcome.tool_errors.is_empty() {
            "실패한 도구의 전제조건을 교훈으로 저장하고 다음 실행에서는 확인 후 호출합니다.".into()
        } else if outcome.iterations >= 8 {
            "긴 작업을 더 작은 Todo로 나누고 독립 조사만 저비용 서브에이전트에 배치합니다.".into()
        } else if crate::ranks::normalize_id(model) == "minimax-m3" {
            "MiniMax-M3의 긴 컨텍스트를 세션 안에서 재사용하고 독립 작업만 위임해 토큰 중복을 줄입니다.".into()
        } else if outcome.changed_files.is_empty() {
            "도구로 확인한 근거와 추론을 분리해 다음 답변의 검증 가능성을 높입니다.".into()
        } else {
            "변경 파일별 최소 검증을 유지하고 반복 수정 패턴만 재사용 교훈으로 저장합니다.".into()
        };
        Self {
            changed_files: outcome.changed_files.clone(),
            iterations: outcome.iterations,
            completed_todos: progress.completed,
            total_todos: progress.total,
            tool_errors: outcome.tool_errors.len(),
            improvement,
        }
    }
}

#[derive(Clone)]
pub struct Session {
    pub cfg: Config,
    pub yes: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub class: Option<String>,
    /// opencode 의 build/plan 모드. 기본 build.
    pub mode: String,
    pub session_id: Option<String>,
    pub messages: Vec<Message>,
    /// 직전 모델 요청이 사용한 컨텍스트 토큰. 다음 턴의 자동 압축 판단에 사용한다.
    pub last_context_tokens: u32,
    pub obsidian_on: bool,
    /// /file 로 붙인 첨부 (경로, 내용) — 다음 턴의 컨텍스트로 주입된 뒤 비운다.
    pub attachments: Vec<(String, String)>,
    pub dirty: bool,
    /// 성공한 턴의 (provider, model) — 사용자가 직접 지정하지 않았을 때 연속성을 위해 재사용.
    pub sticky: Option<(String, String)>,
}

/// 슬래시 명령 테이블 — TUI 하단 팔레트와 도움말이 함께 쓴다.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "명령 보기"),
    ("/connect", "서비스 연결·키 등록"),
    ("/login", "연결 마법사"),
    ("/provider", "기본 연결 변경"),
    ("/model", "모델 선택"),
    ("/engine", "하네스 엔진 rafikx|deepseek|dk|pi"),
    ("/harness", "Harness strategy single|multi"),
    ("/mode", "plan(읽기전용)/build 전환"),
    ("/class", "분류 고정 simple|medium|advanced|dev"),
    ("/agent", "코딩 실행"),
    ("/file", "파일 첨부 (@경로 멘션 가능)"),
    ("/sessions", "세션 목록"),
    ("/resume", "세션 이어하기"),
    ("/find", "지난 세션 검색"),
    ("/compact", "대화 요약 압축"),
    ("/undo", "마지막 질문 되돌리기"),
    ("/tools", "도구 목록"),
    ("/todo", "작업 목록 보기"),
    ("/goal", "장기 목표 상태"),
    ("/status", "연결·사용량 요약"),
    ("/theme", "테마 변경"),
    ("/obsidian", "볼트 사용 on|off"),
    ("/new", "새 세션 시작"),
    ("/save", "세션 저장"),
    ("/clear", "대화 지우기"),
    ("/quit", "종료"),
];

impl Session {
    pub fn is_plan_mode(&self) -> bool {
        self.mode.eq_ignore_ascii_case("plan")
    }
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
        // "/" 만 입력하면 사용 가능한 명령 목록을 바로 보여준다.
        if line == "/" {
            println!("{}", crate::ui::gold("── 슬래시 명령 ──"));
            println!("{}", help_text());
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
                Slash::Compact => {
                    match compact_session(&mut session).await {
                        Ok(s) => println!("대화를 요약해 압축했습니다 ({s}자)."),
                        Err(e) => println!("압축 실패: {e:#}"),
                    };
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
    // 공식 CLI 로컬 로그인이 있으면 자동으로 가져와 연결한다 (프로세스당 1회).
    for note in crate::auth::auto_import_cli_logins(&cfg) {
        crate::ui::note(&note);
    }
    // 새 릴리스 확인 (비동기 화면용과 별개로, 일반 채팅 시작 시 짧게 확인해서 안내만).
    if announce {
        // 별도 스레드에서 확인 (TUI의 async 런타임을 막지 않는다)
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::update::latest_release());
        });
        let checked = rx.recv_timeout(std::time::Duration::from_secs(6)).ok();
        if let Some(Ok(rel)) = checked {
            if let Some(notice) = crate::update::upgrade_notice(&rel, env!("CARGO_PKG_VERSION")) {
                for line in notice.lines() {
                    crate::ui::note(&format!("⚠ {line}"));
                }
            }
        }
    }
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
        return Ok(seed_last_choice(Session {
            cfg,
            yes,
            provider,
            model,
            class,
            mode: "build".into(),
            session_id: Some(row.id),
            messages,
            last_context_tokens: 0,
            obsidian_on: false,
            attachments: Vec::new(),
            dirty: false,
            sticky: None,
        }));
    }
    Ok(seed_default_choice(Session {
        cfg,
        yes,
        provider,
        model,
        class,
        mode: "build".into(),
        session_id: None,
        messages: Vec::new(),
        last_context_tokens: 0,
        obsidian_on: false,
        attachments: Vec::new(),
        dirty: false,
        sticky: None,
    }))
}

fn default_new_session_pair(cfg: &Config) -> Option<(String, String)> {
    crate::auth::registered_models(cfg)
        .into_iter()
        .find(|model| crate::ranks::normalize_id(&model.id) == "minimax-m3")
        .map(|model| (model.provider, model.id))
}

fn seed_default_choice(mut session: Session) -> Session {
    if session.provider.is_none()
        && session.model.is_none()
        && let Some((provider, model)) = default_new_session_pair(&session.cfg)
    {
        session.provider = Some(provider);
        session.model = Some(model);
    }
    session
}

/// 재시작 후에도 이전에 선택/성공한 (provider, model)을 이어받는다.
/// 사용자가 명시적으로 지정한 경우에는 그 값을 우선한다.
fn seed_last_choice(mut s: Session) -> Session {
    if s.provider.is_none() || s.model.is_none() {
        let lp = s.cfg.file.general.last_provider.trim();
        let lm = s.cfg.file.general.last_model.trim();
        if !lp.is_empty() && !lm.is_empty() && s.cfg.file.providers.contains_key(lp) {
            let valid_pair = crate::auth::registered_models(&s.cfg)
                .iter()
                .any(|r| r.provider == lp && r.id == lm);
            let selected_model = if valid_pair {
                lm.to_string()
            } else {
                s.cfg
                    .provider(lp)
                    .map(|p| p.model.clone())
                    .unwrap_or_default()
            };
            if s.provider.is_none() {
                s.provider = Some(lp.to_string());
            }
            if s.model.is_none() && !selected_model.is_empty() {
                s.model = Some(selected_model);
            }
        }
    }
    s
}

/// 마지막 선택을 config 에 영속 저장 — 재시작 후에도 같은 모델로 실행.
pub fn persist_last_choice(cfg: &Config, provider: &str, model: &str) {
    use crate::config::{toml_string, write_toml_key};
    let _ = write_toml_key(&cfg.path, "[general]", "last_provider", &toml_string(provider));
    let _ = write_toml_key(&cfg.path, "[general]", "last_model", &toml_string(model));
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
    /// /compact — 대화 요약으로 압축 (비동기)
    Compact,
}

/// /engine 에서 선택 가능한 엔진인지 판정한다.
pub fn is_valid_engine(e: &str) -> bool {
    matches!(e, "rafikx" | "deepseek" | "dk" | "pi")
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
        "/new" => {
            session.messages.clear();
            session.attachments.clear();
            session.session_id = None;
            session.dirty = false;
            session.sticky = None;
            if let Some((provider, model)) = default_new_session_pair(&session.cfg) {
                session.provider = Some(provider);
                session.model = Some(model);
            }
            Ok(Slash::Continue(vec![
                format!(
                    "새 세션을 시작합니다. 기본 모델: {}",
                    session.model.as_deref().unwrap_or("auto")
                )
            ]))
        }
        "/clear" => {
            session.messages.clear();
            session.dirty = true;
            Ok(Slash::Continue(vec!["대화 맥락을 지웠습니다.".into()]))
        }
        "/engine" => {
            let e = rest.trim().to_ascii_lowercase();
            const ENGINES: &str = "rafikx|deepseek|dk|pi";
            if e.is_empty() {
                let cur = session.cfg.file.general.engine.clone();
                Ok(Slash::Continue(vec![format!(
                    "현재 하네스 엔진: {cur}   ·   변경: /engine {ENGINES}"
                )]))
            } else if is_valid_engine(&e) {
                let label = match e.as_str() {
                    "dk" => "dk-harness (DeepSeek DSH 호환 모드)".into(),
                    "pi" => "pi-harness (oh-my-pi 스타일)".into(),
                    other => other.to_string(),
                };
                session.cfg.file.general.engine = e.clone();
                match crate::api::set_engine(&e) {
                    Ok(msg) => Ok(Slash::Continue(vec![format!("하네스 엔진: {label}\n{msg}")])),
                    Err(err) => Ok(Slash::Continue(vec![format!("저장 실패: {err}")])),
                }
            } else {
                Ok(Slash::Continue(vec![
                    format!("/engine {ENGINES} 중에서 고르세요.")
                ]))
            }
        }
        "/harness" => {
            let strategy = rest.trim().to_ascii_lowercase();
            if strategy.is_empty() {
                return Ok(Slash::Continue(vec![format!(
                    "Harness strategy: {}   ·   /harness single|multi",
                    session.cfg.file.harness.strategy
                )]));
            }
            let Some(parsed) = crate::harness::HarnessStrategy::parse(&strategy) else {
                return Ok(Slash::Continue(vec![
                    "Choose: [1] Single  [2] Multi   ·   /harness single|multi".into(),
                ]));
            };
            match crate::harness::set_strategy(&session.cfg, parsed)
                .and_then(|_| session.cfg.reload())
            {
                Ok(cfg) => {
                    session.cfg = cfg;
                    session.sticky = None;
                    Ok(Slash::Continue(vec![format!(
                        "Harness strategy: {}",
                        session.cfg.file.harness.strategy
                    )]))
                }
                Err(error) => Ok(Slash::Continue(vec![format!(
                    "Harness strategy 저장 실패: {error:#}"
                )])),
            }
        }
        "/mode" => {
            let m = rest.to_ascii_lowercase();
            match m.as_str() {
                "" => Ok(Slash::Continue(vec![format!(
                    "현재 모드: {}   (/mode plan|build)",
                    session.mode
                )])),
                "plan" => {
                    session.mode = "plan".into();
                    Ok(Slash::Continue(vec![
                        "plan 모드: 읽기 전용 도구만 사용합니다.".into()
                    ]))
                }
                "build" => {
                    session.mode = "build".into();
                    Ok(Slash::Continue(vec![
                        "build 모드: 전체 도구를 사용합니다.".into()
                    ]))
                }
                other => Ok(Slash::Continue(vec![format!(
                    "'{other}' 는 모드가 아닙니다. /mode plan|build"
                )])),
            }
        }
        "/sessions" => {
            let rows = db.list_sessions(20)?;
            if rows.is_empty() {
                return Ok(Slash::Continue(vec!["저장된 세션이 없습니다.".into()]));
            }
            let mut notes = vec!["최근 세션:".into()];
            for (i, r) in rows.iter().enumerate() {
                notes.push(format!(
                    "  [{}] {}  {}",
                    i + 1,
                    r.id.clone(),
                    r.title.clone().unwrap_or_else(|| "(제목 없음)".into())
                ));
            }
            notes.push("예: /resume <id>".into());
            Ok(Slash::Continue(notes))
        }
        "/resume" => {
            if rest.is_empty() {
                return Ok(Slash::Continue(vec![
                    "/resume <세션 id>  ·  목록은 /sessions".into(),
                ]));
            }
            let Some(row) = db.load_session(rest)? else {
                return Ok(Slash::Continue(vec![format!(
                    "세션 '{rest}' 를 찾지 못했습니다."
                )]));
            };
            let mut messages: Vec<Message> = serde_json::from_str(&row.messages_json)
                .map_err(|_| anyhow!("세션 메시지를 읽지 못했습니다"))?;
            agent::sanitize_tool_pairs(&mut messages);
            session.messages = messages;
            session.session_id = Some(row.id.clone());
            session.dirty = false;
            session.attachments.clear();
            Ok(Slash::Continue(vec![format!(
                "세션 재개: {}  ({})",
                row.id,
                row.title.clone().unwrap_or_else(|| "(제목 없음)".into())
            )]))
        }
        "/compact" => Ok(Slash::Compact),
        "/find" => {
            if rest.is_empty() {
                return Ok(Slash::Continue(vec![
                    "/find <검색어> — 지난 세션 내용을 검색합니다.".into(),
                ]));
            }
            let rows = db.search_sessions(rest, 15)?;
            if rows.is_empty() {
                return Ok(Slash::Continue(vec![format!(
                    "'{rest}' 결과가 없습니다."
                )]));
            }
            let mut notes = vec![format!("'{rest}' 검색 결과 {}건:", rows.len())];
            for r in &rows {
                notes.push(format!(
                    "  {}  {}",
                    r.id.clone(),
                    r.title.clone().unwrap_or_else(|| "(제목 없음)".into())
                ));
            }
            notes.push("이어하기: /resume <id>".into());
            Ok(Slash::Continue(notes))
        }
        "/undo" => {
            let mut cut = None;
            for (i, m) in session.messages.iter().enumerate().rev() {
                if m.role == Role::User
                    && m.content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::Text { .. }))
                {
                    cut = Some(i);
                    break;
                }
            }
            match cut {
                Some(i) => {
                    session.messages.truncate(i);
                    session.dirty = true;
                    Ok(Slash::Continue(vec![format!(
                        "마지막 질문을 되돌렸습니다 ({i}개 유지)."
                    )]))
                }
                None => Ok(Slash::Continue(vec!["되돌릴 질문이 없습니다.".into()])),
            }
        }
        "/tools" => {
            let mut b = bind(
                &session.cfg,
                crate::harness::TaskClass::Dev,
                session.provider.as_deref(),
                session.model.as_deref(),
            )?;
            if session.is_plan_mode() {
                b.tools.retain(|t| {
                    crate::tools::ToolRegistry::READ_ONLY.contains(&t.as_str())
                });
            }
            let reg = crate::tools::ToolRegistry::with_names(&b.tools);
            let mut names: Vec<String> =
                reg.specs().iter().map(|s| s.name.clone()).collect();
            names.sort();
            let mode = if session.is_plan_mode() { "plan" } else { "build" };
            Ok(Slash::Continue(vec![format!(
                "{mode} 모드 도구({}개): {}",
                names.len(),
                names.join(", ")
            )]))
        }
        "/todo" => Ok(Slash::Continue(vec![crate::tools_more::render_todos(
            &crate::tools_more::current_todos(),
        )])),
        "/goal" => {
            let active = db.active_goal()?;
            Ok(Slash::Continue(vec![match active {
                Some(goal) => format!(
                    "진행 중 목표: {} · Todo {}/{} · 연속 실행 {}/8",
                    goal.objective, goal.completed, goal.total, goal.continuations
                ),
                None => "진행 중인 장기 목표가 없습니다.".into(),
            }]))
        }
        "/status" => {
            let cfg = &session.cfg;
            let db2 = Db::open(&Db::db_path()?)?;
            let dn = cfg.file.general.default_provider.clone();
            let (label, model) = match cfg.provider(&dn) {
                Ok(p) => (crate::auth::provider_label(&dn), p.model.clone()),
                Err(_) => (dn.clone(), "-".into()),
            };
            let mut notes = vec![format!(
                "기본  {label} / {model}   ·   모드 {}",
                if session.is_plan_mode() { "plan" } else { "build" }
            )];
            for c in [
                crate::harness::TaskClass::Simple,
                crate::harness::TaskClass::Medium,
                crate::harness::TaskClass::Advanced,
                crate::harness::TaskClass::Dev,
            ] {
                if let Ok(b) = crate::harness::bind(cfg, c, None, None) {
                    notes.push(format!(
                        "  {:8} → {:8} {}/{}",
                        b.class.as_str(),
                        b.profile_name,
                        b.provider_name,
                        b.model
                    ));
                }
            }
            let (runs, tin, tout) = db2.usage_today()?;
            notes.push(format!("오늘 실행 {runs}회 · 토큰 in {tin} / out {tout}"));
            Ok(Slash::Continue(notes))
        },
        "/theme" => {
            if rest.is_empty() {
                let names = crate::palette::names().join(", ");
                return Ok(Slash::Continue(vec![format!(
                    "현재 테마: {}   (/theme {names})",
                    session.cfg.file.ui.theme
                )]));
            }
            if !crate::palette::names().contains(&rest) {
                return Ok(Slash::Continue(vec![format!(
                    "'{rest}' 테마가 없습니다. 사용 가능: {}",
                    crate::palette::names().join(", ")
                )]));
            }
            match crate::config::write_toml_key(
                &session.cfg.path,
                "[ui]",
                "theme",
                &crate::config::toml_string(rest),
            )
            .and_then(|_| session.cfg.reload())
            {
                Ok(cfg) => {
                    session.cfg = cfg;
                    Ok(Slash::Continue(vec![format!("테마: {rest}")]))
                }
                Err(e) => Ok(Slash::Continue(vec![format!("테마 저장 실패: {e:#}")])),
            }
        },
        "/file" => {
            if rest.is_empty() {
                return Ok(Slash::Continue(vec![
                    "/file <경로> — 다음 질문에 파일 내용을 붙여 넣습니다. @경로 멘션도 가능.".into(),
                ]));
            }
            let resolved = crate::tools::resolve_in_workspace(&session.cfg.workspace, rest)
                .map_err(|e| anyhow!("{e:#}"))?;
            if !resolved.is_file() {
                return Ok(Slash::Continue(vec![format!(
                    "파일이 아닙니다: {}",
                    resolved.display()
                )]));
            }
            let body = std::fs::read_to_string(&resolved)
                .map_err(|_| anyhow!("읽을 수 없습니다: {}", resolved.display()))?;
            let shown: String = body.chars().take(MAX_ATTACH_CHARS).collect();
            let size = shown.chars().count();
            session.attachments.push((rest.to_string(), shown));
            Ok(Slash::Continue(vec![format!(
                "첨부함: {} ({size}자). 다음 질문부터 함께 전달됩니다.",
                resolved.display()
            )]))
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
                    notes.push(apply_model_choice(session, &regs, line.trim()));
                }
                Ok(Slash::Continue(notes))
            } else {
                Ok(Slash::Continue(vec![apply_model_choice(
                    session,
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
                    notes.push(apply_provider_choice(session, &names, line.trim()));
                }
                Ok(Slash::Continue(notes))
            } else {
                Ok(Slash::Continue(vec![apply_provider_choice(
                    session, &names, rest,
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
    "/mode plan|build      계획(읽기 전용)/실행 모드 전환\n\
     /tools                현재 모드에서 쓸 수 있는 도구\n\
     /status               연결·하네스·오늘 사용량 요약\n\
     /todo                 작업 목록 보기\n\
     /goal                 장기 목표·자동 연속 실행 상태\n\
     /file <경로>          다음 질문에 파일 첨부 (@src/main.rs 멘션도 가능)\n\
     /sessions · /resume <id>   세션 목록 · 이어하기\n\
     /find <검색어>         지난 세션 내용 검색\n\
     /compact              지금까지 대화를 요약해 맥락 압축\n\
     /undo                 마지막 질문 되돌리기\n\
     /save  /clear  /quit\n\
     /model  /provider  /connect  /class <simple|medium|advanced|dev>\n\
     /obsidian on|off  /agent <지시>\n\
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
        Ok(_) => {
            // 영속화: 이 서비스를 기본 연결로 저장 — 재시작 후에도 동일 설정.
            let _ = crate::accounts_ui::set_default_provider(cfg, &alias);
            Ok(Slash::Continue(vec![
                format!(
                    "{} 연결됨 · 기본 연결로 저장했습니다 (재시작 후에도 유지)",
                    crate::auth::provider_label(&alias)
                ),
                "모델 변경은 /model 또는 rafikx model".into(),
            ]))
        }
        Err(e) => Ok(Slash::Continue(vec![format!("연결 실패: {e:#}")])),
    }
}

fn apply_model_choice(
    session: &mut Session,
    regs: &[crate::auth::RegisteredModel],
    rest: &str,
) -> String {
    let model = &mut session.model;
    if rest.is_empty() {
        return format!("현재 모델: {}", model.as_deref().unwrap_or("(하네스 자동)"));
    }
    if let Some(nums) = crate::menu::parse_numbers(rest, regs.len(), false, true) {
        if nums.first() == Some(&0) {
            session.provider = None;
            *model = None;
            return "모델을 하네스 자동으로 돌렸습니다.".into();
        }
        if let Some(i) = nums.first() {
            if let Some(r) = regs.get(i - 1) {
                session.provider = Some(r.provider.clone());
                *model = Some(r.id.clone());
                // 영속화: 프로바이더 기본 모델로도 저장
                let _ = crate::accounts_ui::write_provider_model(&session.cfg, &r.provider, &r.id);
                return format!("모델: {} / {} (기본 저장)", r.provider, r.id);
            }
        }
    }
    // 직접 입력 — 등록 목록에서 역방향 매칭되면 영속화
    *model = Some(rest.to_string());
    if let Some(r) = regs.iter().find(|r| r.id == rest) {
        session.provider = Some(r.provider.clone());
        let _ = crate::accounts_ui::write_provider_model(&session.cfg, &r.provider, &r.id);
        return format!("모델: {rest} (기본 저장)");
    }
    format!("모델: {rest} (세션 한정)")
}

fn apply_provider_choice(session: &mut Session, names: &[String], rest: &str) -> String {
    let provider = &mut session.provider;
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
                let _ = crate::accounts_ui::set_default_provider(&session.cfg, n);
                return format!("프로바이더: {n} (기본 저장)");
            }
        }
    }
    let alias = crate::auth::resolve_provider_alias(rest).unwrap_or_else(|| rest.to_string());
    if names.iter().any(|n| n == &alias) {
        *provider = Some(alias.clone());
        let _ = crate::accounts_ui::set_default_provider(&session.cfg, &alias);
        return format!("프로바이더: {alias} (기본 저장)");
    }
    let labels: Vec<String> = names.iter().map(|n| crate::auth::provider_label(n)).collect();
    let hits = crate::menu::match_items(rest, &labels);
    if hits.len() == 1 {
        if let Some(n) = names.get(hits[0] - 1) {
            *provider = Some(n.clone());
            let _ = crate::accounts_ui::set_default_provider(&session.cfg, n);
            return format!("프로바이더: {n} (기본 저장)");
        }
    }
    *provider = Some(rest.to_string());
    format!("프로바이더: {rest}")
}

// ---------------------------------------------------------------------------
// @파일 멘션 확장 — "@src/main.rs" 토큰을 파일 내용 블록으로 바꾼다.
// ---------------------------------------------------------------------------

pub fn expand_mentions(cfg: &Config, prompt: &str) -> String {
    let chars: Vec<char> = prompt.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    let mut budget = MAX_ATTACH_CHARS;
    while i < chars.len() {
        let ch = chars[i];
        let at_start = ch == '@'
            && (i == 0 || chars[i - 1].is_whitespace())
            && i + 1 < chars.len()
            && !chars[i + 1].is_whitespace();
        if !at_start {
            out.push(ch);
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < chars.len() && !chars[end].is_whitespace() && end - start < 300 {
            end += 1;
        }
        let token: String = chars[start..end].iter().collect();
        let resolved = crate::tools::resolve_in_workspace(&cfg.workspace, &token).ok();
        let is_file = resolved.as_ref().is_some_and(|r| r.is_file());
        let body = if is_file {
            resolved
                .as_ref()
                .and_then(|r| std::fs::read_to_string(r).ok())
        } else {
            None
        };
        match (resolved, body) {
            (Some(_path), Some(body)) => {
                let taken: String = body.chars().take(budget).collect();
                budget -= taken.chars().count();
                out.push_str(&format!(
                    "[파일: {token}]\n```\n{taken}\n```\n"
                ));
                i = end;
                if budget <= 0 {
                    break;
                }
            }
            _ => {
                // 파일이 아니면 원문 그대로 (@이메일 등)
                out.push('@');
                i += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// compact — 대화 요약으로 맥락 압축
// ---------------------------------------------------------------------------

fn transcript_for_compact(messages: &[Message]) -> String {
    let mut buf = String::new();
    'outer: for m in messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        };
        for b in &m.content {
            if let ContentBlock::Text { text } = b {
                if text.trim().is_empty() {
                    continue;
                }
                let snippet: String = text.chars().take(1500).collect();
                buf.push_str(&format!("{role}: {snippet}\n"));
                if buf.len() > 60_000 {
                    break 'outer;
                }
            }
        }
    }
    buf
}

pub async fn summarize_messages(cfg: &Config, messages: &[Message]) -> Result<String> {
    let transcript = transcript_for_compact(messages);
    if transcript.trim().is_empty() {
        return Err(anyhow!("요약할 대화가 없습니다"));
    }
    let order =
        crate::harness::fallback_order(cfg, &cfg.file.general.default_provider, None);
    let req = ChatRequest {
        model: String::new(),
        system: "너는 대화 요약가다. 아래 대화를 결정·사실·남은 할 일 중심으로 한국어 15줄 이내로 요약하라.\n\
                 파일 경로·오류 메시지 등 나중에 필요한 구체 정보는 그대로 보존하라."
            .into(),
        messages: vec![Message::user_text(transcript)],
        tools: vec![],
        max_tokens: 1024,
        stream: false,
    };
    let (_name, resp) = crate::harness::chat_with_fallback(cfg, &order, "small", req).await?;
    let text = resp
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.trim().to_string()),
            _ => None,
        })
        .unwrap_or_default();
    if text.is_empty() {
        return Err(anyhow!("요약이 비어 있습니다"));
    }
    Ok(text)
}

/// 세션 메시지를 요약 하나로 압축한다. /compact 공용 진입점.
pub async fn compact_session(session: &mut Session) -> Result<usize> {
    let summary = summarize_messages(&session.cfg, &session.messages).await?;
    let len = summary.chars().count();
    session.messages = vec![Message::user_text(format!("[이전 대화 요약]\n{summary}"))];
    session.last_context_tokens = session
        .messages
        .iter()
        .map(crate::packer::message_tokens)
        .sum::<usize>()
        .min(u32::MAX as usize) as u32;
    session.dirty = true;
    Ok(len)
}

// ---------------------------------------------------------------------------
// 턴 실행
// ---------------------------------------------------------------------------

pub async fn run_turn(
    session: &mut Session,
    prompt: &str,
    forced_class: Option<&str>,
    obsidian_on: bool,
    local_ask: Option<LocalAsk>,
) -> Result<TurnInfo> {
    let started = std::time::Instant::now();
    crate::spinner::set_label("질문 확인 중…");
    let class = classify(&session.cfg, prompt, obsidian_on, forced_class).await?;
    // 연속성: 사용자가 직접 고르지 않았으면 마지막 성공 조합(provider, model)을 재사용해
    // 매 턴마다 다른 모델이 추첨되어 인증·리밋 오류가 나는 일을 막는다.
    let (ov_provider, ov_model) = if session.provider.is_none() && session.model.is_none() {
        match &session.sticky {
            Some((sp, sm)) => (Some(sp.clone()), Some(sm.clone())),
            None => (None, None),
        }
    } else {
        (session.provider.clone(), session.model.clone())
    };
    let mut binding = bind(
        &session.cfg,
        class,
        ov_provider.as_deref(),
        ov_model.as_deref(),
    )?;
    // opencode 스타일 plan 모드 — 하네스 분류·모델 자동선택은 그대로 두고 도구만 제한.
    let plan = session.is_plan_mode();
    if plan {
        binding.tools.retain(|t| {
            crate::tools::ToolRegistry::READ_ONLY.contains(&t.as_str())
        });
    }
    print_binding(&binding);
    if plan {
        crate::ui::live_line("[모드] plan — 읽기 전용");
    }

    let mut task = expand_mentions(&session.cfg, prompt);
    if !session.attachments.is_empty() {
        let mut pre = String::new();
        for (p, body) in session.attachments.drain(..) {
            pre.push_str(&format!("[첨부 파일: {p}]\n```\n{body}\n```\n"));
        }
        task = format!("{pre}{task}");
    }
    let original_prompt = prompt.to_string();

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

    let estimated_context = session
        .messages
        .iter()
        .map(crate::packer::message_tokens)
        .sum::<usize>()
        .saturating_add(crate::packer::estimate_tokens(&task))
        .min(u32::MAX as usize) as u32;
    let context_used = session.last_context_tokens.max(estimated_context);
    if session.messages.len() > 1
        && crate::packer::needs_auto_compaction(context_used, binding.context_window)
    {
        crate::ui::live_status("Compacting context at 80%");
        match compact_session(session).await {
            Ok(len) => crate::ui::live_line(&format!(
                "[context] 자동 압축 완료 · 연속성 요약 {len}자"
            )),
            Err(error) => crate::ui::live_warn(&format!(
                "[context] 자동 압축 실패 · 기존 안전 packer로 계속합니다: {error:#}"
            )),
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
    applog::debug(&format!(
        "bind source={} ov_provider={:?} ov_model={:?} sticky={:?} mode={} class={} profile={} provider={} model={}",
        if ov_provider.is_some() { "override" } else { "auto" },
        ov_provider,
        ov_model,
        session.sticky,
        session.mode,
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
            let summary = CompletionSummary::from_outcome(&outcome, &binding.model);
            agent::record_finish(&db, &run_id, &outcome)?;
            crate::graph::node("persist", &outcome.status, "", Some("bind"));
            crate::ui::live_status(&format!(
                "[run] mode={} class={} profile={} status={} iter={} tokens in={} out={}",
                session.mode,
                binding.class.as_str(),
                binding.profile_name,
                outcome.status,
                outcome.iterations,
                outcome.input_tokens,
                outcome.output_tokens
            ));
            if !outcome.messages.is_empty() {
                // 프롬프트 캐시(omp/opencode 방식): 히스토리는 실제 모델에 보낸 그대로 보관한다.
                // 첫 유저 메시지를 원문으로 재작성하면 다음 턴의 접두어가 달라져
                // 공급자측 프롬프트 캐시가 매번 무효화되다.
                let _ = &original_prompt;
                session.messages = outcome.messages.clone();
            } else {
                session.messages.push(Message::user_text(prompt));
            }
            agent::sanitize_tool_pairs(&mut session.messages);
            session.last_context_tokens = outcome.context_tokens;
            session.dirty = true;
            crate::lessons::maybe_spawn(&session.cfg, prompt, &outcome);
            crate::ui::print_footer();
            // 성공한 조합을 세션에 고정(사용자 지정이 없을 때만) — 다음 턴도 같은 모델로 진행.
            if session.provider.is_none() && session.model.is_none() {
                session.sticky = Some((binding.provider_name.clone(), binding.model.clone()));
            }
            // 영속화: 재시작해도 같은 조합으로 시작한다.
            persist_last_choice(
                &session.cfg.clone(),
                &binding.provider_name,
                &binding.model,
            );
            Ok(TurnInfo {
                run_id,
                label: info_label,
                status: outcome.status,
                tokens_in: outcome.input_tokens,
                tokens_out: outcome.output_tokens,
                ctx_used: outcome.context_tokens,
                ctx_window: binding.context_window,
                cached_in: outcome.cached_tokens,
                cache_reported: outcome.cache_reported,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary,
            })
        }
        Err(e) => {
            // 실패한 조합 고정을 풀어 다음 턴에서 다시 선정하게 한다.
            session.sticky = None;
            let _ = db.finish_run(&run_id, "fail", 0, 0, 0, Some(&e.to_string()));
            crate::graph::node("persist", "fail", &e.to_string(), Some("bind"));
            let fail = agent::AgentOutcome {
                status: "fail".into(),
                iterations: 0,
                input_tokens: 0,
                output_tokens: 0,
                context_tokens: 0,
                cached_tokens: 0,
                cache_reported: false,
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
    /// 마지막 요청의 컨텍스트 사용 추정치 (마지막 턴 입력 토큰)
    pub ctx_used: u32,
    /// 선택된 모델의 컨텍스트 창
    pub ctx_window: u32,
    /// 마지막 모델 요청에서 재사용한 프롬프트 캐시 토큰
    pub cached_in: u32,
    /// 공급자가 캐시 사용량을 보고했는지 여부
    pub cache_reported: bool,
    /// 이 답변에 걸린 시간 (밀리초)
    pub elapsed_ms: u64,
    pub summary: CompletionSummary,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_prefers_connected_minimax_m3() {
        let cfg = Config::load(None).expect("config");
        let pair = default_new_session_pair(&cfg);
        if crate::auth::registered_models(&cfg)
            .iter()
            .any(|model| crate::ranks::normalize_id(&model.id) == "minimax-m3")
        {
            assert_eq!(
                pair.as_ref().map(|(_, model)| crate::ranks::normalize_id(model)),
                Some("minimax-m3".into())
            );
        }
    }

    #[test]
    fn completion_summary_uses_observed_outcome_only() {
        let outcome = crate::agent::AgentOutcome {
            status: "ok".into(),
            iterations: 4,
            changed_files: vec!["src/main.rs".into()],
            ..Default::default()
        };
        let summary = CompletionSummary::from_outcome(&outcome, "minimax-m3");
        assert_eq!(summary.changed_files, vec!["src/main.rs"]);
        assert_eq!(summary.iterations, 4);
        assert!(summary.improvement.contains("MiniMax-M3"));
    }

    #[test]
    fn engine_slash_end_to_end() {
        let cfg_path = Config::load(None).expect("config");
        let mut s = Session {
            cfg: cfg_path,
            yes: true,
            provider: None,
            model: None,
            class: None,
            mode: "build".into(),
            session_id: None,
            messages: vec![],
            last_context_tokens: 0,
            obsidian_on: false,
            attachments: vec![],
            dirty: false,
            sticky: None,
        };

        // pi 엔진 — 정상 저장되어야 한다.
        let out = handle_slash(&mut s, "/engine pi", false).expect("ok");
        assert!(matches!(out, Slash::Continue(_)));
        assert_eq!(s.cfg.file.general.engine, "pi");

        // dk 엔진 — 정상 저장되어야 한다.
        let out = handle_slash(&mut s, "/engine dk", false).expect("ok");
        assert!(matches!(out, Slash::Continue(_)));
        assert_eq!(s.cfg.file.general.engine, "dk");

        // 미지원 값 — 거부 안내를 반환한다.
        let out = handle_slash(&mut s, "/engine nope", false).expect("ok");
        match out {
            Slash::Continue(notes) => {
                assert!(notes.iter().any(|n| n.contains("rafikx|deepseek|dk|pi")));
            }
            _ => panic!("expected Continue"),
        }

        // rafikx 로 원복해 테스트 흔적을 정리한다.
        let _ = handle_slash(&mut s, "/engine rafikx", false);
        assert_eq!(s.cfg.file.general.engine, "rafikx");
    }

    #[test]
    fn engine_slash_accepts_dk_and_pi_only() {
        assert!(is_valid_engine("rafikx"));
        assert!(is_valid_engine("deepseek"));
        assert!(is_valid_engine("dk"));
        assert!(is_valid_engine("pi"));
        // 오타·미지원 값은 거부해야 한다.
        assert!(!is_valid_engine("dkharness"));
        assert!(!is_valid_engine(""));
        assert!(!is_valid_engine("gpt"));
    }

    #[test]
    fn mentions_expand_only_existing_files() {
        let dir = std::env::temp_dir().join("rafikx-mention-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src").join("lib.rs"), "fn f() {}\n").unwrap();
        let cfg_path = Config::load(None).expect("config");
        // workspace 만 교체한 설정 복제
        let mut file = cfg_path.file.clone();
        file.general.workspace = dir.display().to_string();
        let cfg = Config {
            path: cfg_path.path.clone(),
            data_dir: cfg_path.data_dir.clone(),
            workspace: dir.clone(),
            file,
        };

        let out = expand_mentions(&cfg, "@src/lib.rs 요약해줘 me@example.com 은 유지");
        assert!(out.contains("[파일: src/lib.rs]"));
        assert!(out.contains("fn f()"));
        assert!(out.contains("me@example.com"));
        assert!(!out.starts_with("@"));

        let plain = expand_mentions(&cfg, "@없는파일.txt 그대로");
        assert_eq!(plain, "@없는파일.txt 그대로");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn undo_cuts_last_user_exchange() {
        let msgs = vec![
            Message::user_text("첫"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "응답1".into(),
                }],
            },
            Message::user_text("둘"),
        ];
        let mut session_msgs = msgs.clone();
        let mut cut = None;
        for (i, m) in session_msgs.iter().enumerate().rev() {
            if m.role == Role::User
                && m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Text { .. }))
            {
                cut = Some(i);
                break;
            }
        }
        session_msgs.truncate(cut.unwrap());
        assert_eq!(session_msgs.len(), 2);
    }
}
