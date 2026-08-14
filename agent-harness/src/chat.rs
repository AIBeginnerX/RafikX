use std::io::{self, Write};

use anyhow::{Result, anyhow};

use crate::agent;
use crate::applog;
use crate::config::Config;
use crate::db::Db;
use crate::harness::{bind, classify, print_binding, run_pipeline};
use crate::obsidian;
use crate::provider::{ContentBlock, Message, Role};

pub async fn cmd_chat(
    cfg: Config,
    yes: bool,
    mut provider: Option<String>,
    mut model: Option<String>,
    mut class: Option<String>,
    list: bool,
    resume: Option<String>,
) -> Result<()> {
    let db = Db::open(&Db::db_path()?)?;
    if list {
        return list_sessions(&db);
    }

    let mut session_id: Option<String> = None;
    let mut messages: Vec<Message> = Vec::new();
    let mut obsidian_on = false;
    let mut dirty = false;

    if let Some(id) = resume {
        let Some(row) = db.load_session(&id)? else {
            return Err(anyhow!("세션 '{id}' 를 찾지 못했습니다"));
        };
        session_id = Some(row.id.clone());
        messages = serde_json::from_str(&row.messages_json)
            .map_err(|_| anyhow!("세션 메시지를 읽지 못했습니다"))?;
        agent::sanitize_tool_pairs(&mut messages);
        println!(
            "세션 재개: {}  ({})",
            row.id,
            row.title.unwrap_or_else(|| "(제목 없음)".into())
        );
    } else {
        println!("chat 시작. /help 로 명령을 볼 수 있습니다. /quit 로 종료.");
    }

    loop {
        print!("chat> ");
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
            match handle_slash(
                line,
                &db,
                &mut session_id,
                &mut messages,
                &mut provider,
                &mut model,
                &mut class,
                &mut obsidian_on,
                &mut dirty,
            )? {
                Slash::Continue => continue,
                Slash::Quit => break,
                Slash::Agent(task) => {
                    run_turn(
                        &cfg,
                        yes,
                        provider.as_deref(),
                        model.as_deref(),
                        Some("dev"),
                        false,
                        &task,
                        &mut messages,
                        &mut dirty,
                    )
                    .await?;
                }
            }
            continue;
        }

        run_turn(
            &cfg,
            yes,
            provider.as_deref(),
            model.as_deref(),
            class.as_deref(),
            obsidian_on,
            line,
            &mut messages,
            &mut dirty,
        )
        .await?;
    }

    if dirty {
        let id = persist(&db, session_id.as_deref(), &mut messages)?;
        println!("세션을 저장했습니다: {id}");
    }
    Ok(())
}

enum Slash {
    Continue,
    Quit,
    Agent(String),
}

fn handle_slash(
    line: &str,
    db: &Db,
    session_id: &mut Option<String>,
    messages: &mut Vec<Message>,
    provider: &mut Option<String>,
    model: &mut Option<String>,
    class: &mut Option<String>,
    obsidian_on: &mut bool,
    dirty: &mut bool,
) -> Result<Slash> {
    let mut parts = line.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    match cmd {
        "/quit" | "/exit" => Ok(Slash::Quit),
        "/help" => {
            println!(
                "/save  /quit  /clear  /model <ID>  /provider <이름>  /class <simple|medium|advanced|dev>\n\
                 /obsidian on|off  /agent <지시>"
            );
            Ok(Slash::Continue)
        }
        "/save" => {
            let id = persist(db, session_id.as_deref(), messages)?;
            *session_id = Some(id.clone());
            *dirty = false;
            println!("저장됨: {id}");
            Ok(Slash::Continue)
        }
        "/clear" => {
            messages.clear();
            *dirty = true;
            println!("대화 맥락을 지웠습니다.");
            Ok(Slash::Continue)
        }
        "/model" => {
            if rest.is_empty() {
                println!("현재 모델: {}", model.as_deref().unwrap_or("(config 기본)"));
            } else {
                *model = Some(rest.to_string());
                println!("모델: {rest}");
            }
            Ok(Slash::Continue)
        }
        "/provider" => {
            if rest.is_empty() {
                println!(
                    "현재 프로바이더: {}",
                    provider.as_deref().unwrap_or("(config 기본)")
                );
            } else {
                *provider = Some(rest.to_string());
                println!("프로바이더: {rest}");
            }
            Ok(Slash::Continue)
        }
        "/class" => {
            if rest.is_empty() {
                *class = None;
                println!("분류 강제를 해제했습니다.");
            } else {
                *class = Some(rest.to_string());
                println!("분류 강제: {rest}");
            }
            Ok(Slash::Continue)
        }
        "/obsidian" => {
            match rest {
                "on" => {
                    *obsidian_on = true;
                    println!("Obsidian 컨텍스트: 켜짐");
                }
                "off" => {
                    *obsidian_on = false;
                    println!("Obsidian 컨텍스트: 꺼짐");
                }
                _ => println!("/obsidian on  또는  /obsidian off"),
            }
            Ok(Slash::Continue)
        }
        "/agent" => {
            if rest.is_empty() {
                println!("/agent <지시> 형식으로 쓰세요.");
                Ok(Slash::Continue)
            } else {
                Ok(Slash::Agent(rest.to_string()))
            }
        }
        _ => {
            println!("알 수 없는 명령입니다. /help");
            Ok(Slash::Continue)
        }
    }
}

async fn run_turn(
    cfg: &Config,
    yes: bool,
    provider: Option<&str>,
    model: Option<&str>,
    forced_class: Option<&str>,
    obsidian_on: bool,
    prompt: &str,
    messages: &mut Vec<Message>,
    dirty: &mut bool,
) -> Result<()> {
    let class = classify(cfg, prompt, obsidian_on, forced_class).await?;
    let binding = bind(cfg, class, provider, model)?;
    print_binding(&binding);

    let mut task = prompt.to_string();
    if obsidian_on {
        match obsidian::ask_context(cfg, prompt) {
            Ok(ctx) => {
                if ctx.sources.is_empty() {
                    println!("[Obsidian] 검색 결과 없음");
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
        "chat",
        prompt,
        Some(binding.class.as_str()),
        Some(&binding.profile_name),
        Some(&binding.provider_name),
        Some(&binding.model),
    )?;
    applog::info(&format!(
        "chat class={} profile={} provider={} model={}",
        binding.class.as_str(),
        binding.profile_name,
        binding.provider_name,
        binding.model
    ));

    let mut resume = messages.clone();
    resume.push(Message::user_text(&task));
    agent::sanitize_tool_pairs(&mut resume);

    match run_pipeline(cfg, &binding, &task, yes, provider, Some(resume)).await {
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
            if !outcome.messages.is_empty() {
                *messages = outcome.messages.clone();
            } else {
                messages.push(Message::user_text(prompt));
            }
            agent::sanitize_tool_pairs(messages);
            *dirty = true;
            crate::lessons::maybe_spawn(cfg, prompt, &outcome);
            Ok(())
        }
        Err(e) => {
            let _ = db.finish_run(&run_id, "fail", 0, 0, 0, Some(&e.to_string()));
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
            crate::lessons::maybe_spawn(cfg, prompt, &fail);
            Err(e)
        }
    }
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
