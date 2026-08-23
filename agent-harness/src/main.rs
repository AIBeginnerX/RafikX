use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

use rafikx::accounts;
use rafikx::agent;
use rafikx::applog;
use rafikx::auth;
use rafikx::chat;
use rafikx::config::{self, Config};
use rafikx::db::Db;
use rafikx::graph;
use rafikx::harness::{bind, classify, ping_provider, print_binding, print_binding_table, run_pipeline};
use rafikx::inspector;
use rafikx::lessons;
use rafikx::obsidian;
use rafikx::ranks;
use rafikx::settings;
use rafikx::ui;
#[cfg(feature = "telegram")]
use rafikx::telegram;
#[cfg(feature = "tui")]
use rafikx::tui;

#[derive(Parser)]
#[command(
    name = "RafikX",
    bin_name = "rafikx",
    version,
    about = "RafikX — 개인용 AI 코딩 에이전트. 인자 없이 실행하면 대화 화면을 엽니다.",
    subcommand_required = false,
    arg_required_else_help = false
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    provider: Option<String>,
    #[arg(long, global = true)]
    model: Option<String>,
    #[arg(long, global = true)]
    class: Option<String>,
    #[arg(long, global = true)]
    yes: bool,
    #[command(subcommand)]
    cmd: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 분류기 → 서브에이전트 자동 실행
    Ask {
        /// 지시문
        prompt: String,
        /// Vault 검색 결과를 컨텍스트로 주입
        #[arg(long)]
        obsidian: bool,
    },
    /// = ask --class dev
    Agent {
        /// 작업 지시
        prompt: String,
    },
    /// Vault 노트를 FTS5에 인덱싱
    Index,
    /// Vault 노트 검색
    Search {
        query: String,
    },
    /// Vault 파일 변경을 감시하며 재인덱싱
    Watch,
    /// 대화 화면 (TTY). --list 는 세션 목록
    Chat {
        #[arg(long)]
        list: bool,
        #[arg(long)]
        resume: Option<String>,
    },
    /// 교훈 관리
    Lessons {
        #[command(subcommand)]
        action: LessonsCmd,
    },
    /// 점검 리포트
    Inspect {
        /// 최근 N건 (기본 200)
        #[arg(long, default_value_t = 200)]
        last: u32,
        /// 제안 교훈을 lessons에 저장
        #[arg(long)]
        apply: bool,
        /// 점검에 쓸 서브에이전트 이름
        #[arg(long)]
        subagent: Option<String>,
    },
    /// 마지막 점검 리포트
    Report {
        #[command(subcommand)]
        action: ReportCmd,
    },
    /// 상태 점검
    Doctor,
    /// 서비스 연결 (키·로그인)
    #[command(alias = "연결")]
    Login,
    /// 설정 (연결·모델·하네스·텔레그램·옵시디언·순위)
    #[command(alias = "설정")]
    Settings,
    /// 모델 순위표
    Ranks {
        #[command(subcommand)]
        action: Option<RanksCmd>,
    },
    /// 텔레그램 봇 + Inspector 스케줄
    Telegram {
        /// Obsidian Vault 감시(watch)를 함께 켭니다
        #[arg(long)]
        with_watch: bool,
    },
}

#[derive(Subcommand)]
enum LessonsCmd {
    /// 저장된 교훈 목록
    List,
    /// 교훈을 직접 추가
    Add { text: String },
    /// id로 삭제
    Rm { id: i64 },
    /// 모두 삭제
    Clear,
}

#[derive(Subcommand)]
enum ReportCmd {
    /// 마지막 리포트 다시 보기
    Last,
}

#[derive(Subcommand)]
enum RanksCmd {
    /// 지금 갱신 (가능하면 안정 JSON, 실패 시 번들 유지)
    Update,
    /// 기준일·상위 항목
    Status,
}

