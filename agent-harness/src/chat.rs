use std::io::{self, Write};
use std::sync::Arc;

use anyhow::{Result, anyhow};

use crate::agent::{self, LocalAsk};
use crate::applog;
use crate::config::Config;
use crate::db::Db;
use crate::harness::{bind, print_binding, run_pipeline_with_context};
use crate::obsidian;
use crate::provider::{ChatRequest, ContentBlock, Message, Role};
use crate::run::{RunContext, RunId};

pub type RunObserver = Arc<dyn Fn(RunContext) + Send + Sync>;

/// plan 모드에서 허용되는 읽기 전용 도구만 남긴다.
const MAX_ATTACH_CHARS: usize = 80_000;

#[derive(Clone, Debug, Default)]
pub struct CompletionSummary {
    pub changed_files: Vec<String>,
    pub iterations: u32,
    pub completed_todos: usize,
    pub total_todos: usize,
    pub tool_errors: usize,
    pub provider: String,
    pub model: String,
    pub auto_compacted: bool,
    pub memory_enabled: bool,
    pub memory_sources: usize,
}

impl CompletionSummary {
    fn from_outcome(
        outcome: &agent::AgentOutcome,
        todos: &[crate::tools_more::TodoItem],
        provider: &str,
        model: &str,
        auto_compacted: bool,
        memory_enabled: bool,
        memory_sources: usize,
    ) -> Self {
        let progress = crate::tools_more::todo_progress(todos);
        Self {
            changed_files: outcome.changed_files.clone(),
            iterations: outcome.iterations,
            completed_todos: progress.completed,
            total_todos: progress.total,
            tool_errors: outcome.tool_errors.len(),
            provider: provider.into(),
            model: model.into(),
            auto_compacted,
            memory_enabled,
            memory_sources,
        }
    }
}

fn final_assistant_answer(outcome: &agent::AgentOutcome) -> String {
    if outcome.status != "ok" {
        return String::new();
    }
    agent::deliverable_assistant_text(&outcome.messages)
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
    ("/model", "모델 선택 · refresh 로 원격 목록 갱신"),
    (
        "/engine",
        "Harness 엔진 rafikx|claude|deepseek|qwen|kimi|pi · provider mode single|multi",
    ),
    (
        "/discipline",
        "실행 분야 harness|loop|graph — 루프 강화 · 노드 DAG 실행",
    ),
    (
        "/team",
        "팀 모드 single|multi — 독립 단계를 역할 서브에이전트로 위임(병렬)",
    ),
    ("/harness", "Harness strategy single|multi"),
    (
        "/selfharness",
        "Self-Harness 메타 레이어 on|off — 모든 엔진 위 자기개선 루프",
    ),
    ("/mode", "plan(읽기전용)/build 전환"),
    ("/yolo", "권한무시 on|off — 도구 자동 승인 (영속)"),
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
    ("/goal", "장기 목표 상태 · resume 이어가기 · clear 해제"),
    ("/init-deep", "디렉터리별 AGENTS.md 초안 생성"),
    ("/ulw", "자율 완수 루프 — 증거가 모일 때까지"),
    ("/ulw-resume", "중단된 ulw 루프 재개 [id]"),
    ("/quota", "계정·프로바이더 쿼터 상태"),
    ("/tips", "기능 팁 목록 · off 끄기"),
    ("/tip", "팁 상세 /tip <id> — 구현 코드 포함"),
    ("/facts", "기억한 지속 사실 목록"),
    ("/forget", "지속 사실 삭제 /forget <key>"),
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
                Slash::New(notes) => {
                    // 화면을 지우고 커서를 원점으로 — 새 세션은 빈 화면에서 시작한다.
                    print!("\x1b[2J\x1b[H");
                    for n in notes {
                        println!("{n}");
                    }
                }
                Slash::Quit => break,
                Slash::Agent(task) => {
                    run_turn(&mut session, &task, Some("dev"), false, None).await?;
                }
                Slash::Ulw { goal } => {
                    match crate::ulw::UlwState::start(&session.cfg.workspace, &goal) {
                        Ok(state) => {
                            if let Err(e) = ulw_loop(&mut session, &goal, state).await {
                                println!("[ulw] 오류: {e:#}");
                            }
                        }
                        Err(e) => println!("[ulw] 시작 실패: {e:#}"),
                    }
                }
                Slash::UlwResume { run_id } => {
                    if let Err(e) = ulw_resume(&mut session, run_id).await {
                        println!("[ulw] 재개 오류: {e:#}");
                    }
                }
                Slash::Compact => {
                    match compact_session(&mut session).await {
                        Ok(s) => println!("대화를 요약해 압축했습니다 ({s}자)."),
                        Err(e) => println!("압축 실패: {e:#}"),
                    };
                }
                Slash::AssignRoles => match crate::harness::auto_assign_roles(&session.cfg).await {
                    Ok(notes) => {
                        for n in notes {
                            println!("{n}");
                        }
                        if let Ok(cfg) = session.cfg.reload() {
                            session.cfg = cfg;
                        }
                        session.sticky = None;
                    }
                    Err(e) => println!("역할 배정 실패: {e:#}"),
                },
                Slash::ModelFetch { query, fetch } => {
                    if fetch {
                        println!("모델 목록 조회 중…");
                        let rows = crate::auth::refresh_catalogs(&session.cfg).await;
                        for n in crate::auth::refresh_summary(&rows) {
                            println!("{n}");
                        }
                    }
                    // CLI 에는 피커가 없으니 (검색어로 걸러진) 번호 목록을 그대로 보여준다.
                    let regs = crate::auth::registered_models(&session.cfg);
                    for n in model_list_notes(&regs, &query) {
                        println!("{n}");
                    }
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
    // TUI(announce=false)에서는 조용히 — AlternateScreen 전환으로 stdout 이
    // 유실되어 어차피 보이지 않고, 내부 동작이라 알릴 필요도 없다.
    for note in crate::auth::auto_import_cli_logins(&cfg) {
        if announce {
            crate::ui::note(&note);
        }
    }
    // 새 릴리스 확인 (비동기 화면용과 별개로, 일반 채팅 시작 시 짧게 확인해서 안내만).
    if announce {
        // 별도 스레드에서 확인 (TUI의 async 런타임을 막지 않는다)
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::update::latest_release());
        });
        let checked = rx.recv_timeout(std::time::Duration::from_secs(6)).ok();
        if let Some(Ok(rel)) = checked
            && let Some(notice) = crate::update::upgrade_notice(&rel, env!("CARGO_PKG_VERSION"))
        {
            for line in notice.lines() {
                crate::ui::note(&format!("⚠ {line}"));
            }
        }
    }
    // 권한무시(YOLO)가 config 에 켜져 있으면 처음부터 자동 승인으로 시작한다.
    let yes = yes || cfg.file.general.approval.eq_ignore_ascii_case("yolo");
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

/// /new 는 사용자가 마지막으로 쓰던 (provider, model) 을 유지한다 —
/// 특정 모델(minimax-m3)로 강제 리셋하던 하드코딩을 제거했다.
fn default_new_session_pair(cfg: &Config) -> Option<(String, String)> {
    let lp = cfg.file.general.last_provider.trim();
    let lm = cfg.file.general.last_model.trim();
    if !lp.is_empty() && !lm.is_empty() && cfg.file.providers.contains_key(lp) {
        return Some((lp.to_string(), lm.to_string()));
    }
    None
}

fn seed_default_choice(mut session: Session) -> Session {
    if session.provider.is_none()
        && session.model.is_none()
        && let Some(pair) = default_new_session_pair(&session.cfg)
    {
        session.sticky = Some(pair);
    }
    session
}

/// 재시작 후에도 이전에 선택/성공한 (provider, model)을 이어받는다.
/// 사용자가 명시적으로 지정한 경우에는 그 값을 우선한다.
fn seed_last_choice(mut s: Session) -> Session {
    if s.provider.is_none() && s.model.is_none() {
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
            if !selected_model.is_empty() {
                s.sticky = Some((lp.to_string(), selected_model));
            }
        }
    }
    s
}

/// 마지막 선택을 config 에 영속 저장 — 재시작 후에도 같은 모델로 실행.
pub fn persist_last_choice(cfg: &Config, provider: &str, model: &str) {
    use crate::config::{toml_string, write_toml_key};
    let _ = write_toml_key(
        &cfg.path,
        "[general]",
        "last_provider",
        &toml_string(provider),
    );
    let _ = write_toml_key(&cfg.path, "[general]", "last_model", &toml_string(model));
}

pub fn save_if_dirty(session: &mut Session) -> Result<Option<String>> {
    if !session.dirty {
        return Ok(None);
    }
    let db = Db::open(&Db::db_path()?)?;
    let id = persist(&db, session.session_id.as_deref(), &mut session.messages)?;
    session.session_id = Some(id.clone());
    session.dirty = false;
    Ok(Some(id))
}

