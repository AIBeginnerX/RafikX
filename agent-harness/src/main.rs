use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};

use rafikx::accounts;
use rafikx::agent;
use rafikx::applog;
use rafikx::auth;
use rafikx::chat;
use rafikx::config::{self, Config};
use rafikx::db::Db;
use rafikx::graph;
use rafikx::harness;
use rafikx::harness::{
    bind, classify, ping_provider, print_binding, print_binding_table, run_pipeline_with_context,
};
use rafikx::inspector;
use rafikx::lessons;
use rafikx::model_wizard;
use rafikx::obsidian;
use rafikx::ranks;
use rafikx::run::{RunContext, RunId};
use rafikx::settings;
#[cfg(feature = "telegram")]
use rafikx::telegram;
#[cfg(feature = "tui")]
use rafikx::tui;
use rafikx::ui;

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
    /// 최근 세션을 이어서 시작 (pi 의 -c)
    #[arg(short = 'c', long = "continue")]
    continue_last: bool,
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
    /// 기억한 지속 사실(전역+프로젝트) 목록
    Facts,
    /// 디렉터리별 AGENTS.md 초안 생성 (없는 파일만, 기존은 diff 제안)
    InitDeep,
    /// 계정·프로바이더 쿼터 상태
    Quota,
    /// 태스크 문서(JSON)의 검증 명령을 직접 실행해 증거를 남긴다 — 완료 판정은 이 결과만.
    VerifyTask {
        /// 태스크 문서 경로 (JSON)
        path: String,
    },
    /// 계획 문서(JSON)의 AC 커버리지 매트릭스를 검사한다 — 매핑 누락은 확정 거부.
    VerifyPlan {
        /// 계획 문서 경로 (JSON)
        path: String,
    },
    /// SPEC 문서(JSON)를 검증하고 동결한다 — 동결 후 변경은 재승인으로만.
    SpecFreeze {
        /// SPEC 문서 경로 (JSON)
        path: String,
    },
    /// 연결된 모델의 능력을 보정 스위트(9 프로브)로 측정한다 — 결과는 하네스가 쓴다.
    Calibrate {
        /// 프로바이더 (기본: 기본 연결)
        provider: Option<String>,
        /// 모델 (기본: 프로바이더 기본 모델)
        model: Option<String>,
    },
    /// LEDGER 원장을 집계해 실행 리포트를 출력한다.
    PlanReport {
        /// LEDGER.jsonl 경로
        path: String,
    },
    /// 태스크 체크포인트를 되돌린다 — git revert + 태스크 PENDING 재실행 대기.
    PlanRollback {
        /// 계획 문서 경로 (JSON)
        plan: String,
        /// 되돌릴 태스크 id
        task: String,
    },
    /// 계획을 실행한다 — Executor(서브프로세스·격리) → 검증 → 체크포인트 → 재개.
    RunPlan {
        /// 계획 문서 경로 (JSON)
        path: String,
        /// 도구 승인 자동 (executor 서브프로세스에도 전달)
        #[arg(long)]
        yes: bool,
        /// git 체크포인트 커밋 생략
        #[arg(long)]
        no_checkpoint: bool,
    },
    /// MCP 서버 모드 (facts 메모리 remember/recall/forget/list 를 stdio MCP로 노출)
    McpServe,
    /// 새 릴리스 확인 후 업그레이드 진행 (에이전트 밖에서 단독 실행)
    Update,
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
    /// 설정 (연결·모델·Harness·텔레그램·옵시디언·순위)
    #[command(alias = "설정")]
    Settings,
    /// 모델 순위표
    Ranks {
        #[command(subcommand)]
        action: Option<RanksCmd>,
    },
    /// 화면 테마 · 배경 모드 (터미널 = 데스크탑 관리자 '화면' 탭과 동일)
    Theme {
        /// rafikx | opal | synth (생략 시 현재값 표시)
        theme: Option<String>,
        /// light | dark | auto (운영체제 설정 자동)
        #[arg(long)]
        appearance: Option<String>,
    },
    /// 작업 폴더(프로젝트) 지정 또는 확인
    Workspace {
        /// 새 프로젝트 폴더 (생략 시 현재값)
        path: Option<String>,
    },
    /// Harness 선정 모드·분류별 모델 지정 (기본 자동, 수동 선택 가능)
    Harness {
        /// 분류 simple|medium|advanced|dev — 모델 지정 시 필요
        class: Option<String>,
        /// "provider:model" 또는 모델 ID. "" 또는 clear 로 자동 복귀
        model: Option<String>,
        /// auto | manual — 선정 모드만 변경
        #[arg(long)]
        mode: Option<String>,
        /// 해당 분류의 수동 지정 해제
        #[arg(long)]
        clear: bool,
    },
    /// 연결된 서비스에서 사용 가능한 원격 모델 목록
    Models {
        provider: String,
    },
    /// 지난 세션 내용 검색
    Find {
        query: String,
    },
    /// 모델 선택·등록·수정 마법사 (OMO 스타일)
    Model {
        #[command(subcommand)]
        action: Option<ModelCmd>,
    },
    /// 현재 상태 요약 (연결·Harness·오늘 사용량)
    Status,
    /// 텔레그램 봇 + Inspector 스케줄
    Telegram {
        /// Obsidian Vault 감시(watch)를 함께 켭니다
        #[arg(long)]
        with_watch: bool,
    },
    Rpc,
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