#[tokio::main]
async fn main() -> ExitCode {
    ui::init();
    let cli = Cli::parse();
    let result = match &cli.cmd {
        None => cmd_default(&cli).await,
        Some(Commands::Doctor) => cmd_doctor(&cli).await,
        Some(Commands::Login) => cmd_login(&cli).await,
        Some(Commands::Settings) => cmd_settings(&cli).await,
        Some(Commands::Ranks { action }) => cmd_ranks(action.as_ref()).await,
        Some(Commands::Ask { prompt, obsidian }) => cmd_ask(&cli, prompt, *obsidian).await,
        Some(Commands::Agent { prompt }) => cmd_ask(&cli, prompt, false).await,
        Some(Commands::Index) => cmd_index(&cli),
        Some(Commands::Search { query }) => cmd_search(&cli, query),
        Some(Commands::Watch) => cmd_watch(&cli).await,
        Some(Commands::Chat { list, resume }) => {
            cmd_chat_entry(&cli, *list, resume.clone()).await
        }
        Some(Commands::Lessons { action }) => cmd_lessons(&cli, action),
        Some(Commands::Inspect {
            last,
            apply,
            subagent,
        }) => cmd_inspect_entry(&cli, *last, *apply, subagent.as_deref()).await,
        Some(Commands::Report { action }) => match action {
            ReportCmd::Last => inspector::cmd_report_last(),
        },
        Some(Commands::Telegram { with_watch }) => cmd_telegram(&cli, *with_watch).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            applog::error(&format!("{err:#}"));
            eprintln!("오류: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn cmd_login(cli: &Cli) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("연결은 터미널에서 합니다. rafikx login");
    }
    settings::cmd_login(cfg).await
}

async fn cmd_settings(cli: &Cli) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("설정은 터미널에서 고릅니다. rafikx settings");
    }
    settings::cmd_settings(cfg).await
}

async fn cmd_ranks(action: Option<&RanksCmd>) -> Result<()> {
    match action {
        Some(RanksCmd::Update) => settings::cmd_ranks_update().await,
        Some(RanksCmd::Status) | None => {
            settings::cmd_ranks_status();
            Ok(())
        }
    }
}

async fn cmd_doctor(cli: &Cli) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let mut ok = true;

    ui::banner("상태 점검");
    println!();

    for note in auth::maintain_accounts(&cfg)? {
        ui::note(&note);
    }
    let cfg = cfg.reload()?;

    ui::ok(&format!("설정 파일  {}", cfg.path.display()));

    if cfg.workspace.exists() {
        ui::ok(&format!("워크스페이스  {}", cfg.workspace.display()));
    } else {
        ui::warn(&format!("워크스페이스  {} (폴더가 아직 없습니다)", cfg.workspace.display()));
        ok = false;
    }

    let db_path = Db::db_path()?;
    match Db::open(&db_path) {
        Ok(db) => {
            ui::ok(&format!("DB  {}", db_path.display()));
            match db.has_fts5() {
                Ok(true) => ui::ok("FTS5  사용 가능"),
                Ok(false) => {
                    ui::fail("FTS5  rusqlite bundled 에 FTS5가 없습니다");
                    ok = false;
                }
                Err(e) => {
                    ui::fail(&format!("FTS5 확인  {e}"));
                    ok = false;
                }
            }
        }
        Err(e) => {
            ui::fail(&format!("DB  {e}"));
            ok = false;
        }
    }

    let vault = config::expand_tilde(&cfg.file.obsidian.vault_path);
    if vault.exists() {
        ui::ok(&format!("Vault  {}", vault.display()));
    } else {
        ui::warn(&format!("Vault  {} (폴더가 아직 없습니다. index 시 생성됩니다)", vault.display()));
    }
    ui::ok(&format!(
        "tokenizer {}  enabled={}",
        cfg.file.obsidian.tokenizer, cfg.file.obsidian.enabled
    ));
    ui::ok(&format!(
        "하네스  {}",
        if cfg.file.harness.selection.eq_ignore_ascii_case("manual") {
            "수동"
        } else {
            "자동"
        }
    ));
    ui::ok(&ranks::status_line());

    ui::section(&format!("서비스  (기본={})", cfg.file.general.default_provider));
    ui::note("키: 환경변수 또는 secrets.toml. Zen=OPENCODE_API_KEY  Go=OPENCODE_GO_API_KEY");
    let mut names: Vec<String> = cfg.file.providers.keys().cloned().collect();
    names.sort_by_key(|n| auth::provider_sort_key(n));
    let mut any_ready = false;
    for name in &names {
        let Ok(p) = cfg.provider(name) else { continue };
        let mode = auth::auth_mode(name, p);
        let rows = auth::list_account_rows(&cfg, name);
        if rows.is_empty() {
            if mode == "none" {
                ui::ok(&format!("{name:14}  로컬 · 키 없음"));
                any_ready = true;
            } else {
                ui::note(&format!(
                    "{name:14}  미연결  ·  {}",
                    auth::env_hint(&cfg, name)
                ));
            }
        } else {
            any_ready = true;
            ui::ok(&format!(
                "{name:14}  {}  모델={}",
                mode,
                p.model
            ));
            for (a, live, tail) in rows {
                if live {
                    ui::note(&format!("{}  ****{tail}", accounts::display(&a)));
                } else {
                    ui::warn(&format!("{}  만료", accounts::display(&a)));
                }
            }
        }
        if auth::is_usable(&cfg, name) {
            let models = auth::list_remote_models(&cfg, name).await?;
            if !models.is_empty() {
                let show: Vec<_> = models.iter().take(5).cloned().collect();
                ui::note(&format!(
                    "모델 {}{}",
                    show.join(", "),
                    if models.len() > 5 { " …" } else { "" }
                ));
            }
            ui::note(&ping_provider(&cfg, name).await);
        } else if auth::is_connected(&cfg, name) {
            ui::warn(&format!("{name:14}  사용 중지 (호출 안 함)"));
        }
    }
    if !auth::has_cloud_credential(&cfg) && !any_ready {
        ui::warn("연결된 클라우드 서비스가 없습니다. rafikx login  또는  환경변수 키.");
        ok = false;
    }

    print_binding_table(&cfg);

    println!();
    let tg = &cfg.file.telegram;
    match auth::telegram_token(&cfg)? {
        Some(t) if !t.trim().is_empty() => {
            let tail: String = t
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            ui::ok(&format!("telegram token  ****{tail}"));
        }
        _ => {
            ui::warn("telegram token  없음");
            ok = false;
        }
    }
    ui::ok(&format!(
        "telegram allowlist {:?}  allow_agent={}  timeout={}s  enabled={}",
        tg.allowed_user_ids, tg.allow_agent, tg.approval_timeout_secs, tg.enabled
    ));
    if tg.allowed_user_ids.is_empty() {
        ui::warn("telegram allowlist 가 비어 있습니다");
        ok = false;
    }

    if std::io::stdin().is_terminal() {
        println!();
        if ok {
            println!("모든 항목 OK (ping/바인딩은 위 표 참고)");
        } else {
            println!("주의 항목이 있습니다. 번호 메뉴에서 고르면 됩니다.");
        }
        return settings::cmd_doctor_interactive(cfg, true).await;
    }

    println!();
    if ok {
        println!("모든 항목 OK (ping/바인딩은 위 표 참고)");
    } else {
        println!("주의 항목이 있습니다. rafikx settings 에서 번호로 연결하세요.");
    }
    Ok(())
}

