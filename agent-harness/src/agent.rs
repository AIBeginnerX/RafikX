use std::future::Future;
use std::io::{self, Write};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::applog;
use crate::config::Config;
use crate::db::Db;
use crate::lifecycle::{ApprovalDecision, LifecycleEventData, LifecycleState};
use crate::provider::{
    ChatRequest, ChatResponse, ContentBlock, Message, Role, StopReason, StreamEvent,
};
use crate::run::{RunContext, RunId};
use crate::tools::{self, ToolCtx, ToolRegistry};

pub const HARD_CAP: u32 = 50;
pub const AGENT_MAX_ITER: u32 = 25;

/// 한 응답의 도구 호출 목록 앞부분에서 병렬로 묶을 수 있는 연속 `task` 구간의 길이.
/// 2 미만이면 0 — 호출부는 기존 순차 경로를 그대로 탄다.
///
/// 병렬화 대상을 task 하나로 한정하는 이유: task 는 자식 RunContext·자식 승인 범위를
/// 이미 갖고 워크스페이스를 직접 만지지 않는다. 파일·셸 도구는 서로의 결과에 의존하므로
/// 순서를 보존해야 한다.
pub fn leading_task_span(names: &[&str]) -> usize {
    let n = names
        .iter()
        .take_while(|name| **name == tools::TaskTool::NAME)
        .count();
    if n >= 2 { n } else { 0 }
}

/// 직전과 같은 (도구, 입력) 호출의 연속 횟수를 갱신하고, 3회 연속이면 true.
/// 사이에 다른 호출이 끼면 스트릭이 리셋된다 — "고치고 같은 검증 재실행" 허용,
/// 같은 호출만 제자리 반복하는 진짜 루프만 차단한다.
pub fn same_call_repeated(last: &mut Option<String>, streak: &mut u32, key: String) -> bool {
    if last.as_deref() == Some(key.as_str()) {
        *streak += 1;
    } else {
        *last = Some(key);
        *streak = 1;
    }
    *streak >= 3
}

#[derive(Clone)]
pub struct AgentOutcome {
    pub status: String,
    pub iterations: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// 마지막 모델 요청이 실제로 사용한 입력 컨텍스트 토큰
    pub context_tokens: u32,
    /// 마지막 모델 요청에서 재사용한 프롬프트 캐시 토큰
    pub cached_tokens: u32,
    pub cache_reported: bool,
    pub error: Option<String>,
    pub messages: Vec<Message>,
    pub changed_files: Vec<String>,
    pub tool_errors: Vec<String>,
    pub deny_reasons: Vec<String>,
    /// 검증 명령의 **최종** 실패. 재시도로 통과했으면 None 이다 (설계 §15.4).
    pub verify_fail: Option<String>,
    /// 검증이 한 번 실패했다가 재시도로 통과한 경우의 첫 실패 사유.
    /// 성공 판정을 흐리지 않으면서 "실패 후 회복" 교훈 수집 경로를 유지한다.
    pub verify_recovered: Option<String>,
}

impl Default for AgentOutcome {
    fn default() -> Self {
        Self {
            status: "ok".into(),
            iterations: 0,
            input_tokens: 0,
            output_tokens: 0,
            context_tokens: 0,
            cached_tokens: 0,
            cache_reported: false,
            error: None,
            messages: Vec::new(),
            changed_files: Vec::new(),
            tool_errors: Vec::new(),
            deny_reasons: Vec::new(),
            verify_fail: None,
            verify_recovered: None,
        }
    }
}

pub struct AgentRun<'a> {
    pub cfg: &'a Config,
    pub provider_name: &'a str,
    pub model: &'a str,
    /// 콤보 체인 (F8) — 비어 있지 않으면 fallback_order 대신 이 쌍들로 전환한다.
    pub combo_chain: Vec<(String, String)>,
    pub task: &'a str,
    pub yes: bool,
    pub max_iterations: u32,
    pub system: String,
    pub registry: ToolRegistry,
    pub resume: Option<Vec<Message>>,
    pub remote: Option<RemoteApproval>,
    pub local_ask: Option<LocalAsk>,
    pub context_window: u32,
}

#[derive(Clone)]
pub struct RemoteApproval {
    pub timeout: Duration,
    pub ask: AskFn,
}

/** 원격 승인 콜백: 질의 문자열을 받아 승인 여부를 비동기로 반환한다. */
pub type AskFn = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalChoice {
    Yes,
    No,
    Always,
}

pub type LocalAsk =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ApprovalChoice> + Send>> + Send + Sync>;

enum Approval {
    Yes,
    No,
    Always,
}