#[derive(Subcommand)]
enum ModelCmd {
    /// 연결·모델 현황 표
    List,
    /// 기본 연결·모델 변경 (번호 또는 provider:model)
    Use { arg: String },
}

#[tokio::main]
async fn main() -> ExitCode {
    ui::init();
    // MCP 외부 도구 서버 연결 — 설정 없으면 즉시 no-op, 실패해도 계속.
    rafikx::mcp::init_default();
    // 주 1회 모델 순위 갱신 — 백그라운드, 실패 무음.
    rafikx::ranks::spawn_weekly_refresh();
    let cli = Cli::parse();
    // 하루 1회 모델 카탈로그 자동 갱신 — 백그라운드, 실패 무음. 설정을 못 읽으면 건너뛴다.
    if let Ok(cfg) = Config::load(cli.config.as_deref()) {
        rafikx::auth::spawn_catalog_refresh(&cfg);
    }
    let result = match &cli.cmd {
        None => cmd_default(&cli).await,
        Some(Commands::Doctor) => cmd_doctor(&cli).await,
        Some(Commands::Login) => cmd_login(&cli).await,
        Some(Commands::Settings) => cmd_settings(&cli).await,
        Some(Commands::Ranks { action }) => cmd_ranks(action.as_ref()).await,
        Some(Commands::Theme { theme, appearance }) => {
            cmd_theme(&cli, theme.as_deref(), appearance.as_deref())
        }
        Some(Commands::Workspace { path }) => cmd_workspace(&cli, path.as_deref()),
        Some(Commands::Harness {
            class,
            model,
            mode,
            clear,
        }) => cmd_harness(
            &cli,
            class.as_deref(),
            model.as_deref(),
            mode.as_deref(),
            *clear,
        ),
        Some(Commands::Models { provider }) => cmd_models(&cli, provider).await,
        Some(Commands::Find { query }) => cmd_find(&cli, query),
        Some(Commands::Model { action }) => cmd_model(&cli, action.as_ref()).await,
        Some(Commands::Status) => cmd_status(&cli).await,
        Some(Commands::Ask { prompt, obsidian }) => cmd_ask(&cli, prompt, *obsidian).await,
        Some(Commands::Agent { prompt }) => cmd_ask(&cli, prompt, false).await,
        Some(Commands::Index) => cmd_index(&cli),
        Some(Commands::Search { query }) => cmd_search(&cli, query),
        Some(Commands::Watch) => cmd_watch(&cli).await,
        Some(Commands::Chat { list, resume }) => cmd_chat_entry(&cli, *list, resume.clone()).await,
        Some(Commands::Facts) => cmd_facts(&cli),
        Some(Commands::InitDeep) => cmd_init_deep(&cli),
        Some(Commands::Quota) => cmd_quota(&cli),
        Some(Commands::VerifyTask { path }) => cmd_verify_task(path).await,
        Some(Commands::VerifyPlan { path }) => cmd_verify_plan(path),
        Some(Commands::SpecFreeze { path }) => cmd_spec_freeze(path),
        Some(Commands::Calibrate { provider, model }) => {
            cmd_calibrate(provider.as_deref(), model.as_deref()).await
        }
        Some(Commands::PlanReport { path }) => cmd_plan_report(path),
        Some(Commands::PlanRollback { plan, task }) => cmd_plan_rollback(&plan, &task).await,
        Some(Commands::RunPlan {
            path,
            yes,
            no_checkpoint,
        }) => cmd_run_plan(path, *yes, *no_checkpoint).await,
        Some(Commands::McpServe) => rafikx::mcp_serve::stdio().await,
        Some(Commands::Update) => rafikx::update::run_update_flow(),
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
        Some(Commands::Rpc) => rafikx::rpc::stdio().await,
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

fn cmd_theme(cli: &Cli, theme: Option<&str>, appearance: Option<&str>) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    if theme.is_none() && appearance.is_none() {
        println!("테마    {}   (rafikx | opal | synth)", cfg.file.ui.theme);
        println!(
            "배경    {}   (light | dark | auto — auto 는 운영체제 설정 사용)",
            cfg.file.ui.appearance
        );
        println!("변경 예: rafikx theme opal   ·   rafikx theme --appearance dark");
        return Ok(());
    }
    if let Some(t) = theme {
        if !rafikx::palette::names().contains(&t) {
            anyhow::bail!(
                "'{t}' 테마가 없습니다. 사용 가능: {}",
                rafikx::palette::names().join(", ")
            );
        }
        config::write_toml_key(&cfg.path, "[ui]", "theme", &config::toml_string(t))?;
        ui::ok(&format!("테마 저장: {t}"));
    }
    if let Some(a) = appearance {
        let m = match a.trim().to_ascii_lowercase().as_str() {
            "light" => "light",
            "dark" => "dark",
            _ => "auto",
        };
        config::write_toml_key(&cfg.path, "[ui]", "appearance", &config::toml_string(m))?;
        ui::ok(&format!(
            "배경 모드 저장: {m}{}",
            if m == "auto" {
                " (운영체제 설정)"
            } else {
                ""
            }
        ));
    }
    Ok(())
}

fn cmd_workspace(cli: &Cli, path: Option<&str>) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let Some(p) = path else {
        println!("현재 프로젝트 폴더: {}", cfg.workspace.display());
        println!("변경 예: rafikx workspace C:\\projects\\my-app");
        return Ok(());
    };
    let expanded = config::expand_tilde(p);
    if !expanded.exists() {
        std::fs::create_dir_all(&expanded)?;
        ui::note(&format!("폴더를 새로 만들었습니다: {}", expanded.display()));
    }
    config::write_toml_key(&cfg.path, "[general]", "workspace", &config::toml_string(p))?;
    let cfg = cfg.reload()?;
    ui::ok(&format!("프로젝트 폴더 변경: {}", cfg.workspace.display()));
    Ok(())
}

