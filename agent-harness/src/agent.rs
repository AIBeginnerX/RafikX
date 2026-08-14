use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

use anyhow::Result;

use crate::applog;
use crate::config::Config;
use crate::db::Db;
use crate::provider::{
    AnthropicProvider, ChatRequest, ChatResponse, ContentBlock, Message, Role, StopReason,
};
use crate::tools::{self, ToolCtx, ToolRegistry};

pub const HARD_CAP: u32 = 50;
pub const AGENT_MAX_ITER: u32 = 25;

pub struct AgentOutcome {
    pub status: String,
    pub iterations: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub error: Option<String>,
}

enum Approval {
    Yes,
    No,
    Always,
}

pub async fn run_agent(
    cfg: &Config,
    client: &AnthropicProvider,
    model: &str,
    task: &str,
    yes: bool,
) -> Result<AgentOutcome> {
    if !cfg.workspace.exists() {
        std::fs::create_dir_all(&cfg.workspace)?;
        eprintln!("워크스페이스 폴더를 만들었습니다: {}", cfg.workspace.display());
    }
    warn_if_not_git(&cfg.workspace);

    if yes {
        eprintln!("경고: --yes 는 모든 도구를 승인 없이 실행합니다.");
    }

    let registry = ToolRegistry::all();
    let ctx = ToolCtx {
        workspace: cfg.workspace.clone(),
    };
    let mut messages = vec![Message::user_text(task)];
    let mut allow_all = yes;
    let mut iterations = 0u32;
    let mut input_tokens = 0u32;
    let mut output_tokens = 0u32;
    let mut denied_any = false;
    let mut call_counts: HashMap<String, u32> = HashMap::new();
    let max_iter = AGENT_MAX_ITER.min(HARD_CAP);
    let max_chars = cfg.file.general.max_context_chars;

    let system = format!(
        "You are a careful senior developer working in a local workspace.\n\
         Always read a file before editing it. Prefer the smallest possible change.\n\
         Workspace: {}\n\
         If the user writes in Korean, reply in Korean.\n\
         신중한 시니어 개발자다. 수정 전 반드시 원문을 읽고, 최소 diff로 고친다.",
        cfg.workspace.display()
    );

    loop {
        if iterations >= max_iter {
            println!("상한 도달, 여기까지 결과");
            return Ok(AgentOutcome {
                status: "limit".into(),
                iterations,
                input_tokens,
                output_tokens,
                error: Some("반복 상한".into()),
            });
        }
        iterations += 1;
        trim_history(&mut messages, max_chars);

        let req = ChatRequest {
            model: model.to_string(),
            system: system.clone(),
            messages: messages.clone(),
            tools: registry.specs(),
            max_tokens: cfg.file.general.max_tokens,
            stream: false,
        };

        let resp = client.chat(&req).await?;
        input_tokens += resp.input_tokens;
        output_tokens += resp.output_tokens;
        println!(
            "[tokens] in={} out={} (누적 in={} out={})",
            resp.input_tokens, resp.output_tokens, input_tokens, output_tokens
        );

        print_text_blocks(&resp);

        let tool_uses: Vec<(String, String, serde_json::Value)> = resp
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect();

        if resp.stop_reason != StopReason::ToolUse && tool_uses.is_empty() {
            return Ok(AgentOutcome {
                status: if denied_any { "denied".into() } else { "ok".into() },
                iterations,
                input_tokens,
                output_tokens,
                error: None,
            });
        }

        if tool_uses.is_empty() {
            return Ok(AgentOutcome {
                status: "ok".into(),
                iterations,
                input_tokens,
                output_tokens,
                error: None,
            });
        }

        messages.push(Message {
            role: Role::Assistant,
            content: resp.content.clone(),
        });

        let mut results: Vec<ContentBlock> = Vec::new();
        for (id, name, input) in tool_uses {
            let key = format!("{name}:{}", input);
            let count = call_counts.entry(key).or_insert(0);
            *count += 1;
            if *count >= 3 {
                println!("동일 도구·입력이 3회 반복되어 중단합니다.");
                return Ok(AgentOutcome {
                    status: "limit".into(),
                    iterations,
                    input_tokens,
                    output_tokens,
                    error: Some("동일 도구 3회 반복".into()),
                });
            }

            println!("[도구] {name}");
            let Some(tool) = registry.get(&name) else {
                results.push(ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: format!("알 수 없는 도구: {name}"),
                    is_error: true,
                });
                continue;
            };

            if tool.needs_approval(&input) && !allow_all {
                match tools::approval_preview(tool.name(), &input, &ctx) {
                    Ok(preview) => {
                        println!("{preview}");
                        match read_approval()? {
                            Approval::Yes => {}
                            Approval::Always => allow_all = true,
                            Approval::No => {
                                denied_any = true;
                                results.push(ContentBlock::ToolResult {
                                    tool_use_id: id,
                                    content: "사용자가 도구 실행을 거부했습니다.".into(),
                                    is_error: true,
                                });
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        results.push(ContentBlock::ToolResult {
                            tool_use_id: id,
                            content: e.to_string(),
                            is_error: true,
                        });
                        continue;
                    }
                }
            } else if tool.needs_approval(&input) && allow_all {
                eprintln!("[자동승인] {name}");
            }

            match tool.run(input, &ctx) {
                Ok(out) => {
                    println!("{out}");
                    results.push(ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: out,
                        is_error: false,
                    });
                }
                Err(e) => {
                    applog::error(&format!("tool {name}: {e}"));
                    println!("도구 오류: {e}");
                    results.push(ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: e.to_string(),
                        is_error: true,
                    });
                }
            }
        }

        messages.push(Message {
            role: Role::User,
            content: results,
        });
    }
}