#[derive(Debug)]
pub enum Slash {
    Continue(Vec<String>),
    /// /new — 세션을 비우고 새로 시작. TUI 는 화면(트랜스크립트·패널)도 함께 지운다.
    New(Vec<String>),
    Quit,
    Agent(String),
    /// /compact — 대화 요약으로 압축 (비동기)
    Compact,
    /// /engine multi — 등록 모델 원격 조회 후 역할별 자동 배정 (비동기)
    AssignRoles,
    /// /ulw <목표> — 자율 완수 루프 (비동기, .omo/ulw/ 산출물)
    Ulw { goal: String },
    /// /ulw-resume [run-id] — 중단·미완료 루프 재개
    UlwResume { run_id: Option<String> },
    /// /model refresh — 원격 모델 목록 갱신(fetch=true, 비동기) 후 선택 UI.
    /// `/model <검색어>` 는 fetch=false 로 조회 없이 검색어만 넘긴다.
    ModelFetch {
        query: String,
        fetch: bool,
    },
}

/// /engine 에서 선택 가능한 엔진인지 판정한다 — 카탈로그 6종 + legacy `self`.
/// 제거된 값(`dk` 등)은 여기서 거부하고 engine::normalize 만 흡수한다.
pub fn is_valid_engine(e: &str) -> bool {
    crate::engine::is_selectable(e)
}