fn cmd_harness(
    cli: &Cli,
    class: Option<&str>,
    model: Option<&str>,
    mode: Option<&str>,
    clear: bool,
) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    if let Some(m) = mode {
        harness::set_selection_mode(&cfg, m)?;
        let applied = if m.eq_ignore_ascii_case("manual") {
            "manual"
        } else {
            "auto"
        };
        ui::ok(&format!("Harness 선정 모드: {applied}"));
    }
    if clear || model.is_some() {
        let Some(c) = class else {
            anyhow::bail!(
                "분류가 필요합니다. 예: rafikx harness dev gpt-5.6  ·  rafikx harness --clear dev"
            );
        };
        let tc = harness::TaskClass::parse(c).ok_or_else(|| {
            anyhow::anyhow!("분류는 simple|medium|advanced|dev 중 하나여야 합니다")
        })?;
        let spec = if clear { "" } else { model.unwrap_or("") };
        let msg = harness::set_manual_model(&cfg, tc, spec)?;
        ui::ok(&msg);
        if !clear && !spec.is_empty() && cfg.file.harness.selection.eq_ignore_ascii_case("auto") {
            ui::note(
                "현재 선정 모드가 auto 입니다. 수동 지정을 쓰려면: rafikx harness --mode manual",
            );
        }
    }
    // 상태 표시 (변경이 있었으면 다시 읽는다)
    let cfg = if mode.is_some() || clear || model.is_some() {
        cfg.reload()?
    } else {
        cfg
    };
    println!();
    println!(
        "선정 모드: {}",
        if cfg.file.harness.selection.eq_ignore_ascii_case("manual") {
            "manual (수동 우선, 빈 분류는 자동)"
        } else {
            "auto (업무 난이도별 자동 — 기본)"
        }
    );
    for c in [
        harness::TaskClass::Simple,
        harness::TaskClass::Medium,
        harness::TaskClass::Advanced,
        harness::TaskClass::Dev,
    ] {
        let h = &cfg.file.harness;
        let manual = match c {
            harness::TaskClass::Simple => h.manual_simple.clone(),
            harness::TaskClass::Medium => h.manual_medium.clone(),
            harness::TaskClass::Advanced => h.manual_design.clone(),
            harness::TaskClass::Dev => h.manual_debug.clone(),
        }
        .filter(|s| !s.is_empty());
        match harness::bind(&cfg, c, None, None) {
            Ok(b) => println!(
                "  {:8} → {:8} {}/{}  [{}]",
                c.as_str(),
                b.profile_name,
                b.provider_name,
                b.model,
                manual.as_deref().unwrap_or("자동")
            ),
            Err(e) => println!("  {:8} → (미연결: {e})", c.as_str()),
        }
    }
    println!();
    println!("{}", ranks::status_line());
    Ok(())
}