/// working 패널의 이 실행 줄에 "지금 하는 일"만 갱신한다.
/// 역할·모델은 파이프라인(또는 task.rs)이 이미 채웠으므로 빈 값으로 두어 유지시킨다.
fn worker_activity(run: &RunContext, activity: &str) {
    crate::ui::live_worker_in(run, &crate::ui::worker_id(run), "", "", activity, false);
}

pub async fn run_agent(run: AgentRun<'_>) -> Result<AgentOutcome> {
    let context = RunContext::isolated(
        RunId::new(format!("agent-{}", crate::db::Db::new_id())),
        run.cfg.workspace.clone(),
    );
    run_agent_with_context(run, context).await
}

pub async fn run_agent_with_context(
    run: AgentRun<'_>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    let AgentRun {
        cfg,
        provider_name,
        model,
        task,
        yes,
        max_iterations,
        system,
        registry,
        resume,
        remote,
        local_ask,
        context_window,
        combo_chain,
    } = run;

    if run_context.lifecycle_state() == Some(LifecycleState::Queued) {
        let _ = run_context.transition_lifecycle(LifecycleEventData::RunStarted {
            model: Some(model.to_string()),
        });
    }

    // 원격(텔레그램)에서는 yolo/--yes 를 코드 수준에서 금지
    let yes = effective_yes(yes, &remote);
    if yes {
        run_context.approve_run_tree();
    }

    if !cfg.workspace.exists() {
        std::fs::create_dir_all(&cfg.workspace)?;
        crate::ui::live_line_in(
            &run_context,
            &format!(
                "워크스페이스 폴더를 만들었습니다: {}",
                cfg.workspace.display()
            ),
        );
    }
    warn_if_not_git(&run_context, &cfg.workspace);

    if yes {
        crate::ui::live_warn_in(
            &run_context,
            "경고: --yes 는 모든 도구를 승인 없이 실행합니다.",
        );
    }

    let mut ctx = ToolCtx::new(cfg.workspace.clone());
    ctx.vault = Some(crate::config::expand_tilde(&cfg.file.obsidian.vault_path));
    ctx.db_path = crate::config::expand_tilde(&cfg.file.obsidian.db_path);
    ctx.hashline = cfg.file.edit.hashline;
    ctx.local_ask = local_ask.clone();
    ctx.remote = remote.clone();
    ctx.run = Some(run_context.clone());
    let mut messages = resume.unwrap_or_else(|| vec![Message::user_text(task)]);
    let mut allow_all = yes || run_context.run_tree_approved();
    let mut iterations = 0u32;
    let mut input_tokens = 0u32;
    let mut output_tokens = 0u32;
    let mut cached_tokens = 0u32;
    let mut cache_reported = false;
    let mut context_tokens = 0u32;
    let mut denied_any = false;
    // 직전과 같은 (도구, 입력) 호출이 몇 번 연속됐는지. 사이에 다른 호출이 끼면
    // 리셋한다 — "고치고 같은 검증을 재실행" 같은 정상 루프를 막지 않기 위해서다.
    // (누적 카운트는 20회 반복 작업에서 정당한 재실행 3회째를 오차단했다: 2026-08-27 실측)
    let mut last_call_key: Option<String> = None;
    let mut same_call_streak: u32 = 0;
    let mut tool_errors: Vec<String> = Vec::new();
    let mut deny_reasons: Vec<String> = Vec::new();
    let mut truncation_retries = 0u8;
    let max_iter = max_iterations.min(HARD_CAP);

    loop {
        if run_context.is_cancelled() {
            return Ok(AgentOutcome {
                status: "cancelled".into(),
                iterations,
                input_tokens,
                output_tokens,
                context_tokens,
                cached_tokens,
                cache_reported,
                error: Some("실행이 취소되었습니다.".into()),
                messages,
                changed_files: committed_files(&run_context),
                tool_errors,
                deny_reasons,
                verify_fail: None,
                verify_recovered: None,
            });
        }
        if iterations >= max_iter {
            crate::ui::live_line_in(&run_context, "상한 도달, 여기까지 결과");
            return Ok(AgentOutcome {
                status: "limit".into(),
                iterations,
                input_tokens,
                output_tokens,
                context_tokens,
                cached_tokens,
                cache_reported,
                error: Some("반복 상한".into()),
                messages,
                changed_files: committed_files(&run_context),
                tool_errors,
                deny_reasons,
                verify_fail: None,
                verify_recovered: None,
            });
        }
        iterations += 1;
        let _ = run_context.transition_lifecycle(LifecycleEventData::Iteration {
            current: iterations,
            max: max_iter,
        });
        crate::spinner::set_label_in(
            &run_context,
            &format!("반복 {iterations}/{max_iter} · 모델 호출"),
        );
        worker_activity(&run_context, &format!("반복 {iterations}/{max_iter}"));
        let specs = registry.specs();
        messages = crate::packer::pack_messages(
            &messages,
            &system,
            &specs,
            context_window,
            cfg.file.general.max_tokens,
            cfg.file.general.max_context_chars,
        );
        crate::graph::node_in(
            &run_context,
            "pre_step",
            &format!("iter {iterations}"),
            &format!("msgs={} window={context_window}", messages.len()),
            Some("bind"),
        );

        let req = ChatRequest {
            model: model.to_string(),
            system: system.clone(),
            messages: messages.clone(),
            tools: registry.specs(),
            max_tokens: cfg.file.general.max_tokens,
            stream: true,
        };

        // 프로바이더 폴백: 주 연결 실패(4xx·5xx·리밋) 시 fallback_order 의 다음 연결로.
        // 주 연결은 원래 모델을 그대로 쓰고, 이후 연결은 role(main) 기준 모델을 쓴다.
        // 콤보 바인딩이면 체인이 폴 fallback 순서와 모델을 결정한다 (F8).
        let order = if combo_chain.is_empty() {
            crate::harness::fallback_order_pinned(cfg, provider_name, None)
        } else {
            let mut v: Vec<String> = Vec::new();
            for (p, _) in &combo_chain {
                if !v.contains(p) {
                    v.push(p.clone());
                }
            }
            v
        };
        let mut streamed = false;
        let (_used, resp) = {
            let on_event = |ev: crate::provider::StreamEvent| match ev {
                StreamEvent::Text(piece) => {
                    streamed = true;
                    crate::ui::live_chunk_in(&run_context, piece);
                }
                // 대형 tool call 인자를 쓰는 동안은 텍스트가 없다 — 진행만 갱신한다.
                StreamEvent::ToolArgs { name, total_bytes } => {
                    let label = crate::harness::tool_args_label(name, total_bytes);
                    crate::ui::live_status_in(&run_context, &label);
                    worker_activity(&run_context, &label);
                }
            };
            let response = if combo_chain.is_empty() {
                Box::pin(crate::harness::stream_with_fallback(cfg, &order, "main", req, on_event))
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Result<(String, crate::provider::ChatResponse)>> + Send + '_>>
            } else {
                Box::pin(crate::harness::stream_with_fallback_combo(cfg, &combo_chain, "main", req, on_event))
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Result<(String, crate::provider::ChatResponse)>> + Send + '_>>
            };
            tokio::pin!(response);
            tokio::select! {
                result = &mut response => result?,
                _ = run_context.cancelled_reason() => {
                    return Ok(AgentOutcome {
                        status: "cancelled".into(),
                        iterations,
                        input_tokens,
                        output_tokens,
                        context_tokens,
                        cached_tokens,
                        cache_reported,
                        error: Some("실행이 취소되었습니다.".into()),
                        messages,
                        changed_files: committed_files(&run_context),
                        tool_errors,
                        deny_reasons,
                        verify_fail: None,
                        verify_recovered: None,
                    });
                }
            }
        };
        crate::graph::node_in(
            &run_context,
            "request",
            model,
            &format!("in={} out={}", resp.input_tokens, resp.output_tokens),
            Some("pre_step"),
        );
        input_tokens += resp.input_tokens;
        output_tokens += resp.output_tokens;
        cached_tokens = resp.cached_tokens;
        cache_reported = resp.cache_reported;
        context_tokens = if provider_name == "anthropic" {
            resp.input_tokens.saturating_add(resp.cached_tokens)
        } else {
            resp.input_tokens
        };
        let _ = run_context.transition_lifecycle(LifecycleEventData::Tokens {
            input: resp.input_tokens,
            output: resp.output_tokens,
            cached: resp.cached_tokens,
        });
        crate::ui::live_status_in(
            &run_context,
            &format!(
                "[tokens] total_in={} total_out={} context={} cache={}",
                input_tokens,
                output_tokens,
                context_tokens,
                if cache_reported {
                    cached_tokens.to_string()
                } else {
                    "n/a".into()
                }
            ),
        );

        if !streamed {
            print_text_blocks(&run_context, &resp);
        }

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

        // 출력 상한(max_tokens)에서 잘려 도구 호출까지 유실된 응답 — 종료하지 말고
        // 모델에게 잘렸다는 사실과 분할 전략을 알려준 뒤 같은 런에서 이어간다.
        if resp.stop_reason == StopReason::MaxTokens
            && tool_uses.is_empty()
            && truncation_retries < 2
        {
            truncation_retries += 1;
            if !resp.content.is_empty() {
                messages.push(Message {
                    role: Role::Assistant,
                    content: resp.content.clone(),
                });
            }
            messages.push(Message::user_text(
                "[시스템] 직전 응답이 출력 토큰 상한(max_tokens)에 걸려 중간에 잘렸고, \
                 잘린 도구 호출은 실행되지 않았다. 같은 내용을 통째로 다시 출력하지 마라. \
                 큰 파일은 write_file 로 앞부분만 먼저 만들고, 나머지는 edit_file 이나 \
                 apply_patch 로 여러 번에 나눠 이어 붙여라.",
            ));
            crate::ui::live_warn_in(
                &run_context,
                "출력이 토큰 상한에서 잘렸습니다 — 분할 지시 후 재시도합니다.",
            );
            continue;
        }

        if resp.stop_reason != StopReason::ToolUse && tool_uses.is_empty() {
            messages.push(Message {
                role: Role::Assistant,
                content: resp.content.clone(),
            });
            let hit_token_limit = resp.stop_reason == StopReason::MaxTokens;
            let _ = run_context.transition_lifecycle(LifecycleEventData::AnswerStarted);
            return Ok(AgentOutcome {
                status: if hit_token_limit {
                    "incomplete".into()
                } else if denied_any {
                    "denied".into()
                } else {
                    "ok".into()
                },
                iterations,
                input_tokens,
                output_tokens,
                context_tokens,
                cached_tokens,
                cache_reported,
                error: hit_token_limit.then(|| "모델 출력 토큰 상한에 도달했습니다.".into()),
                messages,
                changed_files: committed_files(&run_context),
                tool_errors,
                deny_reasons,
                verify_fail: None,
                verify_recovered: None,
            });
        }

        if tool_uses.is_empty() {
            messages.push(Message {
                role: Role::Assistant,
                content: resp.content.clone(),
            });
            return Ok(AgentOutcome {
                status: "incomplete".into(),
                iterations,
                input_tokens,
                output_tokens,
                context_tokens,
                cached_tokens,
                cache_reported,
                error: Some("모델이 실행할 도구 호출을 반환하지 않았습니다.".into()),
                messages,
                changed_files: committed_files(&run_context),
                tool_errors,
                deny_reasons,
                verify_fail: None,
                verify_recovered: None,
            });
        }

        messages.push(Message {
            role: Role::Assistant,
            content: resp.content.clone(),
        });

        let mut results: Vec<ContentBlock> = Vec::new();
        let mut pending: std::collections::VecDeque<(String, String, serde_json::Value)> =
            tool_uses.into_iter().collect();
        while !pending.is_empty() {
            // 팀 모드가 노리는 병렬 위임: 승인이 필요 없는 상태에서만 연속 task 호출을 한
            // 묶음으로 동시에 돌린다. 승인이 필요하면(allow_all=false) 프리뷰 순서를 지켜야
            // 하므로 기존 순차 경로를 그대로 탄다.
            // 결과는 tool_use 순서대로 되돌려 API 규격(id 짝)을 지킨다.
            let span = if allow_all && registry.get(tools::TaskTool::NAME).is_some() {
                let names: Vec<&str> = pending.iter().map(|(_, name, _)| name.as_str()).collect();
                leading_task_span(&names)
            } else {
                0
            };
            if span >= 2 {
                if run_context.is_cancelled() {
                    return Ok(AgentOutcome {
                        status: "cancelled".into(),
                        iterations,
                        input_tokens,
                        output_tokens,
                        context_tokens,
                        cached_tokens,
                        cache_reported,
                        error: Some("실행이 취소되었습니다.".into()),
                        messages,
                        changed_files: committed_files(&run_context),
                        tool_errors,
                        deny_reasons,
                        verify_fail: None,
                        verify_recovered: None,
                    });
                }
                let batch: Vec<(String, String, serde_json::Value)> =
                    pending.drain(..span).collect();
                // 동일 도구·입력 연속 3회 차단은 병렬 구간에서도 그대로 적용한다.
                let mut repeated = false;
                for (_, name, input) in &batch {
                    repeated |= same_call_repeated(
                        &mut last_call_key,
                        &mut same_call_streak,
                        format!("{name}:{input}"),
                    );
                }
                if repeated {
                    crate::ui::live_line_in(
                        &run_context,
                        "동일 도구·입력이 3회 연속 반복되어 중단합니다.",
                    );
                    return Ok(AgentOutcome {
                        status: "limit".into(),
                        iterations,
                        input_tokens,
                        output_tokens,
                        context_tokens,
                        cached_tokens,
                        cache_reported,
                        error: Some("동일 도구 3회 연속 반복".into()),
                        messages,
                        changed_files: committed_files(&run_context),
                        tool_errors,
                        deny_reasons,
                        verify_fail: None,
                        verify_recovered: None,
                    });
                }
                crate::ui::live_line_in(&run_context, &format!("[병렬] task {span}건 동시 위임"));
                crate::spinner::set_label_in(
                    &run_context,
                    &format!("도구 실행: task ×{span} (병렬)"),
                );
                let prepared: Vec<_> = batch
                    .iter()
                    .map(|(_, name, input)| {
                        let _ = run_context.transition_lifecycle(LifecycleEventData::ToolStarted {
                            name: name.clone(),
                        });
                        crate::graph::node_in(
                            &run_context,
                            "tool_pre",
                            name,
                            "parallel",
                            Some("request"),
                        );
                        tools::TaskTool::parse_args(input, &ctx)
                    })
                    .collect();
                // 인자 파싱이 실패한 호출만 오류 결과가 되고 나머지는 그대로 병렬 실행된다.
                let outs =
                    futures_util::future::join_all(prepared.into_iter().map(|args| async move {
                        match args {
                            Ok(args) => tools::TaskTool::run_async(args).await,
                            Err(e) => Err(e),
                        }
                    }))
                    .await;
                for ((id, name, _), out) in batch.into_iter().zip(outs) {
                    match out {
                        Ok(text) => {
                            crate::graph::node_in(
                                &run_context,
                                "tool_post",
                                &name,
                                "ok",
                                Some("tool_pre"),
                            );
                            crate::ui::live_line_in(
                                &run_context,
                                &tool_output_summary(&name, &text),
                            );
                            results.push(ContentBlock::ToolResult {
                                tool_use_id: id,
                                content: text,
                                is_error: false,
                            });
                            let _ = run_context.transition_lifecycle(
                                LifecycleEventData::ToolFinished { name, ok: true },
                            );
                        }
                        Err(e) => {
                            crate::graph::node_in(
                                &run_context,
                                "tool_post",
                                &name,
                                "error",
                                Some("tool_pre"),
                            );
                            applog::error(&format!("tool {name}: {e}"));
                            crate::ui::live_line_in(&run_context, &format!("도구 오류: {e}"));
                            tool_errors.push(format!("{name}: {e}"));
                            results.push(ContentBlock::ToolResult {
                                tool_use_id: id,
                                content: e.to_string(),
                                is_error: true,
                            });
                            let _ = run_context.transition_lifecycle(
                                LifecycleEventData::ToolFinished { name, ok: false },
                            );
                        }
                    }
                }
                continue;
            }
            let Some((id, name, input)) = pending.pop_front() else {
                break;
            };
            if run_context.is_cancelled() {
                return Ok(AgentOutcome {
                    status: "cancelled".into(),
                    iterations,
                    input_tokens,
                    output_tokens,
                    context_tokens,
                    cached_tokens,
                    cache_reported,
                    error: Some("실행이 취소되었습니다.".into()),
                    messages,
                    changed_files: committed_files(&run_context),
                    tool_errors,
                    deny_reasons,
                    verify_fail: None,
                    verify_recovered: None,
                });
            }
            let key = format!("{name}:{}", input);
            if same_call_repeated(&mut last_call_key, &mut same_call_streak, key) {
                crate::ui::live_line_in(
                    &run_context,
                    "동일 도구·입력이 3회 연속 반복되어 중단합니다.",
                );
                return Ok(AgentOutcome {
                    status: "limit".into(),
                    iterations,
                    input_tokens,
                    output_tokens,
                    context_tokens,
                    cached_tokens,
                    cache_reported,
                    error: Some("동일 도구 3회 연속 반복".into()),
                    messages,
                    changed_files: committed_files(&run_context),
                    tool_errors,
                    deny_reasons,
                    verify_fail: None,
                    verify_recovered: None,
                });
            }

            crate::ui::live_line_in(&run_context, &format!("[도구] {name}"));
            worker_activity(&run_context, &format!("[도구] {name}"));
            let _ = run_context
                .transition_lifecycle(LifecycleEventData::ToolStarted { name: name.clone() });
            crate::spinner::set_label_in(&run_context, &format!("도구 실행: {name}"));
            crate::graph::node_in(&run_context, "tool_pre", &name, "", Some("request"));
            let Some(tool) = registry.get(&name) else {
                let msg = format!("알 수 없는 도구: {name}");
                tool_errors.push(msg.clone());
                results.push(ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: msg,
                    is_error: true,
                });
                let _ = run_context
                    .transition_lifecycle(LifecycleEventData::ToolFinished { name, ok: false });
                continue;
            };

            if tool.needs_approval(&input) && !allow_all {
                match tools::approval_preview(tool.name(), &input, &ctx) {
                    Ok(preview) => {
                        crate::ui::live_line_in(&run_context, &preview);
                        let approval_id = format!("approval-{}", Db::new_id());
                        let _ = run_context.transition_lifecycle(
                            LifecycleEventData::ApprovalRequested {
                                approval_id: approval_id.clone(),
                                preview: preview.clone(),
                            },
                        );
                        let Some(choice) = decide_approval_or_cancel(
                            &remote,
                            &local_ask,
                            preview.clone(),
                            &run_context,
                        )
                        .await?
                        else {
                            return Ok(AgentOutcome {
                                status: "cancelled".into(),
                                iterations,
                                input_tokens,
                                output_tokens,
                                context_tokens,
                                cached_tokens,
                                cache_reported,
                                error: Some("실행이 취소되었습니다.".into()),
                                messages,
                                changed_files: committed_files(&run_context),
                                tool_errors,
                                deny_reasons,
                                verify_fail: None,
                                verify_recovered: None,
                            });
                        };
                        let decision = match &choice {
                            Approval::Yes => ApprovalDecision::Yes,
                            Approval::No => ApprovalDecision::No,
                            Approval::Always => ApprovalDecision::Always,
                        };
                        let _ = run_context.transition_lifecycle(
                            LifecycleEventData::ApprovalResolved {
                                approval_id,
                                decision,
                            },
                        );
                        match choice {
                            choice @ (Approval::Yes | Approval::Always) => {
                                let refreshed = tools::approval_preview(tool.name(), &input, &ctx)?;
                                if refreshed != preview {
                                    let message = "승인 후 대상 상태가 바뀌어 실행을 중단했습니다. 최신 프리뷰로 다시 시도하세요.".to_string();
                                    tool_errors.push(message.clone());
                                    results.push(ContentBlock::ToolResult {
                                        tool_use_id: id,
                                        content: message,
                                        is_error: true,
                                    });
                                    let _ = run_context.transition_lifecycle(
                                        LifecycleEventData::ToolFinished {
                                            name: name.clone(),
                                            ok: false,
                                        },
                                    );
                                    continue;
                                }
                                if matches!(choice, Approval::Always) {
                                    allow_all = true;
                                    run_context.approve_run_tree();
                                }
                            }
                            Approval::No => {
                                denied_any = true;
                                let reason = if remote.is_some() || local_ask.is_some() {
                                    String::new()
                                } else {
                                    read_deny_reason()?
                                };
                                let msg = if reason.is_empty() {
                                    "사용자가 도구 실행을 거부했습니다.".to_string()
                                } else {
                                    format!("사용자가 도구 실행을 거부했습니다. 사유: {reason}")
                                };
                                deny_reasons.push(if reason.is_empty() {
                                    "거부".into()
                                } else {
                                    reason
                                });
                                results.push(ContentBlock::ToolResult {
                                    tool_use_id: id,
                                    content: msg,
                                    is_error: true,
                                });
                                let _ = run_context.transition_lifecycle(
                                    LifecycleEventData::ToolFinished {
                                        name: name.clone(),
                                        ok: false,
                                    },
                                );
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
                        tool_errors.push(e.to_string());
                        let _ =
                            run_context.transition_lifecycle(LifecycleEventData::ToolFinished {
                                name: name.clone(),
                                ok: false,
                            });
                        continue;
                    }
                }
            } else if tool.needs_approval(&input) && allow_all {
                crate::ui::live_warn_in(&run_context, &format!("[자동승인] {name}"));
            }

            match tool.run(input.clone(), &ctx) {
                Ok(out) => {
                    crate::graph::node_in(&run_context, "tool_post", &name, "ok", Some("tool_pre"));
                    if name != "todo_write" {
                        // 도구 출력 원문 전량 투척 대신 요약 한 줄 (pi 저소음).
                        crate::ui::live_line_in(&run_context, &tool_output_summary(&name, &out));
                    }
                    results.push(ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: out,
                        is_error: false,
                    });
                    let _ = run_context.transition_lifecycle(LifecycleEventData::ToolFinished {
                        name: name.clone(),
                        ok: true,
                    });
                }
                Err(e) => {
                    crate::graph::node_in(
                        &run_context,
                        "tool_post",
                        &name,
                        "error",
                        Some("tool_pre"),
                    );
                    applog::error(&format!("tool {name}: {e}"));
                    crate::ui::live_line_in(&run_context, &format!("도구 오류: {e}"));
                    tool_errors.push(format!("{name}: {e}"));
                    results.push(ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: e.to_string(),
                        is_error: true,
                    });
                    let _ = run_context.transition_lifecycle(LifecycleEventData::ToolFinished {
                        name: name.clone(),
                        ok: false,
                    });
                }
            }
        }

        messages.push(Message {
            role: Role::User,
            content: results,
        });
        // 도구가 실제로 실행된 반복 — 진전이 있었으므로 절단 재시도 카운터를 되돌린다.
        truncation_retries = 0;
    }
}

