mod agent;
mod applog;
mod chat;
mod config;
mod db;
mod harness;
mod lessons;
mod obsidian;
mod provider;
mod tools;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use config::Config;
use db::Db;
use harness::{bind, classify, ping_provider, print_binding, print_binding_table, run_pipeline};

#[derive(Parser)]
#[command(name = "agent-harness", version, about = "초경량 개인용 AI 코딩 에이전트 CLI")]
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
    cmd: Commands,
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
    /// 대화 REPL
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
    /// 설정·키·워크스페이스·프로바이더를 점검합니다
    Doctor,
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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match &cli.cmd {
        Commands::Doctor => cmd_doctor(&cli).await,
        Commands::Ask { prompt, obsidian } => cmd_ask(&cli, prompt, *obsidian).await,
        Commands::Agent { prompt } => cmd_ask(&cli, prompt, false).await,
        Commands::Index => cmd_index(&cli),
        Commands::Search { query } => cmd_search(&cli, query),
        Commands::Watch => cmd_watch(&cli).await,
        Commands::Chat { list, resume } => {
            cmd_chat_entry(&cli, *list, resume.clone()).await
        }
        Commands::Lessons { action } => cmd_lessons(&cli, action),
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

async fn cmd_doctor(cli: &Cli) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let mut ok = true;

    println!("agent-harness doctor");
    println!();

    println!("[OK] 설정 파일: {}", cfg.path.display());

    let provider_name = &cfg.file.general.default_provider;
    match cfg.provider(provider_name) {
        Ok(p) => {
            let env_name = if p.api_key_env.is_empty() {
                "(없음)"
            } else {
                p.api_key_env.as_str()
            };
            match cfg.api_key_tail(provider_name)? {
                Some(tail) => println!("[OK] API 키 ({env_name}): ****{tail}"),
                None => {
                    println!("[주의] API 키 ({env_name}): 아직 등록되지 않음 (나중에 설정해도 됩니다)");
                    ok = false;
                }
            }
        }
        Err(e) => {
            println!("[실패] 기본 프로바이더 '{provider_name}': {e}");
            ok = false;
        }
    }

    if cfg.workspace.exists() {
        println!("[OK] 워크스페이스: {}", cfg.workspace.display());
    } else {
        println!("[주의] 워크스페이스: {} (폴더가 아직 없습니다)", cfg.workspace.display());
        ok = false;
    }

    let db_path = Db::db_path()?;
    match Db::open(&db_path) {
        Ok(db) => {
            println!("[OK] DB: {}", db_path.display());
            match db.has_fts5() {
                Ok(true) => println!("[OK] FTS5: 사용 가능"),
                Ok(false) => {
                    println!("[실패] FTS5: rusqlite bundled 에 FTS5가 없습니다");
                    ok = false;
                }
                Err(e) => {
                    println!("[실패] FTS5 확인: {e}");
                    ok = false;
                }
            }
        }
        Err(e) => {
            println!("[실패] DB: {e}");
            ok = false;
        }
    }

    let vault = config::expand_tilde(&cfg.file.obsidian.vault_path);
    if vault.exists() {
        println!("[OK] Vault: {}", vault.display());
    } else {
        println!("[주의] Vault: {} (폴더가 아직 없습니다. index 시 생성됩니다)", vault.display());
    }
    println!(
        "[OK] tokenizer: {}",
        cfg.file.obsidian.tokenizer
    );

    println!();
    println!("프로바이더 ping");
    let mut names: Vec<String> = cfg.file.providers.keys().cloned().collect();
    names.sort();
    for name in names {
        println!("  {}", ping_provider(&cfg, &name).await);
    }

    print_binding_table(&cfg);

    println!();
    if ok {
        println!("모든 항목 OK (ping/바인딩은 위 표 참고)");
    } else {
        println!("주의 항목이 있습니다. API 키와 텔레그램은 나중에 등록해도 됩니다.");
    }
    Ok(())
}

async fn cmd_ask(cli: &Cli, prompt: &str, obsidian: bool) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let mode = if matches!(cli.cmd, Commands::Agent { .. }) {
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
        match crate::obsidian::ask_context(&cfg, prompt) {
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

    let db = Db::open(&Db::db_path()?)?;
    let run_id = db.start_run(
        mode,
        prompt,
        Some(binding.class.as_str()),
        Some(&binding.profile_name),
        Some(&binding.provider_name),
        Some(&binding.model),
    )?;
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
    )
    .await
    {
        Ok(outcome) => {
            agent::record_finish(&db, &run_id, &outcome)?;
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
            Ok(())
        }
        Err(e) => {
            let _ = db.finish_run(&run_id, "fail", 0, 0, 0, Some(&e.to_string()));
            let fail = agent::AgentOutcome {
                status: "fail".into(),
                error: Some(e.to_string()),
                ..Default::default()
            };
            lessons::maybe_spawn(&cfg, prompt, &fail);
            Err(e)
        }
    }
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

async fn cmd_chat_entry(cli: &Cli, list: bool, resume: Option<String>) -> Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    chat::cmd_chat(
        cfg,
        cli.yes,
        cli.provider.clone(),
        cli.model.clone(),
        cli.class.clone(),
        list,
        resume,
    )
    .await
}