async fn cmd_models(cli: &Cli, provider: &str) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let models = auth::list_remote_models(&cfg, provider).await?;
    if models.is_empty() {
        println!("{provider}: 사용 가능한 원격 모델 목록이 비어 있습니다 (키/연결 확인)");
        return Ok(());
    }
    // 조회 결과를 캐시에 저장 — 이 명령이 곧 한 서비스의 카탈로그 갱신이 되도록.
    match auth::save_catalog(&cfg, provider, &models) {
        Ok(()) => println!("({}개 모델을 캐시에 저장했습니다)", models.len()),
        Err(e) => ui::note(&format!("모델 목록 캐시 저장 실패: {e:#}")),
    }
    println!(
        "{} 사용 가능 모델 {}개:",
        auth::provider_label(provider),
        models.len()
    );
    for m in &models {
        println!("  {m}");
    }
    println!();
    println!("Harness에 지정: rafikx harness <분류> <모델ID>");
    Ok(())
}

fn cmd_find(cli: &Cli, query: &str) -> Result<()> {
    let _ = cli;
    let db = Db::open(&Db::db_path()?)?;
    let rows = db.search_sessions(query, 20)?;
    if rows.is_empty() {
        println!("'{query}' 검색 결과가 없습니다.");
        return Ok(());
    }
    println!("'{query}' 검색 결과 {}건:", rows.len());
    for r in rows {
        println!(
            "  {:<26} {}",
            r.id,
            r.title.unwrap_or_else(|| "(제목 없음)".into())
        );
    }
    println!("이어하기: rafikx chat --resume <id>");
    Ok(())
}

/// 운영 상태 요약 — OMO doctor 의 "한눈에 현재 구성" 철학.
async fn cmd_status(cli: &Cli) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let db = Db::open(&Db::db_path()?)?;

    let default_name = cfg.file.general.default_provider.clone();
    let (label, model) = match cfg.provider(&default_name) {
        Ok(p) => (crate::auth::provider_label(&default_name), p.model.clone()),
        Err(_) => (default_name.clone(), "-".into()),
    };
    ui::section("연결");
    println!("  기본  {label} / {}", ui::bold(&model));
    for r in model_wizard::rows(&cfg).iter().filter(|r| r.connected) {
        if r.id != default_name {
            println!("  연결  {} / {}", r.label, r.model);
        }
    }

    ui::section("Harness");
    let (engine_name, _) = rafikx::engine::normalize(&cfg.file.general.engine);
    let engine_spec = rafikx::engine::resolve_with(&cfg.file.engines, &engine_name);
    let discipline = rafikx::engine::normalize_discipline(&cfg.file.general.discipline);
    println!("  엔진: {engine_name} ({})", engine_spec.summary);
    println!(
        "  분야: {}  ·  팀: {}  ·  self 메타: {}  ·  독립 검증자 게이트: {}",
        discipline.as_str(),
        harness::team_mode(&cfg).as_str(),
        if rafikx::self_harness::meta_active(&cfg) {
            "on"
        } else {
            "off"
        },
        if cfg.file.harness.strict_gate {
            "on (strict 엔진에서)"
        } else {
            "off"
        }
    );
    println!(
        "  선정 모드: {}",
        if cfg.file.harness.selection.eq_ignore_ascii_case("manual") {
            "manual"
        } else {
            "auto (업무별 자동)"
        }
    );
    for c in [
        harness::TaskClass::Simple,
        harness::TaskClass::Medium,
        harness::TaskClass::Advanced,
        harness::TaskClass::Dev,
    ] {
        if let Ok(mut b) = harness::bind(&cfg, c, None, None) {
            // 표는 실제 실행에 붙는 조합을 보여야 하므로 엔진 고정을 반영한다.
            let _ = harness::apply_engine_pin(&cfg, &mut b, None, None);
            println!(
                "  {:8} → {:8} {}/{}",
                b.class.as_str(),
                b.profile_name,
                b.provider_name,
                b.model
            );
        }
    }

    ui::section("오늘");
    let (runs, tin, tout) = db.usage_today()?;
    println!("  실행 {runs}회 · 토큰 in {tin} / out {tout}");
    for line in rafikx::usage::footer_lines() {
        println!("  {line}");
    }

    ui::section("순위표");
    println!("  {}", ranks::status_line());
    Ok(())
}