pub fn handle_slash(session: &mut Session, line: &str, read_stdin: bool) -> Result<Slash> {
    let db = Db::open(&Db::db_path()?)?;
    let mut parts = line.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    match cmd {
        "/quit" | "/exit" => Ok(Slash::Quit),
        "/help" => Ok(Slash::Continue(vec![help_text()])),
        "/quota" => Ok(Slash::Continue(crate::usage::quota_lines(&session.cfg))),
        "/tips" => {
            match rest {
                "off" => {
                    crate::config::write_toml_key(&session.cfg.path, "[ui]", "tips", "false")?;
                    Ok(Slash::Continue(vec!["시작 화면 팁을 껐습니다. /tips on 으로 다시 켭니다.".into()]))
                }
                "on" => {
                    crate::config::write_toml_key(&session.cfg.path, "[ui]", "tips", "true")?;
                    Ok(Slash::Continue(vec!["시작 화면 팁을 켰습니다.".into()]))
                }
                "" => Ok(Slash::Continue(crate::tips::list_lines())),
                _ => Ok(Slash::Continue(vec!["/tips · /tips on|off · /tip <id>".into()])),
            }
        }
        "/tip" => {
            if rest.is_empty() {
                return Ok(Slash::Continue(vec!["/tip <id> — 예: /tip ulw".into()]));
            }
            match crate::tips::find(rest) {
                Some(tip) => Ok(Slash::Continue(crate::tips::detail_lines(&tip))),
                None => Ok(Slash::Continue(vec![format!(
                    "'{rest}' 팁이 없습니다. /tips 로 목록을 보세요."
                )])),
            }
        }
        "/facts" => {
            let rows = db.list_facts(Some(&session.cfg.workspace))?;
            if rows.is_empty() {
                return Ok(Slash::Continue(vec!["기억하는 사실이 없습니다.".into()]));
            }
            let notes = rows
                .iter()
                .map(|r| {
                    let scope = if r.project_id.is_empty() { "전역" } else { "프로젝트" };
                    format!("({}·{}) {}: {}", r.kind, scope, r.key, r.value)
                })
                .collect();
            Ok(Slash::Continue(notes))
        }
        "/init-deep" => Ok(Slash::Continue(init_deep_notes(&session.cfg.workspace))),
        "/ulw" => {
            if rest.is_empty() {
                return Ok(Slash::Continue(vec![
                    "/ulw <목표> — 완료 기준의 증거가 모일 때까지 자율 실행. /ulw-resume [id] 로 재개".into(),
                ]));
            }
            Ok(Slash::Ulw { goal: rest.to_string() })
        }
        "/ulw-resume" => Ok(Slash::UlwResume {
            run_id: if rest.is_empty() { None } else { Some(rest.to_string()) },
        }),
        "/forget" => {
            if rest.is_empty() {
                return Ok(Slash::Continue(vec!["/forget <key>".into()]));
            }
            match db.forget_fact(Some(&session.cfg.workspace), rest)? {
                Some(row) => Ok(Slash::Continue(vec![format!(
                    "삭제했습니다: {} = {} ({}·{})",
                    row.key, row.value, row.kind, row.source
                )])),
                None => Ok(Slash::Continue(vec![format!(
                    "해당 키를 찾지 못했습니다: {rest}"
                )])),
            }
        }
        "/save" => {
            let id = persist(&db, session.session_id.as_deref(), &mut session.messages)?;
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
            if session.provider.is_none() && session.model.is_none() {
                session.sticky = default_new_session_pair(&session.cfg);
            }
            let default_model = session
                .model
                .as_deref()
                .or_else(|| session.sticky.as_ref().map(|(_, model)| model.as_str()))
                .unwrap_or("auto");
            Ok(Slash::New(vec![format!(
                "새 세션을 시작합니다. 기본 모델: {}",
                default_model
            )]))
        }
        "/clear" => {
            session.messages.clear();
            session.dirty = true;
            Ok(Slash::Continue(vec!["대화 맥락을 지웠습니다.".into()]))
        }
        "/engine" => {
            let arg = rest.trim().to_ascii_lowercase();
            let engines = crate::engine::names_joined();
            // provider 모드 서브커맨드 — Harness 선택 뒤의 두 번째 단계.
            if arg == "multi" {
                return Ok(Slash::AssignRoles);
            }
            if let Some(sub) = arg.strip_prefix("single") {
                return engine_single(session, sub.trim(), read_stdin);
            }
            if arg.is_empty() {
                let cur = session.cfg.file.general.engine.clone();
                let strategy = session.cfg.file.harness.strategy.clone();
                let mut notes = vec![format!(
                    "현재 Harness 엔진: {cur} · provider mode: {strategy}   ·   변경: /engine {engines}"
                )];
                for spec in crate::engine::catalog() {
                    notes.push(format!("  {:<9} {}", spec.name, spec.summary));
                }
                notes.push("  self      (legacy) rafikx + Self-Harness 자기개선 루프".into());
                notes.push(format!(
                    "self 메타: {}   ·   변경: /selfharness on|off",
                    if session.cfg.file.self_harness.meta {
                        "on"
                    } else {
                        "off"
                    }
                ));
                if cur.eq_ignore_ascii_case("self") {
                    notes.extend(crate::self_harness::status_lines(&db));
                }
                notes.push(
                    "provider mode 변경: /engine single <연결>  ·  /engine multi (역할 자동 배정)"
                        .into(),
                );
                Ok(Slash::Continue(notes))
            } else if is_valid_engine(&arg) {
                let label = match arg.as_str() {
                    "self" => "self-harness (자기개선 루프 · arXiv:2606.09498)".into(),
                    other => match crate::engine::resolve(other) {
                        Some(spec) => format!("{} — {}", spec.name, spec.summary),
                        None => other.to_string(),
                    },
                };
                session.cfg.file.general.engine = arg.clone();
                match crate::api::set_engine_for(&session.cfg, &arg) {
                    Ok(msg) => {
                        let mut notes = vec![format!("Harness 엔진: {label}\n{msg}")];
                        if arg == "self" {
                            notes.extend(crate::self_harness::status_lines(&db));
                        }
                        // 2단계: provider 모드 선택 — 하나만 쓸지, 역할별 자동 배정일지.
                        if read_stdin {
                            for n in &notes {
                                println!("{n}");
                            }
                            notes.clear();
                            println!(
                                "Provider mode  [1] Single — 하나의 연결만 사용  [2] Multi — 등록 모델을 역할별 자동 배정"
                            );
                            print!("번호> ");
                            io::stdout().flush()?;
                            let mut line = String::new();
                            io::stdin().read_line(&mut line)?;
                            match line.trim() {
                                "1" => return engine_single(session, "", true),
                                "2" => return Ok(Slash::AssignRoles),
                                _ => notes.push(
                                    "모드 선택을 건너뜁니다. 나중에 /engine single|multi 로 지정하세요."
                                        .into(),
                                ),
                            }
                        } else {
                            notes.push(
                                "Provider mode 선택: /engine single <연결>  ·  /engine multi (역할 자동 배정)"
                                    .into(),
                            );
                        }
                        Ok(Slash::Continue(notes))
                    }
                    Err(err) => Ok(Slash::Continue(vec![format!("저장 실패: {err}")])),
                }
            } else {
                Ok(Slash::Continue(vec![format!(
                    "/engine {engines}|self 중에서 고르거나, /engine single|multi 로 provider mode 를 정하세요."
                )]))
            }
        }
        "/discipline" => {
            // 실행 분야 — 엔진(품질 장치)과 직교하는 축. 제어 전략만 바꾼다.
            let arg = rest.trim().to_ascii_lowercase();
            let names = crate::engine::discipline_names_joined();
            if arg.is_empty() {
                let cur = crate::engine::normalize_discipline(&session.cfg.file.general.discipline);
                let mut notes = vec![format!(
                    "현재 실행 분야: {}   ·   변경: /discipline {names}",
                    cur.as_str()
                )];
                for d in crate::engine::DISCIPLINES {
                    notes.push(format!("  {:<8} {}", d.as_str(), d.summary()));
                }
                return Ok(Slash::Continue(notes));
            }
            match crate::api::set_discipline_for(&session.cfg, &arg) {
                Ok(msg) => {
                    session.cfg.file.general.discipline = arg;
                    Ok(Slash::Continue(vec![msg]))
                }
                Err(err) => Ok(Slash::Continue(vec![format!("{err}")])),
            }
        }
        "/team" => {
            // 팀 모드 — 엔진·분야와 직교하는 축. 위임 지침과 병렬 실행만 바꾼다.
            let arg = rest.trim().to_ascii_lowercase();
            let names = crate::engine::team_names_joined();
            if arg.is_empty() {
                let cur = crate::harness::team_mode(&session.cfg);
                let mut notes = vec![format!(
                    "현재 팀 모드: {}   ·   변경: /team {names}",
                    cur.as_str()
                )];
                for t in crate::engine::TEAM_MODES {
                    notes.push(format!("  {:<8} {}", t.as_str(), t.summary()));
                }
                return Ok(Slash::Continue(notes));
            }
            match crate::api::set_team_for(&session.cfg, &arg) {
                Ok(msg) => {
                    session.cfg.file.harness.team = arg;
                    Ok(Slash::Continue(vec![msg]))
                }
                Err(err) => Ok(Slash::Continue(vec![format!("{err}")])),
            }
        }
        "/selfharness" => {
            // Self-Harness 메타 레이어 — 엔진과 무관하게 자기개선 루프를 겹친다.
            let arg = rest.trim().to_ascii_lowercase();
            let legacy = session
                .cfg
                .file
                .general
                .engine
                .trim()
                .eq_ignore_ascii_case("self");
            if arg.is_empty() {
                let state = crate::self_harness::SelfHarnessState::load();
                let mut notes = vec![format!(
                    "Self-Harness 메타: {} · legacy engine=self: {} · Harness v{}   ·   변경: /selfharness on|off",
                    if session.cfg.file.self_harness.meta {
                        "on"
                    } else {
                        "off"
                    },
                    if legacy { "예" } else { "아니오" },
                    state.version
                )];
                if !session.cfg.file.self_harness.enabled {
                    notes.push(
                        "[self_harness] enabled = false 이므로 메타를 켜도 루프는 돌지 않습니다."
                            .into(),
                    );
                }
                notes.extend(crate::self_harness::status_lines(&db));
                return Ok(Slash::Continue(notes));
            }
            let on = match arg.as_str() {
                "on" => true,
                "off" => false,
                _ => return Ok(Slash::Continue(vec!["/selfharness on|off".into()])),
            };
            session.cfg.file.self_harness.meta = on;
            match crate::api::set_self_meta_for(&session.cfg, on) {
                Ok(msg) => {
                    let mut notes = vec![msg];
                    if on && !session.cfg.file.self_harness.enabled {
                        notes.push(
                            "[self_harness] enabled = false 이므로 루프는 아직 돌지 않습니다."
                                .into(),
                        );
                    }
                    if on {
                        notes.extend(crate::self_harness::status_lines(&db));
                    }
                    Ok(Slash::Continue(notes))
                }
                Err(err) => Ok(Slash::Continue(vec![format!("저장 실패: {err}")])),
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
        "/yolo" => {
            // 권한무시 — 도구 승인을 전부 자동으로. config 에 영속되어
            // 다음 실행도 처음부터 자동 승인으로 시작한다 (/yolo 로 다시 끔).
            let on = match rest {
                "on" => true,
                "off" => false,
                _ => !session.yes,
            };
            session.yes = on;
            let value = if on { "yolo" } else { "ask" };
            session.cfg.file.general.approval = value.into();
            let _ = crate::config::write_toml_key(
                &session.cfg.path,
                "[general]",
                "approval",
                &crate::config::toml_string(value),
            );
            Ok(Slash::Continue(vec![if on {
                "권한무시(YOLO) 켜짐 — 모든 도구를 자동 승인합니다. 끄기: /yolo off".into()
            } else {
                "권한무시(YOLO) 꺼짐 — 도구 실행 전에 다시 물어봅니다.".into()
            }]))
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
                        "plan 모드: 읽기 전용 도구만 사용합니다.".into(),
                    ]))
                }
                "build" => {
                    session.mode = "build".into();
                    Ok(Slash::Continue(vec![
                        "build 모드: 전체 도구를 사용합니다.".into(),
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
                return Ok(Slash::Continue(vec![format!("'{rest}' 결과가 없습니다.")]));
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
                b.tools
                    .retain(|t| crate::tools::ToolRegistry::READ_ONLY.contains(&t.as_str()));
            }
            let reg = crate::tools::ToolRegistry::with_names(&b.tools);
            let mut names: Vec<String> = reg.specs().iter().map(|s| s.name.clone()).collect();
            names.sort();
            let mode = if session.is_plan_mode() {
                "plan"
            } else {
                "build"
            };
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
            if rest == "clear" {
                return Ok(Slash::Continue(vec![match db.clear_active_goal()? {
                    true => "활성 목표를 해제했습니다.".into(),
                    false => "활성 목표가 없습니다.".into(),
                }]));
            }
            let active = db.active_goal()?;
            Ok(Slash::Continue(vec![match active {
                Some(goal) => format!(
                    "진행 중 목표: {} · Todo {}/{} · 연속 실행 {}/8 · 이어가기 /goal resume · 지우기 /goal clear",
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
                if session.is_plan_mode() {
                    "plan"
                } else {
                    "build"
                }
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
        }
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
        }
        "/file" => {
            if rest.is_empty() {
                return Ok(Slash::Continue(vec![
                    "/file <경로> — 다음 질문에 파일 내용을 붙여 넣습니다. @경로 멘션도 가능."
                        .into(),
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
        "/model" | "/models" => {
            // 원격 조회는 비동기라 여기서 못 돈다 — 호출자(TUI·CLI)에게 위임한다.
            if is_model_refresh_arg(rest) {
                return Ok(Slash::ModelFetch {
                    query: String::new(),
                    fetch: true,
                });
            }
            let regs = crate::auth::registered_models(&session.cfg);
            if regs.is_empty() {
                return Ok(Slash::Continue(vec![
                    "등록된 모델이 없습니다. rafikx settings 에서 연결하세요.".into(),
                ]));
            }
            if rest.is_empty() {
                let mut notes = model_list_notes(&regs, "");
                if read_stdin {
                    print!("번호> ");
                    io::stdout().flush()?;
                    let mut line = String::new();
                    io::stdin().read_line(&mut line)?;
                    notes.push(apply_model_choice(session, &regs, line.trim()));
                }
                Ok(Slash::Continue(notes))
            } else if model_arg_is_query(&regs, rest) {
                Ok(Slash::ModelFetch {
                    query: rest.to_string(),
                    fetch: false,
                })
            } else {
                Ok(Slash::Continue(vec![apply_model_choice(
                    session, &regs, rest,
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
                Ok(Slash::Continue(vec![
                    "/agent <지시> 형식으로 쓰세요.".into(),
                ]))
            } else {
                Ok(Slash::Agent(rest.to_string()))
            }
        }
        "/connect" | "/login" => connect_slash(&session.cfg, rest, read_stdin),
        _ => Ok(Slash::Continue(vec!["알 수 없는 명령입니다. /help".into()])),
    }
}

/// /engine single [연결] — 하나의 연결만 쓰는 provider 모드.
/// 인자가 없으면 연결 목록을 보여주고 (CLI 는 번호 입력, TUI 는 명령 안내),
/// 인자가 있으면 그 연결을 기본으로 고정하고 strategy=single 로 저장한다.
fn engine_single(session: &mut Session, arg: &str, read_stdin: bool) -> Result<Slash> {
    let names = crate::auth::usable_names(&session.cfg);
    if names.is_empty() {
        return Ok(Slash::Continue(vec![
            "연결된 서비스가 없습니다. rafikx settings 에서 먼저 연결하세요.".into(),
        ]));
    }
    let apply = |session: &mut Session, name: &str| -> String {
        match crate::harness::set_single_provider(&session.cfg, name) {
            Ok(msg) => {
                if let Ok(cfg) = session.cfg.reload() {
                    session.cfg = cfg;
                }
                session.provider = None;
                session.model = None;
                session.sticky = None;
                msg
            }
            Err(e) => format!("single 설정 실패: {e:#}"),
        }
    };
    let resolve = |names: &[String], token: &str| -> Option<String> {
        if let Some(nums) = crate::menu::parse_numbers(token, names.len(), false, false)
            && let Some(i) = nums.first()
        {
            return names.get(i.saturating_sub(1)).cloned();
        }
        let alias = crate::auth::resolve_provider_alias(token).unwrap_or_else(|| token.to_string());
        names.iter().find(|n| **n == alias).cloned()
    };
    if !arg.is_empty() {
        return Ok(Slash::Continue(vec![match resolve(&names, arg) {
            Some(name) => apply(session, &name),
            None => format!("'{arg}' 연결을 찾지 못했습니다. /engine single <연결이름|번호>"),
        }]));
    }
    let mut notes = vec!["Single — 사용할 연결을 고르세요:".into()];
    for (i, n) in names.iter().enumerate() {
        notes.push(format!("  [{}] {}", i + 1, crate::auth::provider_label(n)));
    }
    if read_stdin {
        for n in &notes {
            println!("{n}");
        }
        notes.clear();
        print!("번호> ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        notes.push(match resolve(&names, line.trim()) {
            Some(name) => apply(session, &name),
            None => "선택을 건너뜁니다. /engine single <연결이름> 으로 지정하세요.".into(),
        });
    } else {
        notes.push("지정: /engine single <연결이름|번호>".into());
    }
    Ok(Slash::Continue(notes))
}

/// /init-deep — 루트+1단계 디렉터리에 AGENTS.md 초안을 만든다 (F7).
/// 기존 파일은 덮어쓰지 않고 diff 제안만 보여준다.
pub fn init_deep_notes(workspace: &std::path::Path) -> Vec<String> {
    let proposals = crate::rules::propose_init_deep(workspace);
    if proposals.is_empty() {
        return vec!["AGENTS.md 를 만들 대상 디렉터리가 없습니다.".into()];
    }
    let (created, proposed) = crate::rules::apply_init_deep(&proposals);
    let mut notes = Vec::new();
    for path in &created {
        notes.push(format!("생성: {}", path.display()));
    }
    for p in &proposals {
        if p.exists
            && let Some(diff) = &p.diff
        {
            notes.push(format!(
                "기존 파일 있음 — 덮어쓰지 않았습니다: {}\n제안 diff:\n{}",
                p.path.display(),
                diff
            ));
        }
    }
    if created.is_empty() && proposed.is_empty() {
        notes.push("모든 대상에 AGENTS.md 가 이미 있습니다.".into());
    }
    notes
}

pub(crate) fn help_text() -> String {
    "/mode plan|build      계획(읽기 전용)/실행 모드 전환\n\
     /tools                현재 모드에서 쓸 수 있는 도구\n\
     /status               연결·Harness·오늘 사용량 요약\n\
     /todo                 작업 목록 보기\n\
     /goal                 장기 목표·자동 연속 실행 상태\n\
     /facts · /forget <키>  기억한 지속 사실 목록 · 삭제\n\
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
        return Ok(Slash::Continue(vec![format!(
            "'{rest}' 서비스가 config에 없습니다."
        )]));
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
    println!(
        "키는 secrets.toml 에만 저장됩니다. {}",
        crate::auth::env_hint(cfg, &alias)
    );
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

/// `/model refresh` 별칭인지 — 원격 카탈로그를 다시 조회하라는 뜻.
pub fn is_model_refresh_arg(rest: &str) -> bool {
    matches!(
        rest.trim().to_ascii_lowercase().as_str(),
        "refresh" | "fetch" | "새로고침" | "-r"
    )
}

/// `/model <인자>` 를 검색어로 볼지 판정한다.
/// 번호·목록에 정확히 있는 id·목록에 없는 직접 지정은 기존 선택 경로를 그대로 타고,
/// 목록에 부분 일치하는 자유 텍스트만 검색어(피커 타이핑 검색과 같은 뜻)로 본다.
pub fn model_arg_is_query(regs: &[crate::auth::RegisteredModel], rest: &str) -> bool {
    let rest = rest.trim();
    if rest.is_empty() || crate::menu::parse_numbers(rest, regs.len(), false, true).is_some() {
        return false;
    }
    if regs.iter().any(|r| r.id == rest) {
        return false;
    }
    let q = rest.to_lowercase();
    regs.iter()
        .any(|r| r.id.to_lowercase().contains(&q) || r.provider.to_lowercase().contains(&q))
}

/// 등록 모델 목록 줄 — 검색어가 있으면 부분 일치로 거른다.
/// 번호는 전체 목록 기준이라 걸러진 화면에서도 그대로 `/model <번호>` 로 쓸 수 있다.
pub fn model_list_notes(regs: &[crate::auth::RegisteredModel], query: &str) -> Vec<String> {
    let q = query.trim().to_lowercase();
    let mut notes = vec![if q.is_empty() {
        "등록된 모델:".to_string()
    } else {
        format!("등록된 모델 — '{}' 검색:", query.trim())
    }];
    for (i, r) in regs.iter().enumerate() {
        let label = format!("{} / {}", r.provider, r.id);
        if !q.is_empty() && !label.to_lowercase().contains(&q) {
            continue;
        }
        notes.push(format!("  [{}] {}", i + 1, label));
    }
    if notes.len() == 1 {
        notes.push("  (일치하는 모델이 없습니다)".into());
    }
    notes.push("예: /model 2".into());
    notes
}

fn apply_model_choice(
    session: &mut Session,
    regs: &[crate::auth::RegisteredModel],
    rest: &str,
) -> String {
    let model = &mut session.model;
    if rest.is_empty() {
        return format!(
            "현재 모델: {}",
            model.as_deref().unwrap_or("(Harness 자동)")
        );
    }
    if let Some(nums) = crate::menu::parse_numbers(rest, regs.len(), false, true) {
        if nums.first() == Some(&0) {
            session.provider = None;
            *model = None;
            return "모델을 Harness 자동으로 돌렸습니다.".into();
        }
        if let Some(i) = nums.first()
            && let Some(r) = regs.get(i - 1)
        {
            if r.provider == "combo" {
                // 콤보 선택 (F8) — 프로바이더는 비우고 모델에 combo:<이름> 을 담는다.
                // 바인딩이 체인 첫 쌍으로 결정하고 combo_chain 을 단다. 세션 한정.
                session.provider = None;
                *model = Some(format!("combo:{}", r.id));
                return format!("콤보: {} — 체인 폴 fallback 적용 (세션 한정)", r.id);
            }
            session.provider = Some(r.provider.clone());
            *model = Some(r.id.clone());
            // 영속화: 프로바이더 기본 모델로도 저장
            let _ = crate::accounts_ui::write_provider_model(&session.cfg, &r.provider, &r.id);
            return format!("모델: {} / {} (기본 저장)", r.provider, r.id);
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
        if let Some(i) = nums.first()
            && let Some(n) = names.get(i - 1)
        {
            *provider = Some(n.clone());
            let _ = crate::accounts_ui::set_default_provider(&session.cfg, n);
            return format!("프로바이더: {n} (기본 저장)");
        }
    }
    let alias = crate::auth::resolve_provider_alias(rest).unwrap_or_else(|| rest.to_string());
    if names.iter().any(|n| n == &alias) {
        *provider = Some(alias.clone());
        let _ = crate::accounts_ui::set_default_provider(&session.cfg, &alias);
        return format!("프로바이더: {alias} (기본 저장)");
    }
    let labels: Vec<String> = names
        .iter()
        .map(|n| crate::auth::provider_label(n))
        .collect();
    let hits = crate::menu::match_items(rest, &labels);
    if hits.len() == 1
        && let Some(n) = names.get(hits[0] - 1)
    {
        *provider = Some(n.clone());
        let _ = crate::accounts_ui::set_default_provider(&session.cfg, n);
        return format!("프로바이더: {n} (기본 저장)");
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
                out.push_str(&format!("[파일: {token}]\n```\n{taken}\n```\n"));
                i = end;
                if budget == 0 {
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
    let order = crate::harness::fallback_order(cfg, &cfg.file.general.default_provider, None);
    let req = ChatRequest {
        model: String::new(),
        system: "너는 대화 요약가다. 아래 대화를 결정·사실·남은 할 일 중심으로 한국어 15줄 이내로 요약하라.\n\
                 파일 경로·오류 메시지 등 나중에 필요한 구체 정보는 그대로 보존하라.\n\
                 검증 구분(G20): 명령을 실행해 확인된 사실에는 앞에 '✓'를 붙인다.\n\
                 실행 없이 모델이 주장만 한 내용은 ✓ 없이 나열해 검증되지 않았음이 드러나게 한다."
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
    let mut source = session.messages.clone();
    agent::sanitize_tool_pairs(&mut source);
    let compacted = vec![Message::user_text(format!("[이전 대화 요약]\n{summary}"))];
    let source_json = serde_json::to_string(&source)?;
    let compacted_json = serde_json::to_string(&compacted)?;
    let title = session_title(&source);
    let db = Db::open(&Db::db_path()?)?;
    let id = db.save_session_compaction(
        session.session_id.as_deref(),
        &title,
        &source_json,
        &compacted_json,
    )?;
    session.session_id = Some(id);
    session.messages = compacted;
    session.last_context_tokens = session
        .messages
        .iter()
        .map(crate::packer::message_tokens)
        .sum::<usize>()
        .min(u32::MAX as usize) as u32;
    session.dirty = false;
    Ok(len)
}

// ---------------------------------------------------------------------------
// 턴 실행
// ---------------------------------------------------------------------------

/// 다른 세션·에디터가 config.toml 을 바꿨으면 이 세션에도 즉시 반영한다.
/// 설정형 슬래시(/team 등)는 메모리를 직접 갱신하지만, 병렬로 띄운 다른 rafikx
/// 인스턴스의 변경은 파일로만 오므로 턴 시작마다 mtime 을 비교해 다시 읽는다.
pub fn reload_cfg_if_changed(session: &mut Session) {
    let Ok(meta) = std::fs::metadata(&session.cfg.path) else {
        return;
    };
    let Ok(mtime) = meta.modified() else { return };
    if session.cfg.loaded_at.is_some_and(|t| mtime <= t) {
        return;
    }
    if let Ok(fresh) = crate::config::Config::load(Some(&session.cfg.path.clone())) {
        session.cfg = fresh;
    }
}

pub async fn run_turn(
    session: &mut Session,
    prompt: &str,
    forced_class: Option<&str>,
    obsidian_on: bool,
    local_ask: Option<LocalAsk>,
) -> Result<TurnInfo> {
    run_turn_observed(session, prompt, forced_class, obsidian_on, local_ask, None).await
}

pub async fn run_turn_observed(
    session: &mut Session,
    prompt: &str,
    forced_class: Option<&str>,
    obsidian_on: bool,
    local_ask: Option<LocalAsk>,
    observer: Option<RunObserver>,
) -> Result<TurnInfo> {
    let started = std::time::Instant::now();
    let mut memory_sources = 0usize;
    let mut obsidian_sources = Vec::new();
    let mut obsidian_tokens = 0u32;
    let mut auto_compacted = false;
    crate::spinner::set_label("질문 확인 중…");
    reload_cfg_if_changed(session);
    let decision = crate::harness::classify_gated(&session.cfg, prompt, obsidian_on, forced_class).await?;
    let class = crate::harness::continuation_class(
        prompt,
        decision.class,
        &session.messages,
        forced_class.is_some(),
    );
    if decision.via == crate::harness::ClassSource::Judge && decision.rules_class != decision.class
        && let Ok(db) = Db::open(&Db::db_path()?)
    {
        // 재판정이 규칙을 뒤집은 사례 — 규칙 개선 재료로 lessons 에 남긴다 (F2).
        let _ = db.add_lesson(
            "분류 재판정",
            &format!("{}→{}", decision.rules_class.as_str(), decision.class.as_str()),
            &format!(
                "규칙은 {}(으)로 봤지만 재판정은 {}: {}",
                decision.rules_class.as_str(),
                decision.class.as_str(),
                prompt.chars().take(80).collect::<String>()
            ),
            200,
        );
    }
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
    // 레인 라우팅 (F5): 코드 탐색·외부 리서치 패턴이면 읽기 전용 레인 우선 배정.
    let lane = crate::harness::suggest_lane(prompt);
    let mut binding = match lane {
        Some(lane_profile) => crate::harness::bind_profile(
            &session.cfg,
            class,
            Some(lane_profile),
            ov_provider.as_deref(),
            ov_model.as_deref(),
        )?,
        None => bind(
            &session.cfg,
            class,
            ov_provider.as_deref(),
            ov_model.as_deref(),
        )?,
    };

    // 엔진 고정 — sticky 재사용은 "직접 지정"이 아니므로 고정이 이긴다.
    if let Some(w) = crate::harness::apply_engine_pin(
        &session.cfg,
        &mut binding,
        session.provider.as_deref(),
        session.model.as_deref(),
    ) {
        crate::ui::live_warn(&w);
    }
    // opencode 스타일 plan 모드 — Harness 분류·모델 자동선택은 그대로 두고 도구만 제한.
    let plan = session.is_plan_mode();
    if plan {
        binding
            .tools
            .retain(|t| crate::tools::ToolRegistry::READ_ONLY.contains(&t.as_str()));
    }
    print_binding(&session.cfg, &binding);
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
                    memory_sources = ctx.sources.len();
                    obsidian_sources = ctx.sources.clone();
                    obsidian_tokens = crate::context::tokens(&ctx.block);
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
                Err(e) => {
                    crate::ui::live_warn(&format!("Obsidian 컨텍스트를 넣지 못했습니다: {e}"))
                }
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
            Ok(len) => {
                auto_compacted = true;
                crate::ui::live_line(&format!("[context] 자동 압축 완료 · 연속성 요약 {len}자"));
            }
            Err(error) => crate::ui::live_warn(&format!(
                "[context] 자동 압축 실패 · 기존 안전 packer로 계속합니다: {error:#}"
            )),
        }
    }
    crate::ui::live_status("Working");
    // 이번 턴의 실행 축 — working 패널 마지막 줄로 진행 내내 남는다 (§16.2).
    crate::ui::live_mode(&crate::harness::mode_line(&session.cfg));

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
    // 라이브 sink 는 observer 유무와 무관하게 전역 sink 다. TUI 일 때 no-op 을 끼우면
    // `ui::emit_in` 의 단락 평가(`run.emit_live(e) || emit(e)`)가 no-op 에서 true 를
    // 돌려 전역 TUI sink 에 아무 이벤트도 도달하지 못했다 (§16.1).
    let live_sink = crate::ui::current_live_sink();
    let run_context =
        RunContext::for_config(RunId::new(run_id.clone()), Arc::new(session.cfg.clone()))
            .with_live_sink(live_sink);
    if let Some(observer) = observer {
        observer(run_context.clone());
    }
    if let Some(lane_profile) = lane {
        crate::graph::node_in(
            &run_context,
            "bind",
            "lane",
            &format!("레인 우선 배정: {lane_profile}"),
            None,
        );
    }
    let session_tokens = session
        .messages
        .iter()
        .map(crate::packer::message_tokens)
        .sum::<usize>()
        .min(u32::MAX as usize) as u32;
    run_context.record_context_source(
        crate::run::ContextSourceKind::SessionHistory,
        session
            .session_id
            .clone()
            .unwrap_or_else(|| "draft-session".into()),
        binding.context_window.saturating_mul(3) / 5,
        session_tokens,
    );
    if obsidian_tokens > 0 {
        run_context.record_context_source(
            crate::run::ContextSourceKind::Obsidian,
            if obsidian_sources.is_empty() {
                "obsidian:no-match".into()
            } else {
                obsidian_sources.join(",")
            },
            session
                .cfg
                .file
                .obsidian
                .context_limit_chars
                .saturating_add(3)
                / 4,
            obsidian_tokens,
        );
    }
    crate::graph::trace_start_in(
        &run_context,
        binding.class.as_str(),
        &binding.profile_name,
        &binding.provider_name,
        &binding.model,
        obsidian_on,
    );
    applog::debug(&format!(
        "bind source={} ov_provider={:?} ov_model={:?} sticky={:?} mode={} class={} profile={} provider={} model={}",
        if ov_provider.is_some() {
            "override"
        } else {
            "auto"
        },
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

    match run_pipeline_with_context(
        &session.cfg,
        &binding,
        &task,
        session.yes,
        session.provider.as_deref(),
        Some(resume),
        None,
        local_ask,
        run_context.clone(),
    )
    .await
    {
        Ok(outcome) => {
            let answer = final_assistant_answer(&outcome);
            let summary = CompletionSummary::from_outcome(
                &outcome,
                &crate::tools_more::current_todos_in(&run_context),
                &binding.provider_name,
                &binding.model,
                auto_compacted,
                obsidian_on,
                memory_sources,
            );
            agent::record_finish(&db, &run_id, &outcome)?;
            crate::graph::node_in(&run_context, "persist", &outcome.status, "", Some("bind"));
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
            if outcome.status == "ok" {
                if session.provider.is_none() && session.model.is_none() {
                    session.sticky = Some((binding.provider_name.clone(), binding.model.clone()));
                }
                persist_last_choice(&session.cfg.clone(), &binding.provider_name, &binding.model);
            } else if session.provider.is_none() && session.model.is_none() {
                session.sticky = None;
            }
            // 세션 자동 저장 (pi 스타일) — 턴 태스크(백그라운드)에서 실행되므로
            // 큰 세션 직렬화가 TUI 메인 루프를 멈추지 않는다.
            let _ = save_if_dirty(session);
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
                todos: crate::tools_more::current_todos_in(&run_context),
                elapsed_ms: started.elapsed().as_millis() as u64,
                answer,
                summary,
                lifecycle_state: run_context.lifecycle_state(),
                lifecycle: run_context.lifecycle_events(),
                context_sources: run_context.context_sources(),
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
                verify_recovered: None,
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
    /// 모델이 사용자의 요청에 직접 답한 최종 Markdown 본문.
    pub answer: String,
    /// 턴 종료 시점의 todo 목록 — ulw 기준별 판정(F4 개선)에 쓴다.
    pub todos: Vec<crate::tools_more::TodoItem>,
    pub summary: CompletionSummary,
    pub lifecycle_state: Option<crate::lifecycle::LifecycleState>,
    pub lifecycle: Vec<crate::lifecycle::LifecycleEvent>,
    pub context_sources: Vec<crate::run::ContextSourceRecord>,
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
    fn new_session_keeps_last_choice_instead_of_hardcoded_model() {
        let cfg = Config::load(None).expect("config");
        let pair = default_new_session_pair(&cfg);
        // last_provider/last_model 이 있으면 그 조합, 없으면 None (Harness 자동).
        let lp = cfg.file.general.last_provider.trim();
        if lp.is_empty() || !cfg.file.providers.contains_key(lp) {
            assert!(pair.is_none());
        } else {
            assert_eq!(pair.map(|(p, _)| p), Some(lp.to_string()));
        }
    }

    #[test]
    fn persisted_choice_is_a_soft_sticky_selection() {
        let dir = std::env::temp_dir().join(format!("rafikx-sticky-{}", Db::new_id()));
        let mut cfg = Config::load(Some(&dir.join("config.toml"))).expect("config");
        cfg.file.general.last_provider = "soft".into();
        cfg.file.general.last_model = "soft-model".into();
        cfg.file.providers.insert(
            "soft".into(),
            crate::config::ProviderConfig {
                kind: "openai_compat".into(),
                auth: "api_key".into(),
                api_key_env: "SOFT_KEY".into(),
                model: "soft-model".into(),
                small_model: None,
                base_url: None,
                supports_tools: true,
                models_url: None,
                model_auto: false,
                context_window: None,
                enabled: true,
            },
        );
        let session = seed_default_choice(Session {
            cfg,
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
        });

        assert!(session.provider.is_none());
        assert!(session.model.is_none());
        assert_eq!(session.sticky, Some(("soft".into(), "soft-model".into())));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn completion_summary_uses_observed_outcome_only() {
        let outcome = crate::agent::AgentOutcome {
            status: "ok".into(),
            iterations: 4,
            changed_files: vec!["src/main.rs".into()],
            ..Default::default()
        };
        let summary = CompletionSummary::from_outcome(
            &outcome,
            &[],
            "minimax",
            "minimax-m3",
            false,
            false,
            0,
        );
        assert_eq!(summary.changed_files, vec!["src/main.rs"]);
        assert_eq!(summary.iterations, 4);
    }

    #[test]
    fn final_answer_preserves_table_and_model_identity() {
        let answer =
            "| model | provider |\n|---|---|\n| minimax-m3 | minimax |\n\n모델은 minimax-m3입니다.";
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "tool-1".into(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: format!("<think>내부 추론</think>\n{answer}"),
                }],
            },
        ];

        assert_eq!(
            final_assistant_answer(&agent::AgentOutcome {
                messages,
                ..Default::default()
            }),
            answer
        );
    }

    #[test]
    fn failed_turn_does_not_reexpose_a_provisional_answer() {
        let outcome = agent::AgentOutcome {
            status: "fail".into(),
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "검증 전에만 유효했던 후보".into(),
                }],
            }],
            ..Default::default()
        };

        assert!(final_assistant_answer(&outcome).is_empty());
    }

    #[test]
    fn selfharness_slash_toggles_meta_and_persists() {
        let dir = std::env::temp_dir().join(format!("rafikx-shmeta-{}", Db::new_id()));
        let config_path = dir.join("config.toml");
        let cfg = Config::load(Some(&config_path)).expect("config");
        // 새 config 기본값은 off.
        assert!(!cfg.file.self_harness.meta);
        let mut s = Session {
            cfg,
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

        // 인자 없음 — 현재 상태를 보여주고 아무것도 바꾸지 않는다.
        let out = handle_slash(&mut s, "/selfharness", false).expect("ok");
        match out {
            Slash::Continue(notes) => {
                assert!(notes.iter().any(|n| n.contains("Self-Harness 메타: off")));
                assert!(notes.iter().any(|n| n.contains("legacy engine=self")));
            }
            _ => panic!("expected Continue"),
        }
        assert!(!s.cfg.file.self_harness.meta);

        // on — 세션과 config 양쪽에 반영된다 ([self_harness] 섹션 생성 포함).
        handle_slash(&mut s, "/selfharness on", false).expect("ok");
        assert!(s.cfg.file.self_harness.meta);
        assert!(
            Config::load(Some(&config_path))
                .expect("reload meta on")
                .file
                .self_harness
                .meta
        );

        // off — 되돌린다.
        handle_slash(&mut s, "/selfharness off", false).expect("ok");
        assert!(!s.cfg.file.self_harness.meta);
        assert!(
            !Config::load(Some(&config_path))
                .expect("reload meta off")
                .file
                .self_harness
                .meta
        );

        // 잘못된 인자는 사용법만 안내한다.
        match handle_slash(&mut s, "/selfharness maybe", false).expect("ok") {
            Slash::Continue(notes) => {
                assert!(notes.iter().any(|n| n.contains("/selfharness on|off")))
            }
            _ => panic!("expected Continue"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discipline_slash_switches_and_persists() {
        let dir = std::env::temp_dir().join(format!("rafikx-discipline-{}", Db::new_id()));
        let config_path = dir.join("config.toml");
        let cfg = Config::load(Some(&config_path)).expect("config");
        // 새 config 기본값은 harness.
        assert_eq!(
            crate::engine::normalize_discipline(&cfg.file.general.discipline),
            crate::engine::Discipline::Harness
        );
        let mut s = Session {
            cfg,
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

        // 인자 없음 — 현재 값과 3종 설명만 보여주고 아무것도 바꾸지 않는다.
        match handle_slash(&mut s, "/discipline", false).expect("ok") {
            Slash::Continue(notes) => {
                assert!(notes.iter().any(|n| n.contains("현재 실행 분야: harness")));
                for name in ["harness", "loop", "graph"] {
                    assert!(
                        notes.iter().any(|n| n.trim_start().starts_with(name)),
                        "{name} 설명 없음"
                    );
                }
            }
            _ => panic!("expected Continue"),
        }
        assert_eq!(s.cfg.file.general.discipline, "harness");

        // 값 지정 — 세션과 config 양쪽에 반영된다.
        handle_slash(&mut s, "/discipline graph", false).expect("ok");
        assert_eq!(s.cfg.file.general.discipline, "graph");
        assert_eq!(
            Config::load(Some(&config_path))
                .expect("reload graph")
                .file
                .general
                .discipline,
            "graph"
        );

        handle_slash(&mut s, "/discipline LOOP", false).expect("ok");
        assert_eq!(s.cfg.file.general.discipline, "loop");
        assert_eq!(
            Config::load(Some(&config_path))
                .expect("reload loop")
                .file
                .general
                .discipline,
            "loop"
        );

        // 미지원 값은 저장하지 않고 사용법만 안내한다.
        match handle_slash(&mut s, "/discipline dag", false).expect("ok") {
            Slash::Continue(notes) => {
                assert!(notes.iter().any(|n| n.contains("harness|loop|graph")))
            }
            _ => panic!("expected Continue"),
        }
        assert_eq!(s.cfg.file.general.discipline, "loop");

        // 팔레트에 등록되어 있어야 TUI 자동완성에 뜬다.
        assert!(SLASH_COMMANDS.iter().any(|(c, _)| *c == "/discipline"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_slash_clears_session_and_signals_screen_reset() {
        let dir = std::env::temp_dir().join(format!("rafikx-new-{}", Db::new_id()));
        let config_path = dir.join("config.toml");
        let cfg = Config::load(Some(&config_path)).expect("config");
        let mut s = Session {
            cfg,
            yes: true,
            provider: None,
            model: None,
            class: None,
            mode: "build".into(),
            session_id: Some("old".into()),
            messages: vec![crate::provider::Message::user_text("이전 대화")],
            last_context_tokens: 0,
            obsidian_on: false,
            attachments: vec![],
            dirty: true,
            sticky: Some(("p".into(), "m".into())),
        };
        // /new 는 Continue 가 아니라 New 를 돌려줘야 TUI 가 화면을 지운다.
        match handle_slash(&mut s, "/new", false).expect("ok") {
            Slash::New(notes) => {
                assert!(notes.iter().any(|n| n.contains("새 세션")));
            }
            other => panic!("Slash::New 여야 한다: {other:?}"),
        }
        assert!(s.messages.is_empty());
        assert!(s.session_id.is_none());
        assert!(s.sticky.is_none());
        assert!(!s.dirty);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn team_slash_switches_and_persists() {
        let dir = std::env::temp_dir().join(format!("rafikx-team-{}", Db::new_id()));
        let config_path = dir.join("config.toml");
        let cfg = Config::load(Some(&config_path)).expect("config");
        // 새 config 기본값은 single.
        assert_eq!(
            crate::harness::team_mode(&cfg),
            crate::engine::TeamMode::Single
        );
        let mut s = Session {
            cfg,
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

        // 인자 없음 — 현재 값과 2종 설명만 보여주고 아무것도 바꾸지 않는다.
        match handle_slash(&mut s, "/team", false).expect("ok") {
            Slash::Continue(notes) => {
                assert!(notes.iter().any(|n| n.contains("현재 팀 모드: single")));
                for name in ["single", "multi"] {
                    assert!(
                        notes.iter().any(|n| n.trim_start().starts_with(name)),
                        "{name} 설명 없음"
                    );
                }
            }
            _ => panic!("expected Continue"),
        }
        assert_eq!(s.cfg.file.harness.team, "single");

        // 값 지정 — 세션과 config 양쪽에 반영된다.
        handle_slash(&mut s, "/team MULTI", false).expect("ok");
        assert_eq!(s.cfg.file.harness.team, "multi");
        assert_eq!(
            Config::load(Some(&config_path))
                .expect("reload multi")
                .file
                .harness
                .team,
            "multi"
        );

        handle_slash(&mut s, "/team single", false).expect("ok");
        assert_eq!(s.cfg.file.harness.team, "single");
        assert_eq!(
            Config::load(Some(&config_path))
                .expect("reload single")
                .file
                .harness
                .team,
            "single"
        );

        // 미지원 값은 저장하지 않고 사용법만 안내한다.
        match handle_slash(&mut s, "/team duo", false).expect("ok") {
            Slash::Continue(notes) => assert!(notes.iter().any(|n| n.contains("single|multi"))),
            _ => panic!("expected Continue"),
        }
        assert_eq!(s.cfg.file.harness.team, "single");

        // 팔레트에 등록되어 있어야 TUI 자동완성에 뜬다.
        assert!(SLASH_COMMANDS.iter().any(|(c, _)| *c == "/team"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_slash_end_to_end() {
        let dir = std::env::temp_dir().join(format!("rafikx-engine-{}", Db::new_id()));
        let config_path = dir.join("config.toml");
        let cfg_path = Config::load(Some(&config_path)).expect("config");
        let original_engine = {
            let e = cfg_path.file.general.engine.trim().to_ascii_lowercase();
            if is_valid_engine(&e) {
                e
            } else {
                "rafikx".to_string()
            }
        };
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

        // deepseek 엔진 — 정상 저장되어야 한다.
        let out = handle_slash(&mut s, "/engine deepseek", false).expect("ok");
        assert!(matches!(out, Slash::Continue(_)));
        assert_eq!(s.cfg.file.general.engine, "deepseek");

        // self 엔진 — 저장되고 Self-Harness 상태 요약이 함께 나와야 한다.
        let out = handle_slash(&mut s, "/engine self", false).expect("ok");
        match out {
            Slash::Continue(notes) => {
                assert!(notes.iter().any(|n| n.contains("self-harness")));
                assert!(notes.iter().any(|n| n.contains("Self-Harness v")));
            }
            _ => panic!("expected Continue"),
        }
        assert_eq!(s.cfg.file.general.engine, "self");
        assert_eq!(
            Config::load(Some(&config_path))
                .expect("reload self engine")
                .file
                .general
                .engine,
            "self"
        );

        // 미지원 값 — 카탈로그 목록을 담은 거부 안내를 반환한다.
        let out = handle_slash(&mut s, "/engine nope", false).expect("ok");
        match out {
            Slash::Continue(notes) => {
                assert!(
                    notes
                        .iter()
                        .any(|n| n.contains(&format!("{}|self", crate::engine::names_joined())))
                );
            }
            _ => panic!("expected Continue"),
        }

        // 사용자가 쓰던 원래 엔진으로 복원해 테스트 흔적을 남기지 않는다.
        let _ = handle_slash(&mut s, &format!("/engine {original_engine}"), false);
        assert_eq!(s.cfg.file.general.engine, original_engine);
        assert_eq!(
            Config::load(Some(&config_path))
                .expect("reload restored engine")
                .file
                .general
                .engine,
            original_engine
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn engine_slash_accepts_known_engines_only() {
        assert!(is_valid_engine("rafikx"));
        assert!(is_valid_engine("claude"));
        assert!(is_valid_engine("deepseek"));
        assert!(is_valid_engine("qwen"));
        assert!(is_valid_engine("kimi"));
        assert!(is_valid_engine("pi"));
        assert!(is_valid_engine("self"));
        // 오타·미지원·제거된 값은 거부해야 한다.
        assert!(!is_valid_engine("dk")); // deepseek 로 통합되어 제거됨
        assert!(!is_valid_engine("dkharness"));
        assert!(!is_valid_engine("selfharness"));
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
            loaded_at: None,
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

    fn sample_regs() -> Vec<crate::auth::RegisteredModel> {
        vec![
            crate::auth::RegisteredModel {
                provider: "anthropic".into(),
                id: "claude-sonnet-4-6".into(),
                small: false,
            },
            crate::auth::RegisteredModel {
                provider: "minimax".into(),
                id: "minimax-m3".into(),
                small: false,
            },
            crate::auth::RegisteredModel {
                provider: "minimax".into(),
                id: "minimax-m2".into(),
                small: true,
            },
        ]
    }

    #[test]
    fn model_refresh_aliases_route_to_remote_fetch() {
        let dir = std::env::temp_dir().join(format!("rafikx-modelfetch-{}", Db::new_id()));
        let config_path = dir.join("config.toml");
        let cfg = Config::load(Some(&config_path)).expect("config");
        let mut s = Session {
            cfg,
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

        for line in [
            "/model refresh",
            "/models fetch",
            "/model 새로고침",
            "/model -r",
            "/model REFRESH",
        ] {
            match handle_slash(&mut s, line, false).expect("ok") {
                Slash::ModelFetch { query, fetch } => {
                    assert!(fetch, "{line} 는 원격 조회여야 한다");
                    assert!(query.is_empty(), "{line} 에는 검색어가 없다");
                }
                other => panic!("{line}: ModelFetch 를 기대했는데 {other:?}"),
            }
        }

        // 번호 선택은 조회 경로로 새지 않는다 — 기존 apply_model_choice 그대로.
        let out = handle_slash(&mut s, "/model 3", false).expect("ok");
        assert!(!matches!(out, Slash::ModelFetch { .. }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_arg_is_query_only_for_partial_matches() {
        let regs = sample_regs();
        // 목록에 부분 일치하는 자유 텍스트만 검색어다.
        assert!(model_arg_is_query(&regs, "claude"));
        assert!(model_arg_is_query(&regs, "MINIMAX"), "대소문자 무시");
        // 번호·정확한 id·목록에 없는 직접 지정은 기존 선택 경로.
        assert!(!model_arg_is_query(&regs, "2"));
        assert!(!model_arg_is_query(&regs, "claude-sonnet-4-6"));
        assert!(!model_arg_is_query(&regs, "gpt-9-preview"));
        assert!(!model_arg_is_query(&regs, ""));
        // refresh 별칭은 검색어보다 먼저 걸린다.
        assert!(is_model_refresh_arg("refresh"));
        assert!(is_model_refresh_arg("새로고침"));
        assert!(!is_model_refresh_arg("claude"));
    }

    #[test]
    fn model_list_notes_filters_but_keeps_original_numbers() {
        let regs = sample_regs();
        let all = model_list_notes(&regs, "");
        assert_eq!(all[0], "등록된 모델:");
        assert_eq!(all[1], "  [1] anthropic / claude-sonnet-4-6");
        assert_eq!(all.len(), 5); // 머리 + 3줄 + 안내

        let hit = model_list_notes(&regs, "minimax-m2");
        assert!(hit[0].contains("검색"));
        // 걸러도 번호는 전체 목록 기준이라 그대로 /model 3 으로 고를 수 있다.
        assert!(hit.iter().any(|l| l == "  [3] minimax / minimax-m2"));
        assert!(!hit.iter().any(|l| l.contains("claude")));

        let miss = model_list_notes(&regs, "없는모델");
        assert!(miss.iter().any(|l| l.contains("일치하는 모델이 없습니다")));
    }
}

// ---------------------------------------------------------------------------
// /ulw 자율 완수 루프 (F4) — 계획 → 실행 → 증거 판정 → 재촉/완료/중단
// ---------------------------------------------------------------------------

/// 경량 계획 호출 — 완료 기준 체크리스트만 뽑는다 (실행 계획은 파이프라인이 다시 세운다).
async fn ulw_plan(cfg: &crate::config::Config, goal: &str) -> Result<String> {
    let order = crate::harness::fallback_order(cfg, &cfg.file.general.default_provider, None);
    let req = crate::provider::ChatRequest {
        model: String::new(),
        system: "목표 완수에 필요한 완료 기준 체크리스트만 출력하라. 3~7개, 각 항목은 '- '로 시작하는 한 줄, \
                 관측 가능한 결과(파일·명령·동작)여야 한다. 다른 텍스트는 출력하지 마라."
            .into(),
        messages: vec![Message::user_text(goal)],
        tools: vec![],
        max_tokens: 800,
        stream: false,
    };
    let (_name, resp) = crate::harness::chat_with_fallback(cfg, &order, "main", req).await?;
    Ok(resp
        .content
        .iter()
        .find_map(|b| match b {
            crate::provider::ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default())
}

/// 목표를 facts(kind='goal')에 기록/갱신 — 세션이 갈려도 목표가 유실되지 않는다.
fn ulw_record_goal_fact(session: &Session, state: &crate::ulw::UlwState, status: &str) {
    let Ok(path) = Db::db_path() else { return };
    if let Ok(db) = Db::open(&path) {
        let _ = db.upsert_fact(
            Some(&session.cfg.workspace),
            "goal",
            &format!("ulw:{}", state.run_id),
            &format!("[{status}] {}", state.goal),
            "agent",
        );
    }
}

fn ulw_summary_of(info: &TurnInfo) -> crate::ulw::RunSummaryLite {
    crate::ulw::RunSummaryLite {
        changed_files: info.summary.changed_files.clone(),
        completed_todos: info.summary.completed_todos,
        total_todos: info.summary.total_todos,
        iterations: info.summary.iterations,
        tool_errors: info.summary.tool_errors,
        answer_tail: info.answer.chars().take(200).collect(),
        todos: info.todos.iter().map(|td| (td.content.clone(), td.status.clone())).collect(),
        verify_ran: false,
        verify_ok: false,
        verify_tail: String::new(),
    }
}

/// 테스트 명령 감지 (F4b) — 있으면 빌드 검사를 통과한 뒤 함께 실행한다.
fn ulw_detect_test_command(workspace: &std::path::Path) -> String {
    if workspace.join("Cargo.toml").exists() {
        return "cargo test --quiet".into();
    }
    if workspace.join("pytest.ini").exists()
        || workspace.join("tests").is_dir() && workspace.join("pyproject.toml").exists()
    {
        return "python3 -m pytest -q".into();
    }
    if workspace.join("package.json").exists()
        && std::fs::read_to_string(workspace.join("package.json"))
            .map(|s| s.contains("\"test\""))
            .unwrap_or(false)
    {
        return "npm test --silent".into();
    }
    String::new()
}

/// ulw 가 에이전트 주장과 독립으로 검증을 직접 실행한다 (mandela 원칙 — 외부 근거가
/// 루프에 들어와야 한다). 빌드 수준 명령(auto_verify_command) → 통과 시 테스트 명령.
/// 반환: (실행 여부, 통과 여부, 실패 출력 꼬리)
async fn ulw_run_verification(
    cfg: &crate::config::Config,
    changed: &[String],
) -> (bool, bool, String) {
    if changed.is_empty() {
        return (false, true, String::new());
    }
    let mut commands: Vec<String> = Vec::new();
    let build = crate::harness::auto_verify_command(cfg, changed);
    if !build.is_empty() {
        commands.push(build);
    }
    let test = ulw_detect_test_command(&cfg.workspace);
    if !test.is_empty() && !commands.contains(&test) {
        commands.push(test);
    }
    if commands.is_empty() {
        return (false, true, String::new());
    }
    let mut ran_any = false;
    for cmd in commands {
        ran_any = true;
        let shell = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
        let child = tokio::process::Command::new(shell.0)
            .arg(shell.1)
            .arg(&cmd)
            .current_dir(&cfg.workspace)
            .output();
        let output = match tokio::time::timeout(std::time::Duration::from_secs(180), child).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return (true, false, format!("검증 명령 실행 실패 ({cmd}): {e}")),
            Err(_) => return (true, false, format!("검증 명령 타임아웃 (180초): {cmd}")),
        };
        if !output.status.success() {
            let mut tail = String::from_utf8_lossy(&output.stderr).to_string();
            if tail.trim().is_empty() {
                tail = String::from_utf8_lossy(&output.stdout).to_string();
            }
            let tail: String = tail.chars().rev().take(400).collect::<String>().chars().rev().collect();
            return (true, false, format!("{cmd}\n{tail}"));
        }
    }
    (ran_any, true, String::new())
}

async fn ulw_finish(
    session: &Session,
    state: &crate::ulw::UlwState,
    status: &str,
    headline: &str,
) -> Result<()> {
    ulw_record_goal_fact(session, state, status);
    #[cfg(feature = "telegram")]
    crate::telegram::notify_owner(
        &session.cfg,
        &format!("[ulw {status}] {}\n{}", state.goal, headline),
    )
    .await;
    Ok(())
}

async fn ulw_loop(session: &mut Session, goal: &str, state: crate::ulw::UlwState) -> Result<TurnInfo> {
    ulw_loop_observed(session, goal, state, None, None).await
}

pub async fn ulw_loop_observed(
    session: &mut Session,
    goal: &str,
    mut state: crate::ulw::UlwState,
    observer: Option<RunObserver>,
    local_ask: Option<LocalAsk>,
) -> Result<TurnInfo> {
    let workspace = session.cfg.workspace.clone();
    let fresh = state.runs == 0 && state.criteria.is_empty();
    if fresh {
        crate::ui::live_line(&format!("[ulw] {} — 계획 수립 중…", state.run_id));
        let plan = ulw_plan(&session.cfg, goal).await.unwrap_or_default();
        let mut items = crate::ulw::UlwState::parse_criteria_lines(&plan);
        if items.is_empty() {
            items = vec![goal.trim().to_string()];
        }
        state.set_criteria(&workspace, &plan, items)?;
        ulw_record_goal_fact(session, &state, "running");
    }
    crate::ui::live_line(&format!(
        "[ulw] {} — 완료 기준 {}개 · 실행 상한 {}회",
        state.run_id,
        state.criteria.len(),
        crate::ulw::MAX_RUNS
    ));
    let last_info = loop {
        let prompt = if state.runs == 0 {
            state.kickoff_task()
        } else {
            state.nudge_message()
        };
        let info = match run_turn_observed(
            session,
            &prompt,
            Some("dev"),
            false,
            local_ask.clone(),
            observer.clone(),
        )
        .await
        {
            Ok(info) => info,
            Err(e) => {
                state.status = "blocked".into();
                state.blocked_reason = format!("실행 오류: {e:#}");
                state.save(&workspace)?;
                state.write_report(&workspace, "실행 오류로 중단")?;
                ulw_finish(session, &state, "blocked", &state.blocked_reason).await?;
                return Err(e);
            }
        };
        let mut summary = ulw_summary_of(&info);
        let (v_ran, v_ok, v_tail) = ulw_run_verification(&session.cfg, &summary.changed_files).await;
        summary.verify_ran = v_ran;
        summary.verify_ok = v_ok;
        summary.verify_tail = v_tail;
        match state.record_run(&workspace, &summary)? {
            crate::ulw::UlwVerdict::Done => {
                state.write_report(&workspace, "")?;
                crate::ui::live_line(&format!("[ulw] 완료 — .omo/ulw/{}/report.md", state.run_id));
                ulw_finish(session, &state, "done", "모든 완료 기준 충족").await?;
                break info;
            }
            crate::ulw::UlwVerdict::Continue => {
                crate::ui::live_line(&format!(
                    "[ulw] 미완료 기준 {}개 — 재촉 {}/{}",
                    state.unmet().len(),
                    state.nudges,
                    crate::ulw::MAX_NUDGES
                ));
            }
            crate::ulw::UlwVerdict::Blocked(reason) => {
                state.write_report(&workspace, "미달 기준 있음")?;
                crate::ui::live_line(&format!("[ulw] 중단: {reason} — .omo/ulw/{}/report.md", state.run_id));
                ulw_finish(session, &state, "blocked", &reason).await?;
                break info;
            }
        }
    };
    Ok(last_info)
}

async fn ulw_resume(session: &mut Session, run_id: Option<String>) -> Result<TurnInfo> {
    ulw_resume_observed(session, run_id, None, None).await
}

pub async fn ulw_resume_observed(
    session: &mut Session,
    run_id: Option<String>,
    observer: Option<RunObserver>,
    local_ask: Option<LocalAsk>,
) -> Result<TurnInfo> {
    let workspace = session.cfg.workspace.clone();
    let id = match run_id {
        Some(id) => id,
        None => crate::ulw::UlwState::latest_id(&workspace)
            .ok_or_else(|| anyhow::anyhow!("재개할 ulw 실행이 없습니다 (.omo/ulw/ 비어 있음)"))?,
    };
    let state = crate::ulw::UlwState::load(&workspace, &id)?;
    match state.status.as_str() {
        "done" => {
            crate::ui::live_line(&format!("[ulw] {id} — 이미 완료된 실행입니다. .omo/ulw/{id}/report.md"));
            anyhow::bail!("이미 완료된 실행입니다");
        }
        "blocked" => {
            crate::ui::live_line(&format!("[ulw] {id} — 중단된 실행을 재개합니다: {}", state.blocked_reason));
        }
        _ => crate::ui::live_line(&format!("[ulw] {id} — 미완료 기준 {}개부터 재개합니다.", state.unmet().len())),
    }
    // 재개는 새 활성화로 본다 — 재촉 카운트는 유지하되 상태만 running 으로.
    let mut state = state;
    state.status = "running".into();
    state.save(&workspace)?;
    let goal = state.goal.clone();
    ulw_loop_observed(session, &goal, state, observer, local_ask).await
}