async fn cmd_ask(cli: &Cli, prompt: &str, obsidian: bool) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let cfg = settings::maybe_first_run(cfg, std::io::stdin().is_terminal()).await?;
    let mode = if matches!(cli.cmd, Some(Commands::Agent { .. })) {
        "agent"
    } else {
        "ask"
    };
    let forced = if mode == "agent" {
        Some("dev")
    } else {
        cli.class.as_deref()
    };
    let class = classify(&cfg, prompt, obsidian, forced).await?;
    let binding = bind(
        &cfg,
        class,
        cli.provider.as_deref(),
        cli.model.as_deref(),
    )?;
    print_binding(&binding);

    let mut task = prompt.to_string();
    if obsidian {
        if !cfg.file.obsidian.enabled {
            println!("[Obsidian] 꺼져 있습니다. rafikx settings 에서 켜세요.");
        } else {
        match obsidian::ask_context(&cfg, prompt) {
            Ok(ctx) => {
                if ctx.sources.is_empty() {
                    println!("[Obsidian] 검색 결과 없음 (index 를 먼저 실행하세요)");
                } else {
                    println!("[Obsidian] 출처:");
                    for s in &ctx.sources {
                        println!("  - {s}");
                    }
                }
                task = format!("{}\n\n(질문)\n{prompt}", ctx.block);
            }
            Err(e) => eprintln!("Obsidian 컨텍스트를 넣지 못했습니다: {e}"),
        }
        }
    }

    let db = Db::open(&Db::db_path()?)?;
    let run_id = db.start_run(
        mode,
        prompt,
        Some(binding.class.as_str()),
        Some(&binding.profile_name),
        Some(&binding.provider_name),
        Some(&binding.model),
    )?;
    let _g = graph::scope(&run_id);
    graph::trace_start(
        binding.class.as_str(),
        &binding.profile_name,
        &binding.provider_name,
        &binding.model,
        obsidian,
    );
    applog::info(&format!(
        "{mode} class={} profile={} provider={} model={}",
        binding.class.as_str(),
        binding.profile_name,
        binding.provider_name,
        binding.model
    ));

    match run_pipeline(
        &cfg,
        &binding,
        &task,
        cli.yes,
        cli.provider.as_deref(),
        None,
        None,
        None,
    )
    .await
    {
        Ok(outcome) => {
            agent::record_finish(&db, &run_id, &outcome)?;
            graph::node("persist", &outcome.status, "", Some("bind"));
            println!(
                "[run] class={} profile={} status={} iter={} tokens in={} out={}",
                binding.class.as_str(),
                binding.profile_name,
                outcome.status,
                outcome.iterations,
                outcome.input_tokens,
                outcome.output_tokens
            );
            lessons::maybe_spawn(&cfg, prompt, &outcome);
            ui::print_footer();
            Ok(())
        }
        Err(e) => {
            let _ = db.finish_run(&run_id, "fail", 0, 0, 0, Some(&e.to_string()));
            graph::node("persist", "fail", &e.to_string(), Some("bind"));
            let fail = agent::AgentOutcome {
                status: "fail".into(),
                error: Some(e.to_string()),
                ..Default::default()
            };
            lessons::maybe_spawn(&cfg, prompt, &fail);
            ui::print_footer();
            Err(e)
        }
    }
}