async fn cmd_model(cli: &Cli, action: Option<&ModelCmd>) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    match action {
        Some(ModelCmd::List) => model_wizard::cmd_list(&cfg).await,
        Some(ModelCmd::Use { arg }) => model_wizard::cmd_use(&cfg, arg).await,
        None => {
            if !std::io::stdin().is_terminal() {
                return model_wizard::cmd_list(&cfg).await;
            }
            model_wizard::run_wizard(cfg).await
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
        ui::warn(&format!(
            "워크스페이스  {} (폴더가 아직 없습니다)",
            cfg.workspace.display()
        ));
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
        ui::warn(&format!(
            "Vault  {} (폴더가 아직 없습니다. index 시 생성됩니다)",
            vault.display()
        ));
    }
    ui::ok(&format!(
        "tokenizer {}  enabled={}",
        cfg.file.obsidian.tokenizer, cfg.file.obsidian.enabled
    ));
    ui::ok(&format!(
        "Harness  {}",
        if cfg.file.harness.selection.eq_ignore_ascii_case("manual") {
            "수동"
        } else {
            "자동"
        }
    ));
    ui::ok(&format!(
        "화면  테마={} 배경={}",
        cfg.file.ui.theme, cfg.file.ui.appearance
    ));
    ui::ok(&ranks::status_line());

    let reg = rafikx::tools::ToolRegistry::all();
    let specs = reg.specs();
    let mut tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    tool_names.sort();
    ui::ok(&format!(
        "도구 {}개  {}",
        tool_names.len(),
        tool_names.join(", ")
    ));

    ui::section(&format!(
        "서비스  (기본={})",
        cfg.file.general.default_provider
    ));
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
            ui::ok(&format!("{name:14}  {}  모델={}", mode, p.model));
            for (a, live, tail) in rows {
                if live {
                    ui::note(&format!("{}  ****{tail}", accounts::display(&a)));
                } else {
                    ui::warn(&format!("{}  만료", accounts::display(&a)));
                }
            }
        }
        if auth::is_usable(&cfg, name) {
            match auth::list_remote_models(&cfg, name).await {
                Ok(models) if !models.is_empty() => {
                    let show: Vec<_> = models.iter().take(5).cloned().collect();
                    ui::note(&format!(
                        "모델 {}{}",
                        show.join(", "),
                        if models.len() > 5 { " …" } else { "" }
                    ));
                }
                Ok(_) => {}
                Err(e) => ui::note(&format!("{name}: 모델 목록 조회 실패 ({e:#})")),
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

    // OMO doctor 의 모델 해석 검증 수용: 4개 분류가 전부 유효한 모델로 묶이는지.
    let ok_classes = [
        harness::TaskClass::Simple,
        harness::TaskClass::Medium,
        harness::TaskClass::Advanced,
        harness::TaskClass::Dev,
    ]
    .iter()
    .filter(|c| harness::bind(&cfg, **c, None, None).is_ok())
    .count();
    if ok_classes == 4 {
        ui::ok("폴백 체인  4/4 분류 유효");
    } else {
        ui::warn(&format!(
            "폴백 체인  {ok_classes}/4 분류만 유효 — 연결을 추가하거나 rafikx model use 로 기본 연결을 고르세요"
        ));
        ok = false;
    }

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
    // 레인 라우팅 — chat 경로(run_turn_observed)와 같은 규칙을 단발 ask 에도 적용한다.
    let lane = rafikx::harness::suggest_lane(prompt);
    let mut binding = match lane {
        Some(l) => rafikx::harness::bind_profile(
            &cfg,
            class,
            Some(l),
            cli.provider.as_deref(),
            cli.model.as_deref(),
        )?,
        None => bind(&cfg, class, cli.provider.as_deref(), cli.model.as_deref())?,
    };
    if let Some(w) = harness::apply_engine_pin(
        &cfg,
        &mut binding,
        cli.provider.as_deref(),
        cli.model.as_deref(),
    ) {
        rafikx::ui::warn(&w);
    }
    print_binding(&cfg, &binding);

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
    let run_context = RunContext::for_config(RunId::new(run_id.clone()), Arc::new(cfg.clone()))
        .with_live_sink(ui::current_live_sink());
    graph::trace_start_in(
        &run_context,
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

    // 진행 표시 — 첫 출력이 나오면 스피너는 스스로 물러난다.
    let sp = rafikx::spinner::Spinner::start_in(run_context.clone(), "응답 생성 중…");
    match run_pipeline_with_context(
        &cfg,
        &binding,
        &task,
        cli.yes || cfg.file.general.approval.eq_ignore_ascii_case("yolo"),  // chat 경로(open_session)와 같은 yolo 규칙 (B2 수정)
        cli.provider.as_deref(),
        None,
        None,
        None,
        run_context.clone(),
    )
    .await
    {
        Ok(outcome) => {
            sp.finish();
            agent::record_finish(&db, &run_id, &outcome)?;
            graph::node_in(&run_context, "persist", &outcome.status, "", Some("bind"));
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
            // 단발 실행은 여기서 프로세스가 끝난다 — Self-Harness 관찰(실패 채굴·
            // 제안)이 백그라운드에서 abort 되지 않게 완료를 기다린다.
            rafikx::self_harness::flush_observations(std::time::Duration::from_secs(120)).await;
            ui::print_footer();
            Ok(())
        }
        Err(e) => {
            sp.finish();
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

/// verify-task — 태스크 문서의 verification 을 시스템이 직접 실행한다.
/// 증거는 태스크 파일과 LEDGER.jsonl 에 남고, exit code 가 판정이다.
async fn cmd_verify_task(path: &str) -> Result<()> {
    use rafikx::verify::TaskState;
    let task_path = std::path::PathBuf::from(path);
    let mut task = rafikx::verify::TaskDoc::load(&task_path)?;
    let workspace = std::env::current_dir()?;
    println!(
        "[verify-task] {} — 검증 명령 {}개 실행",
        task.id,
        task.verification.len()
    );
    let outcome = rafikx::verify::run_task_verification(&task, &workspace).await;
    // LEDGER 원장 기록 (증거는 시스템만 쓴다).
    let ledger = task_path
        .parent()
        .map(|d| d.join("LEDGER.jsonl"))
        .unwrap_or_else(|| std::path::PathBuf::from("LEDGER.jsonl"));
    let event = serde_json::json!({
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "task_id": task.id,
        "event": "verification",
        "state_before": task.state,
    });
    rafikx::verify::runner::append_ledger(&ledger, &event)?;
    let mut outcome = outcome;
    // 품질 래칫 — 통과 테스트 수가 과거 최고점에서 후퇴했으면 완료를 뺏는다 (G17).
    if let rafikx::verify::task::TaskOutcome::Done(report) = &outcome {
        let current = report
            .results
            .results
            .iter()
            .filter_map(|e| e.tests_passed)
            .max()
            .unwrap_or(0);
        if current > 0 {
            if let Some(violation) = rafikx::verify::ratchet_check(&ledger, current) {
                println!("  ✗ {violation}");
                outcome = rafikx::verify::task::TaskOutcome::Rework(violation);
            } else {
                rafikx::verify::runner::append_ledger(
                    &ledger,
                    &serde_json::json!({
                        "event": "metric",
                        "task_id": task.id,
                        "tests_passed": current
                    }),
                )?;
            }
        }
    }
    let state = *task.apply(outcome);
    if let TaskState::Done = state {
        let n = task.verification.len();
        for ev in task.evidence.iter().rev().take(n).rev() {
            println!("  ✓ {} → exit {:?}", ev.cmd, ev.exit_code);
        }
        let stat = task.evidence.last().and_then(|e| e.diff_stat.clone());
        if let Some(stat) = stat {
            println!("  diff: {}", stat.lines().last().unwrap_or(""));
        }
    } else if let Some(last) = task.evidence.last() {
        println!("  ✗ {}", last.output_tail);
    }
    task.save(&task_path)?;
    println!("[verify-task] 상태: {state:?}");
    match state {
        TaskState::Done => Ok(()),
        _ => Err(anyhow::anyhow!("verification failed — 상태 {state:?}")),
    }
}

/// verify-plan — 계획 데이터의 AC 커버리지·태스크 검증 가능성을 검사한다 (M2).
/// spec-freeze — SPEC 을 검증하고 동결한다 (M3). 동결 후엔 재승인 없이 변경 불가.
fn cmd_spec_freeze(path: &str) -> Result<()> {
    let mut spec = rafikx::verify::SpecDoc::load(std::path::Path::new(path))?;
    spec.freeze()?;
    spec.save(std::path::Path::new(path))?;
    println!(
        "[spec-freeze] {} 동결 — AC {}개 · 가정 {}건. 이후 변경은 변경 요청 절차로만 가능합니다.",
        spec.id,
        spec.acceptance.len(),
        spec.assumptions.len()
    );
    Ok(())
}

/// calibrate — 보정 스위트 실행 (M5). 능력 점수는 model_profiles.json 에 저장되고
/// 검증 강도 상향에 쓰인다.
async fn cmd_calibrate(provider: Option<&str>, model: Option<&str>) -> Result<()> {
    let cfg = Config::load(None)?;
    let provider = provider
        .map(str::to_string)
        .unwrap_or_else(|| cfg.file.general.default_provider.clone());
    let model = match model {
        Some(m) => m.to_string(),
        None => cfg
            .provider(&provider)
            .ok()
            .map(|p| p.model.clone())
            .filter(|m| !m.is_empty())
            .ok_or_else(|| anyhow::anyhow!("모델을 지정하거나 기본 모델을 설정하세요"))?,
    };
    println!("[calibrate] {provider}/{model} — 9 프로브 실행 중…");
    let cal = rafikx::calibrate::run_calibration(&cfg, &provider, &model).await?;
    for (group, (passed, total)) in &cal.detail {
        println!("  {group}: {passed}/{total}");
    }
    println!(
        "[calibrate] 능력 점수: {:.2} — {}",
        cal.capability,
        if cal.capability < 0.5 {
            "검증 Strict 강제"
        } else if cal.capability < 0.8 {
            "검증 Auto 상향"
        } else {
            "기존 정책 유지"
        }
    );
    rafikx::calibrate::save_profile(&cfg, &cal)?;
    Ok(())
}

/// plan-rollback — 태스크 체크포인트 revert (G15 완성).
async fn cmd_plan_rollback(plan: &str, task: &str) -> Result<()> {
    let mut work = rafikx::verify::WorkRun::load(std::path::Path::new(plan))?;
    let ledger = work
        .plan_path
        .parent()
        .map(|d| d.join("LEDGER.jsonl"))
        .unwrap_or_else(|| std::path::PathBuf::from("LEDGER.jsonl"));
    let workspace = std::env::current_dir()?;
    let msg = rafikx::verify::orchestrator::rollback_task(&mut work, task, &workspace, &ledger).await?;
    println!("[plan-rollback] {msg}");
    Ok(())
}

/// plan-report — LEDGER 원장 집계 (M6).
fn cmd_plan_report(path: &str) -> Result<()> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("원장을 읽을 수 없습니다: {path}"))?;
    let mut done = 0usize;
    let mut escalated: Vec<(String, String)> = Vec::new();
    let mut peak_tests: u32 = 0;
    let mut verifications = 0usize;
    for line in raw.lines() {
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match ev.get("event").and_then(|v| v.as_str()) {
            Some("verification") => verifications += 1,
            Some("metric") => {
                if let Some(n) = ev.get("tests_passed").and_then(|v| v.as_u64()) {
                    peak_tests = peak_tests.max(n as u32);
                }
            }
            Some("state") => {
                let id = ev.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
                match ev.get("state").and_then(|v| v.as_str()) {
                    Some("DONE") => done += 1,
                    Some("ESCALATED") => escalated.push((
                        id.to_string(),
                        ev.get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("사유 미상")
                            .to_string(),
                    )),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    println!("=== 원장 리포트 ({path}) ===");
    println!("검증 실행: {verifications}회 · 완료 태스크: {done}개 · 최고 통과 테스트: {peak_tests}개");
    if escalated.is_empty() {
        println!("에스컬레이션: 없음");
    } else {
        println!("에스컬레이션 {}건:", escalated.len());
        for (id, reason) in &escalated {
            println!("  - {id}: {reason}");
        }
    }
    Ok(())
}

/// run-plan — 계획을 순서대로 실행한다 (M4). 재개 지점부터, 검증 통과분만 진행.
async fn cmd_run_plan(path: &str, yes: bool, no_checkpoint: bool) -> Result<()> {
    let mut work = rafikx::verify::WorkRun::load(std::path::Path::new(path))?;
    work.spec_gate()?;
    let violations = work.plan.coverage_violations();
    if !violations.is_empty() {
        for v in &violations {
            println!("  ✗ {v}");
        }
        anyhow::bail!("계획 확정 거부 — verify-plan 으로 먼저 통과하세요");
    }
    if no_checkpoint {
        work.config.checkpoint_commits = false;
    }
    let total = work.plan.task_docs().len();
    let done_before = work.done_count();
    println!(
        "[run-plan] {} — 태스크 {}/{} 완료 상태에서 재개합니다 (executor: {})",
        work.plan.id,
        done_before,
        total,
        work.config.executor
    );
    let ledger = work
        .plan_path
        .parent()
        .map(|d| d.join("LEDGER.jsonl"))
        .unwrap_or_else(|| std::path::PathBuf::from("LEDGER.jsonl"));
    let workspace = std::env::current_dir()?;
    let reports = rafikx::verify::run_plan(&mut work, &workspace, yes, &ledger).await;
    println!("[run-plan] 결과:");
    for r in &reports {
        let reason = r
            .reason
            .as_deref()
            .map(|s| format!(" — {s}"))
            .unwrap_or_default();
        println!("  {:?} {} (시도 {}회){reason}", r.state, r.id, r.attempts);
    }
    let all_done = reports.iter().all(|r| r.state == rafikx::verify::TaskState::Done);
    if all_done {
        println!("[run-plan] 계획 전체 완료 — 각 태스크는 시스템 검증을 통과했다.");
        Ok(())
    } else {
        Err(anyhow::anyhow!("계획 미완료 — 위 보고를 확인하세요"))
    }
}

fn cmd_verify_plan(path: &str) -> Result<()> {
    let plan = rafikx::verify::PlanDoc::load(std::path::Path::new(path))?;
    print!("{}", plan.coverage_matrix());
    let violations = plan.coverage_violations();
    if violations.is_empty() {
        println!(
            "[verify-plan] 확정 가능 — AC {}개가 태스크 {}개에 모두 매핑됐습니다",
            plan.acceptance.len(),
            plan.task_docs().len()
        );
        return Ok(());
    }
    for v in &violations {
        println!("  ✗ {v}");
    }
    Err(anyhow::anyhow!(
        "계획 확정 거부 — 위반 {}건을 고친 뒤 다시 검사하세요",
        violations.len()
    ))
}

fn cmd_quota(cli: &Cli) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    for line in rafikx::usage::quota_lines(&cfg) {
        println!("{line}");
    }
    Ok(())
}

fn cmd_init_deep(cli: &Cli) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    for note in chat::init_deep_notes(&cfg.workspace) {
        println!("{note}");
    }
    Ok(())
}

fn cmd_facts(cli: &Cli) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let db = Db::open(&Db::db_path()?)?;
    let rows = db.list_facts(Some(&cfg.workspace))?;
    if rows.is_empty() {
        println!("기억하는 사실이 없습니다.");
        return Ok(());
    }
    for r in rows {
        let scope = if r.project_id.is_empty() { "전역" } else { "프로젝트" };
        println!("({}·{}) {}: {}", r.kind, scope, r.key, r.value);
    }
    Ok(())
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
    // rafikx -c — 최근 세션을 바로 이어서 시작한다.
    let resume = if cli.continue_last {
        Db::open(&Db::db_path()?)
            .ok()
            .and_then(|db| db.list_sessions(1).ok())
            .and_then(|rows| rows.into_iter().next())
            .map(|row| row.id)
    } else {
        None
    };
    cmd_chat_entry(cli, false, resume).await
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
            Ok(()) => {
                // U 키로 업그레이드가 요청됐으면 에이전트를 나와서 단독으로 진행한다.
                if rafikx::update::take_update_request() {
                    println!();
                    return rafikx::update::run_update_flow();
                }
                return Ok(());
            }
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