fn print_text_blocks(resp: &ChatResponse) {
    for b in &resp.content {
        if let ContentBlock::Text { text } = b {
            if !text.trim().is_empty() {
                println!("{text}");
            }
        }
    }
}

fn warn_if_not_git(workspace: &Path) {
    if !workspace.join(".git").exists() {
        eprintln!("경고: 이 폴더는 git 저장소가 아닙니다. git init 을 권장합니다.");
    }
}

fn read_approval() -> Result<Approval> {
    loop {
        print!("[y] 이번만  / [n] 거부  / [a] 이번 실행 모두 허용 : ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        match line.trim().to_lowercase().as_str() {
            "y" | "yes" => return Ok(Approval::Yes),
            "n" | "no" => return Ok(Approval::No),
            "a" | "all" => return Ok(Approval::Always),
            _ => println!("y / n / a 중에서 고르세요."),
        }
    }
}

fn message_chars(m: &Message) -> usize {
    m.content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolUse { id, name, input } => {
                id.len() + name.len() + input.to_string().len()
            }
            ContentBlock::ToolResult { content, .. } => content.len(),
        })
        .sum()
}

fn trim_history(messages: &mut Vec<Message>, max_chars: u32) {
    let max = max_chars as usize;
    let mut total: usize = messages.iter().map(message_chars).sum();
    while total > max && messages.len() > 1 {
        let has_tool_use = messages[0]
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
        let remove = if has_tool_use
            && messages.len() > 1
            && messages[1]
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
        {
            2
        } else {
            1
        };
        for _ in 0..remove {
            if messages.len() <= 1 {
                break;
            }
            let m = messages.remove(0);
            total = total.saturating_sub(message_chars(&m));
        }
    }
}

pub fn record_finish(
    db: &Db,
    run_id: &str,
    outcome: &AgentOutcome,
) -> Result<()> {
    db.finish_run(
        run_id,
        &outcome.status,
        outcome.iterations as i64,
        outcome.input_tokens as i64,
        outcome.output_tokens as i64,
        outcome.error.as_deref(),
    )
}