/// 도구 결과를 트랜스크립트용 한 줄로 압축한다 — 짧으면 그대로, 길면
/// 첫 줄 + 규모만. 원문은 모델에게 전달되는 ToolResult 에 온전히 남는다.
fn tool_output_summary(name: &str, out: &str) -> String {
    let trimmed = out.trim_end();
    let lines = trimmed.lines().count();
    if lines <= 2 && trimmed.chars().count() <= 160 {
        return trimmed.to_string();
    }
    let first = trimmed.lines().next().unwrap_or("");
    let first: String = first.chars().take(120).collect();
    format!("{first}  … ({name} 결과 {lines}줄)")
}

fn committed_files(run: &RunContext) -> Vec<String> {
    run.committed_paths()
        .into_iter()
        .map(|path| run.workspace_relative(&path).to_string_lossy().into_owned())
        .collect()
}

fn print_text_blocks(run: &RunContext, resp: &ChatResponse) {
    for b in &resp.content {
        if let ContentBlock::Text { text } = b
            && !text.trim().is_empty()
        {
            crate::ui::live_assistant_in(run, text);
        }
    }
}

fn warn_if_not_git(run: &RunContext, workspace: &Path) {
    if !workspace.join(".git").exists() {
        crate::ui::live_warn_in(
            run,
            "경고: 이 폴더는 git 저장소가 아닙니다. git init 을 권장합니다.",
        );
    }
}

