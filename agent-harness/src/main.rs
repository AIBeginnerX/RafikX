mod agent;
mod applog;
mod config;
mod db;
mod provider;
mod tools;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use config::Config;
use db::Db;
use provider::{AnthropicProvider, ChatRequest, Message};

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
    /// 한 번 질문하고 응답을 스트리밍합니다
    Ask {
        /// 지시문
        prompt: String,
    },
    /// 개발 하네스: 도구를 쓰며 작업을 수행합니다 (승인 필요)
    Agent {
        /// 작업 지시
        prompt: String,
    },
    /// 설정·키·워크스페이스를 점검합니다
    Doctor,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match &cli.cmd {
        Commands::Doctor => cmd_doctor(cli.config.as_deref()),
        Commands::Ask { prompt } => cmd_ask(&cli, prompt).await,
        Commands::Agent { prompt } => cmd_agent(&cli, prompt).await,
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

fn cmd_doctor(config_path: Option<&std::path::Path>) -> Result<()> {
    let cfg = Config::load(config_path)?;
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
        Ok(_) => println!("[OK] DB: {}", db_path.display()),
        Err(e) => {
            println!("[실패] DB: {e}");
            ok = false;
        }
    }

    println!();
    if ok {
        println!("모든 항목 OK");
    } else {
        println!("주의 항목이 있습니다. API 키와 텔레그램은 나중에 등록해도 됩니다.");
    }
    Ok(())
}

async fn cmd_ask(cli: &Cli, prompt: &str) -> Result<()> {
    if cli.class.is_some() {
        eprintln!("알림: --class 는 Phase 3 하네스에서 사용합니다. 지금은 무시합니다.");
    }

    let cfg = Config::load(cli.config.as_deref())?;
    let (provider_name, model, api_key) = resolve_anthropic(&cfg, cli)?;
    let db = Db::open(&Db::db_path()?)?;
    let run_id = db.start_run("ask", prompt, Some(&provider_name), Some(&model))?;

    applog::info(&format!("ask start provider={provider_name} model={model}"));

    let client = AnthropicProvider::new(api_key)?;
    let req = ChatRequest {
        model: model.clone(),
        system: "You are agent-harness, a personal CLI assistant. Answer clearly and concisely.".to_string(),
        messages: vec![Message::user_text(prompt)],
        tools: vec![],
        max_tokens: cfg.file.general.max_tokens,
        stream: true,
    };

    match client
        .chat_stream(&req, |piece| {
            print!("{piece}");
            let _ = io::stdout().flush();
        })
        .await
    {
        Ok(resp) => {
            println!();
            println!(
                "[tokens] in={} out={} stop={:?}",
                resp.input_tokens, resp.output_tokens, resp.stop_reason
            );
            db.finish_run(
                &run_id,
                "ok",
                1,
                resp.input_tokens as i64,
                resp.output_tokens as i64,
                None,
            )?;
            applog::info(&format!(
                "ask done in={} out={}",
                resp.input_tokens, resp.output_tokens
            ));
            Ok(())
        }
        Err(e) => {
            let _ = db.finish_run(&run_id, "fail", 0, 0, 0, Some(&e.to_string()));
            Err(e)
        }
    }
}

async fn cmd_agent(cli: &Cli, prompt: &str) -> Result<()> {
    if cli.class.is_some() {
        eprintln!("알림: --class 는 Phase 3 하네스에서 사용합니다. 지금은 무시합니다.");
    }

    let cfg = Config::load(cli.config.as_deref())?;
    let (provider_name, model, api_key) = resolve_anthropic(&cfg, cli)?;
    let db = Db::open(&Db::db_path()?)?;
    let run_id = db.start_run("agent", prompt, Some(&provider_name), Some(&model))?;
    applog::info(&format!("agent start provider={provider_name} model={model}"));

    let client = AnthropicProvider::new(api_key)?;
    let result = agent::run_agent(&cfg, &client, &model, prompt, cli.yes).await;
    match result {
        Ok(outcome) => {
            agent::record_finish(&db, &run_id, &outcome)?;
            println!(
                "[run] status={} iter={} tokens in={} out={}",
                outcome.status, outcome.iterations, outcome.input_tokens, outcome.output_tokens
            );
            Ok(())
        }
        Err(e) => {
            let _ = db.finish_run(&run_id, "fail", 0, 0, 0, Some(&e.to_string()));
            Err(e)
        }
    }
}

fn resolve_anthropic(cfg: &Config, cli: &Cli) -> Result<(String, String, String)> {
    let provider_name = cli
        .provider
        .clone()
        .unwrap_or_else(|| cfg.file.general.default_provider.clone());
    let p = cfg.provider(&provider_name)?;
    if p.kind != "anthropic" {
        anyhow::bail!(
            "프로바이더 '{provider_name}' (kind={}) 는 Phase 3에서 연결됩니다. 지금은 anthropic 만 지원합니다.",
            p.kind
        );
    }
    let Some(api_key) = cfg.api_key(&provider_name)? else {
        anyhow::bail!(
            "환경변수 {} 가 없습니다. API 키는 나중에 등록하면 됩니다. 지금은 `cargo run -- doctor` 로 설정을 확인하세요.",
            if p.api_key_env.is_empty() {
                "ANTHROPIC_API_KEY"
            } else {
                p.api_key_env.as_str()
            }
        );
    };
    let model = cli.model.clone().unwrap_or_else(|| p.model.clone());
    Ok((provider_name, model, api_key))
}