async fn cmd_inspect_entry(
    cli: &Cli,
    last: u32,
    apply: bool,
    subagent: Option<&str>,
) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    inspector::cmd_inspect(&cfg, last, apply, subagent).await
}

fn cmd_index(cli: &Cli) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let stats = obsidian::index_vault(&cfg)?;
    println!(
        "인덱싱 완료: 추가/갱신 {}개, 변경 없음 {}개, 삭제 {}개",
        stats.updated, stats.skipped, stats.deleted
    );
    Ok(())
}

fn cmd_search(cli: &Cli, query: &str) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    obsidian::search_print(&cfg, query)
}

async fn cmd_watch(cli: &Cli) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    obsidian::watch_vault(&cfg).await
}

fn cmd_lessons(cli: &Cli, action: &LessonsCmd) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    match action {
        LessonsCmd::List => lessons::cmd_list(&cfg),
        LessonsCmd::Add { text } => lessons::cmd_add(&cfg, text),
        LessonsCmd::Rm { id } => lessons::cmd_rm(*id),
        LessonsCmd::Clear => lessons::cmd_clear(),
    }
}

async fn cmd_telegram(cli: &Cli, with_watch: bool) -> Result<()> {
    #[cfg(feature = "telegram")]
    {
        telegram::run(cli.config.as_deref(), with_watch).await
    }
    #[cfg(not(feature = "telegram"))]
    {
        let _ = (cli, with_watch);
        anyhow::bail!(
            "이 RafikX는 텔레그램 없이 빌드되었습니다. 개발 폴더에서 cargo install --path . --force 로 다시 설치하세요."
        );
    }
}

async fn cmd_default(cli: &Cli) -> Result<()> {
    if ui::stdio_is_piped() {
        let mut cmd = Cli::command();
        cmd.print_help()?;
        println!();
        return Ok(());
    }
    cmd_chat_entry(cli, false, None).await
}

async fn cmd_chat_entry(cli: &Cli, list: bool, resume: Option<String>) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let interactive = ui::want_interactive_ui();
    let cfg = settings::maybe_first_run(cfg, interactive).await?;
    if list {
        return chat::cmd_chat(
            cfg,
            cli.yes,
            cli.provider.clone(),
            cli.model.clone(),
            cli.class.clone(),
            true,
            resume,
        )
        .await;
    }
    #[cfg(feature = "tui")]
    if interactive {
        match tui::run(
            cfg.clone(),
            cli.yes,
            cli.provider.clone(),
            cli.model.clone(),
            cli.class.clone(),
            resume.clone(),
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("화면을 열지 못했습니다 ({err:#}). 줄 단위 대화로 전환합니다.");
            }
        }
    }
    chat::cmd_chat(
        cfg,
        cli.yes,
        cli.provider.clone(),
        cli.model.clone(),
        cli.class.clone(),
        false,
        resume,
    )
    .await
}