fn read_deny_reason() -> Result<String> {
    print!("사유(선택, Enter로 생략): ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

pub fn sanitize_tool_pairs(messages: &mut Vec<Message>) {
    let mut keep: Vec<Message> = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        let has_use = messages[i]
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
        let has_result = messages[i]
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
        if messages[i].role == Role::Assistant && has_use {
            if i + 1 < messages.len()
                && messages[i + 1].role == Role::User
                && messages[i + 1]
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
            {
                keep.push(messages[i].clone());
                keep.push(messages[i + 1].clone());
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if messages[i].role == Role::User
            && has_result
            && !messages[i]
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { .. }))
        {
            i += 1;
            continue;
        }
        keep.push(messages[i].clone());
        i += 1;
    }
    *messages = keep;
}

fn parse_approval(input: &str) -> Option<Approval> {
    match input.trim().to_lowercase().as_str() {
        "y" | "yes" => Some(Approval::Yes),
        "n" | "no" => Some(Approval::No),
        "a" | "all" => Some(Approval::Always),
        _ => None,
    }
}

fn read_approval() -> Result<Approval> {
    loop {
        print!("[y] 이번만  / [n] 거부  / [a] 이번 실행 모두 허용 : ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        let n = io::stdin().read_line(&mut line)?;
        // 비대화형 stdin EOF — 재질문 루프는 무한 스핀이 된다. 안전하게 거부로 닫는다.
        if n == 0 {
            println!("(입력이 닫혔습니다 — 거부로 처리)");
            return Ok(Approval::No);
        }
        match parse_approval(&line) {
            Some(a) => return Ok(a),
            None => println!("y / n / a 중에서 고르세요."),
        }
    }
}

async fn decide_approval(
    remote: &Option<RemoteApproval>,
    local_ask: &Option<LocalAsk>,
    preview: String,
) -> Result<Approval> {
    if let Some(ask) = local_ask {
        return Ok(match ask(preview).await {
            ApprovalChoice::Yes => Approval::Yes,
            ApprovalChoice::No => Approval::No,
            ApprovalChoice::Always => Approval::Always,
        });
    }
    let Some(r) = remote else {
        return read_approval();
    };
    let ask = r.ask.clone();
    match tokio::time::timeout(r.timeout, (ask)(preview)).await {
        Ok(true) => Ok(Approval::Yes),
        _ => Ok(Approval::No),
    }
}

async fn decide_approval_or_cancel(
    remote: &Option<RemoteApproval>,
    local_ask: &Option<LocalAsk>,
    preview: String,
    run: &RunContext,
) -> Result<Option<Approval>> {
    tokio::select! {
        result = decide_approval(remote, local_ask, preview) => result.map(Some),
        _ = run.cancelled_reason() => Ok(None),
    }
}

pub fn effective_yes(yes: bool, remote: &Option<RemoteApproval>) -> bool {
    if remote.is_some() { false } else { yes }
}

#[allow(dead_code)]
pub fn assistant_text(messages: &[Message]) -> String {
    let mut parts = Vec::new();
    for m in messages {
        if m.role != Role::Assistant {
            continue;
        }
        for b in &m.content {
            if let ContentBlock::Text { text } = b
                && !text.trim().is_empty()
            {
                parts.push(text.as_str());
            }
        }
    }
    parts.join("\n")
}

/// 마지막 assistant 메시지의 텍스트만 이어붙인다.
/// 판정을 읽어야 하는 곳(검증자 게이트)은 전체 발화가 아니라 이 결론만 본다 —
/// 1회차의 "…판정하겠다" 같은 중간 발화가 결론으로 오인되면 안 된다.
pub fn last_assistant_text(messages: &[Message]) -> String {
    let Some(last) = messages.iter().rev().find(|m| m.role == Role::Assistant) else {
        return String::new();
    };
    last.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn record_finish(db: &Db, run_id: &str, outcome: &AgentOutcome) -> Result<()> {
    db.finish_run(
        run_id,
        &outcome.status,
        outcome.iterations as i64,
        outcome.input_tokens as i64,
        outcome.output_tokens as i64,
        outcome.error.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn same_call_streak_resets_when_another_call_intervenes() {
        let mut last = None;
        let mut streak = 0u32;
        let mut hit = |k: &str| super::same_call_repeated(&mut last, &mut streak, k.to_string());
        // 고치고 재실행하는 정상 루프 — 사이에 edit 가 끼면 무한정 허용.
        assert!(!hit("bash:test"));
        assert!(!hit("edit:a"));
        assert!(!hit("bash:test"));
        assert!(!hit("edit:b"));
        assert!(!hit("bash:test"));
        // 제자리 반복 — 같은 호출 3회 연속이면 차단.
        assert!(!hit("bash:loop"));
        assert!(!hit("bash:loop"));
        assert!(hit("bash:loop"));
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn last_assistant_text_takes_only_the_final_turn() {
        let msgs = vec![
            Message::user_text("판정하라"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "[판정] 은 파일을 읽고 내리겠다".into(),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "계속".into(),
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "확인 완료.".into(),
                    },
                    ContentBlock::Text {
                        text: "[판정] pass".into(),
                    },
                ],
            },
        ];
        let last = last_assistant_text(&msgs);
        // 마지막 assistant 턴만 — 1회차의 "판정하겠다" 발화가 섞이면 안 된다.
        assert!(last.contains("[판정] pass"));
        assert!(!last.contains("내리겠다"));
        // 같은 턴의 텍스트 블록은 모두 이어붙인다.
        assert!(last.contains("확인 완료."));
        // 전체 이어붙이기(assistant_text)는 두 발화를 모두 담는다 — 대비.
        assert!(assistant_text(&msgs).contains("내리겠다"));
        assert_eq!(last_assistant_text(&[]), "");
        assert_eq!(last_assistant_text(&[Message::user_text("x")]), "");
    }

    #[test]
    fn drops_unpaired_tool_use() {
        let mut msgs = vec![
            Message::user_text("hi"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "1".into(),
                    name: "read_file".into(),
                    input: json!({"path": "a.rs"}),
                }],
            },
        ];
        sanitize_tool_pairs(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0].role, Role::User));
    }

    #[test]
    fn keeps_paired_tool_use() {
        let mut msgs = vec![
            Message::user_text("hi"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "1".into(),
                    name: "read_file".into(),
                    input: json!({"path": "a.rs"}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "1".into(),
                    content: "ok".into(),
                    is_error: false,
                }],
            },
        ];
        sanitize_tool_pairs(&mut msgs);
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn groups_only_leading_runs_of_two_or_more_task_calls() {
        // 병렬 대상은 "선두의 연속 task 2개 이상"뿐이다.
        assert_eq!(leading_task_span(&["task", "task"]), 2);
        assert_eq!(leading_task_span(&["task", "task", "task"]), 3);
        assert_eq!(leading_task_span(&["task", "task", "read_file"]), 2);
        // 1개짜리는 병렬화하지 않는다 — 기존 순차 경로가 그대로 처리한다.
        assert_eq!(leading_task_span(&["task"]), 0);
        assert_eq!(leading_task_span(&["task", "read_file", "task"]), 0);
        // 선두가 task 가 아니면 0 — 앞선 도구의 부수효과 순서를 먼저 지킨다.
        assert_eq!(leading_task_span(&["read_file", "task", "task"]), 0);
        assert_eq!(leading_task_span(&[]), 0);
        assert_eq!(leading_task_span(&["write_file"]), 0);
    }

    #[test]
    fn remote_forces_yes_off() {
        let remote = Some(RemoteApproval {
            timeout: Duration::from_secs(1),
            ask: Arc::new(|_| Box::pin(async { true })),
        });
        assert!(!effective_yes(true, &remote));
        assert!(!effective_yes(false, &remote));
        assert!(effective_yes(true, &None));
        assert!(!effective_yes(false, &None));
    }
}


#[cfg(test)]
mod approval_eof_tests {
    use super::*;

    #[test]
    fn parses_approval_answers() {
        assert!(matches!(parse_approval("y"), Some(Approval::Yes)));
        assert!(matches!(parse_approval("Y"), Some(Approval::Yes)));
        assert!(matches!(parse_approval("no"), Some(Approval::No)));
        assert!(matches!(parse_approval("a"), Some(Approval::Always)));
        assert!(parse_approval("").is_none());
        assert!(parse_approval("뭐").is_none());
    }
}
