use crate::agent;
use crate::config::ProviderConfig;
use crate::provider::{ContentBlock, Message, Role, ToolSpec};

const KEEP_TAIL: usize = 8;
const TOOL_RESULT_OMIT_NOTICE: &str = "\n[도구 결과 일부 생략]\n";
pub const AUTO_COMPACT_PERCENT: u32 = 80;

pub const fn needs_auto_compaction(used: u32, window: u32) -> bool {
    window > 0 && used.saturating_mul(100) >= window.saturating_mul(AUTO_COMPACT_PERCENT)
}

pub fn effective_output_limit(context_window: u32, requested: u32) -> u32 {
    if requested == 0 {
        0
    } else {
        requested.min(context_window / 2)
    }
}

/// 대략적인 토큰 수 (문자/4). 정밀 토크나이저는 쓰지 않는다.
pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

fn block_tokens(b: &ContentBlock) -> usize {
    match b {
        ContentBlock::Text { text } => estimate_tokens(text),
        ContentBlock::ToolUse { id, name, input } => {
            estimate_tokens(id) + estimate_tokens(name) + estimate_tokens(&input.to_string())
        }
        ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
    }
}

pub fn message_tokens(m: &Message) -> usize {
    m.content.iter().map(block_tokens).sum()
}

fn tools_tokens(tools: &[ToolSpec]) -> usize {
    tools
        .iter()
        .map(|t| {
            estimate_tokens(&t.name)
                + estimate_tokens(&t.description)
                + estimate_tokens(&t.input_schema.to_string())
        })
        .sum()
}

pub(crate) fn request_output_limit(
    system: &str,
    tools: &[ToolSpec],
    context_window: u32,
    requested: u32,
) -> u32 {
    let fixed_input = estimate_tokens(system).saturating_add(tools_tokens(tools));
    let available = (context_window as usize)
        .saturating_sub(fixed_input)
        .saturating_sub(1);
    effective_output_limit(context_window, requested).min(available as u32)
}

fn is_tool_use(m: &Message) -> bool {
    m.role == Role::Assistant
        && m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
}

fn is_tool_result(m: &Message) -> bool {
    m.role == Role::User
        && m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
}

fn drop_oldest_unit(msgs: &mut Vec<Message>) -> bool {
    if msgs.len() <= 1 {
        return false;
    }
    if agent::is_complete_tool_pair(&msgs[0], &msgs[1]) {
        if msgs.len() == 2 {
            return false;
        }
        msgs.drain(0..2);
        return true;
    }
    msgs.remove(0);
    true
}

fn has_omit_notice(msgs: &[Message]) -> bool {
    msgs.first().is_some_and(|m| {
        m.content.iter().any(|b| match b {
            ContentBlock::Text { text } => text.contains("[이전 대화 일부 생략]"),
            _ => false,
        })
    })
}

fn insert_notice(msgs: &mut Vec<Message>) {
    if has_omit_notice(msgs) {
        return;
    }
    msgs.insert(0, Message::user_text("[이전 대화 일부 생략]"));
}

fn truncate_oldest_text(msgs: &mut [Message], budget: usize) -> bool {
    for m in msgs.iter_mut() {
        if is_tool_use(m) || is_tool_result(m) {
            continue;
        }
        for b in &mut m.content {
            if let ContentBlock::Text { text } = b
                && estimate_tokens(text) > 32
            {
                let keep = (budget / 8).max(24);
                let cut: String = text.chars().take(keep * 4).collect();
                let truncated = format!("{cut}\n[이전 대화 일부 생략]");
                if estimate_tokens(&truncated) < estimate_tokens(text) {
                    *text = truncated;
                    return true;
                }
            }
        }
    }
    false
}

fn minimum_truncated_tool_result_tokens() -> usize {
    estimate_tokens(&format!("h{TOOL_RESULT_OMIT_NOTICE}t"))
}

fn truncate_tool_result(content: &mut String, max_tokens: usize) -> bool {
    if estimate_tokens(content) <= max_tokens {
        return false;
    }

    if max_tokens < minimum_truncated_tool_result_tokens() {
        return false;
    }

    let keep_chars = max_tokens
        .saturating_mul(4)
        .saturating_sub(TOOL_RESULT_OMIT_NOTICE.chars().count());
    let content_chars = content.chars().count();
    let head_chars = keep_chars.div_ceil(2).min(content_chars);
    let tail_chars = keep_chars
        .saturating_sub(head_chars)
        .min(content_chars.saturating_sub(head_chars));
    let head: String = content.chars().take(head_chars).collect();
    let tail: String = content
        .chars()
        .skip(content_chars.saturating_sub(tail_chars))
        .collect();
    let truncated = format!("{head}{TOOL_RESULT_OMIT_NOTICE}{tail}");
    if estimate_tokens(&truncated) >= estimate_tokens(content) {
        return false;
    }
    *content = truncated;
    true
}

/// `available` is shared by every result in one provider user-result message.
/// Small results keep their full contents; the remaining capacity is water-filled
/// across larger siblings so no result is starved by an earlier truncation.
fn allocate_tool_result_limits(result_tokens: &[usize], available: usize) -> Option<Vec<usize>> {
    let minimum = minimum_truncated_tool_result_tokens();
    let mut limits: Vec<usize> = result_tokens
        .iter()
        .map(|tokens| (*tokens).min(minimum))
        .collect();
    let baseline = limits.iter().sum::<usize>();
    if available < baseline {
        return None;
    }

    let mut remaining = available - baseline;
    while remaining > 0 {
        let active: Vec<usize> = limits
            .iter()
            .enumerate()
            .filter_map(|(index, limit)| (*limit < result_tokens[index]).then_some(index))
            .collect();
        if active.is_empty() {
            break;
        }

        let share = remaining.div_ceil(active.len());
        let mut granted = 0;
        for index in active {
            let grant = share
                .min(result_tokens[index].saturating_sub(limits[index]))
                .min(remaining.saturating_sub(granted));
            limits[index] += grant;
            granted += grant;
        }
        if granted == 0 {
            break;
        }
        remaining -= granted;
    }
    Some(limits)
}

fn truncate_tool_results_in_message(message: &mut Message, available: usize) -> bool {
    let result_tokens: Vec<usize> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(estimate_tokens(content)),
            _ => None,
        })
        .collect();
    let Some(limits) = allocate_tool_result_limits(&result_tokens, available) else {
        // The pair's fixed structure alone exceeds the budget. Emptying every
        // result is the deterministic fixed point that still preserves IDs,
        // order, errors, and the assistant/result adjacency contract.
        let mut changed = false;
        for block in &mut message.content {
            if let ContentBlock::ToolResult { content, .. } = block
                && !content.is_empty()
            {
                content.clear();
                changed = true;
            }
        }
        return changed;
    };

    let mut changed = false;
    let mut index = 0;
    for block in &mut message.content {
        if let ContentBlock::ToolResult { content, .. } = block {
            changed |= truncate_tool_result(content, limits[index]);
            index += 1;
        }
    }
    changed
}

fn truncate_newest_tool_result(msgs: &mut [Message], budget: usize) -> bool {
    let total = msgs.iter().map(message_tokens).sum::<usize>();
    for message in msgs.iter_mut().rev() {
        let result_tokens = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(estimate_tokens(content)),
                _ => None,
            })
            .sum::<usize>();
        if result_tokens == 0 {
            continue;
        }
        let fixed_tokens = total.saturating_sub(result_tokens);
        let available = budget.saturating_sub(fixed_tokens);
        if truncate_tool_results_in_message(message, available) {
            return true;
        }
    }
    false
}

fn drop_tool_pair_for_budget(msgs: &mut Vec<Message>, budget: usize) -> bool {
    let pair_index = msgs
        .windows(2)
        .position(|pair| {
            agent::is_complete_tool_pair(&pair[0], &pair[1])
                && message_tokens(&pair[0]).saturating_add(message_tokens(&pair[1])) > budget
        })
        .or_else(|| {
            msgs.windows(2)
                .position(|pair| agent::is_complete_tool_pair(&pair[0], &pair[1]))
        });
    let Some(pair_index) = pair_index else {
        return false;
    };

    msgs.drain(pair_index..pair_index + 2);
    true
}

/// 출력 토큰을 남기고, 시스템·도구·최근 대화를 우선 유지한다.
/// 도구 짝은 깨지 않는다. 잘리면 프롬프트에 한 줄 안내를 넣는다.
pub fn pack_messages(
    messages: &[Message],
    system: &str,
    tools: &[ToolSpec],
    context_window: u32,
    reserve_output: u32,
    max_context_chars: u32,
) -> Vec<Message> {
    let mut msgs = messages.to_vec();
    agent::sanitize_tool_pairs(&mut msgs);

    let window = context_window as usize;
    let output_reserve =
        request_output_limit(system, tools, context_window, reserve_output) as usize;
    let reserved = estimate_tokens(system) + tools_tokens(tools) + output_reserve;
    let mut budget = window.saturating_sub(reserved);
    let char_cap = max_context_chars as usize;
    // 문자 한도와 토큰 한도 중 더 작은 쪽 (토큰≈문자/4)
    let char_as_tokens = char_cap / 4;
    if char_as_tokens < budget {
        budget = char_as_tokens;
    }

    let total = |m: &[Message]| m.iter().map(message_tokens).sum::<usize>();
    let mut omitted = false;
    while total(&msgs) > budget && msgs.len() > 1 {
        let min_keep = KEEP_TAIL.min(msgs.len());
        if msgs.len() > min_keep && drop_oldest_unit(&mut msgs) {
            omitted = true;
            continue;
        }
        if drop_oldest_unit(&mut msgs) {
            omitted = true;
        } else {
            break;
        }
    }
    if omitted {
        insert_notice(&mut msgs);
    }
    while total(&msgs) > budget {
        let before = total(&msgs);
        let shrunk = truncate_oldest_text(&mut msgs, budget)
            || truncate_newest_tool_result(&mut msgs, budget);
        agent::sanitize_tool_pairs(&mut msgs);
        if shrunk && total(&msgs) < before {
            continue;
        }
        if drop_tool_pair_for_budget(&mut msgs, budget) {
            insert_notice(&mut msgs);
            if total(&msgs) < before {
                continue;
            }
        }
        if msgs.len() > 1 {
            if drop_oldest_unit(&mut msgs) {
                insert_notice(&mut msgs);
            }
            agent::sanitize_tool_pairs(&mut msgs);
            if total(&msgs) < before {
                continue;
            }
        }
        break;
    }
    agent::sanitize_tool_pairs(&mut msgs);
    while total(&msgs) > budget && !msgs.is_empty() {
        if !drop_oldest_unit(&mut msgs) {
            msgs.clear();
        }
    }
    msgs
}

#[allow(dead_code)]
pub fn packed_over_budget(before: usize, after: &[Message]) -> bool {
    after.iter().any(|m| {
        m.content.iter().any(|b| match b {
            ContentBlock::Text { text } => text.contains("[이전 대화 일부 생략]"),
            _ => false,
        })
    }) || after.len() < before
}

/// 모델별 컨텍스트 창. config 값이 있으면 우선.
/// 알려진 모델 패밀리의 컨텍스트 창 — opencode/omp 의 모델 카탈로그 방식과 동일하게
/// 모델 id 로 메타데이터를 먼저 조회한다. (부분 일치, 앞에서부터 우선)
const MODEL_CONTEXTS: &[(&str, u32)] = &[
    // OpenAI
    ("gpt-5", 400_000),
    ("codex", 400_000),
    ("o4", 200_000),
    ("o3", 200_000),
    ("gpt-4.1", 1_000_000),
    ("gpt-4o", 128_000),
    // Anthropic
    ("claude-opus", 200_000),
    ("claude-sonnet", 200_000),
    ("claude-haiku", 200_000),
    ("claude-3", 200_000),
    // Google
    ("gemini-3", 1_000_000),
    ("gemini-2.5-pro", 1_000_000),
    ("gemini-2.5-flash", 1_000_000),
    ("gemini-2", 128_000),
    // xAI
    ("grok-4", 256_000),
    ("grok-code-fast", 256_000),
    ("grok-3", 131_072),
    // 기타 주요 모델
    ("deepseek-v3", 128_000),
    ("deepseek-r1", 128_000),
    ("glm-5", 200_000),
    ("glm-4", 128_000),
    ("kimi-k2", 256_000),
    ("minimax-m2", 204_800),
    ("minimax-m3", 1_000_000),
    ("qwen3-coder", 262_144),
    ("qwen3", 131_072),
    ("mistral-large", 128_000),
    ("llama-4", 1_000_000),
];

fn model_context_from_catalog(model: &str) -> Option<u32> {
    let n = crate::ranks::normalize_id(model);
    for (pat, ctx) in MODEL_CONTEXTS {
        if n.contains(pat) {
            return Some(*ctx);
        }
    }
    None
}

pub fn context_window_for(provider: &str, model: &str, p: Option<&ProviderConfig>) -> u32 {
    // 1) config 명시값 최우선
    if let Some(p) = p
        && let Some(n) = p.context_window
        && n > 0
    {
        return n;
    }
    // 2) 모델 카탈로그 (opencode/omp 방식)
    if let Some(ctx) = model_context_from_catalog(model) {
        return ctx;
    }
    let n = crate::ranks::normalize_id(model);
    if crate::ranks::is_cheap_id(&n) {
        return 128_000;
    }
    match provider {
        "anthropic" => 200_000,
        "openai" => {
            if n.contains("codex") || n.contains("gpt-5") {
                400_000
            } else {
                128_000
            }
        }
        "gemini" => {
            if n.contains("1.5") {
                128_000
            } else {
                400_000
            }
        }
        "grok" => 128_000,
        "local" => 32_000,
        _ => 128_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn catalog_context_wins_over_provider_heuristics() {
        // gemini 계열이지만 2.5 pro 는 1M — provider 휴리스틱(400k)보다 카탈로그 우선
        assert_eq!(
            context_window_for("gemini", "gemini-2.5-pro", None),
            1_000_000
        );
        assert_eq!(context_window_for("xai", "grok-4", None), 256_000);
        assert_eq!(
            context_window_for("opencode_zen", "minimax-m2.7", None),
            204_800
        );
        assert_eq!(
            context_window_for("anthropic", "claude-sonnet-4.6", None),
            200_000
        );
        // config 명시값이 카탈로그보다 우선
        let pc = ProviderConfig {
            models_url: None,
            kind: "openai_compat".into(),
            auth: "api_key".into(),
            api_key_env: "X".into(),
            model: "m".into(),
            small_model: None,
            base_url: None,
            supports_tools: true,
            model_auto: false,
            context_window: Some(55_000),
            enabled: true,
        };
        assert_eq!(context_window_for("x", "gpt-5", Some(&pc)), 55_000);
    }

    fn pair() -> Vec<Message> {
        vec![
            Message::user_text("첫번째 질문 ".repeat(80)),
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
            Message::user_text("이어서"),
        ]
    }

    #[test]
    fn packer_respects_window() {
        let mut msgs = Vec::new();
        for i in 0..20 {
            msgs.push(Message::user_text(format!(
                "메시지 {i} {}",
                "가나다라 ".repeat(40)
            )));
            msgs.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: format!("답 {i} {}", "마바사 ".repeat(40)),
                }],
            });
        }
        let packed = pack_messages(&msgs, "sys", &[], 800, 100, 200_000);
        let used = packed.iter().map(message_tokens).sum::<usize>();
        let reserved = estimate_tokens("sys") + 100;
        assert!(
            used + reserved <= 800 + 80,
            "used={used} reserved={reserved}"
        );
        assert!(packed.iter().any(|m| m.content.iter().any(|b| match b {
            ContentBlock::Text { text } => text.contains("[이전 대화 일부 생략]"),
            _ => false,
        })));
        assert!(packed_over_budget(msgs.len(), &packed));
    }

    #[test]
    fn packer_never_exceeds_a_256_token_window_after_reservations() {
        let messages = vec![Message::user_text("x".repeat(800))];
        let system = "system";
        let reserve_output = 128;

        let packed = pack_messages(&messages, system, &[], 256, reserve_output, 200_000);

        let request_tokens = estimate_tokens(system)
            + tools_tokens(&[])
            + effective_output_limit(256, reserve_output) as usize
            + packed.iter().map(message_tokens).sum::<usize>();
        assert!(
            request_tokens <= 256,
            "request_tokens={request_tokens} window=256"
        );
    }

    #[test]
    fn fixed_system_prompt_without_output_capacity_is_rejected() {
        let system = "s".repeat(1_024);
        assert_eq!(estimate_tokens(&system), 256);
        assert_eq!(request_output_limit(&system, &[], 256, 128), 0);

        let packed = pack_messages(
            &[Message::user_text("must not be sent")],
            &system,
            &[],
            256,
            128,
            200_000,
        );
        assert!(packed.is_empty());
    }

    #[test]
    fn output_limit_reserves_capacity_for_the_current_message() {
        let system = "s".repeat(1_020);
        assert_eq!(estimate_tokens(&system), 255);
        assert_eq!(request_output_limit(&system, &[], 256, 128), 0);
    }

    #[test]
    fn sub_token_character_cap_cannot_preserve_the_current_message() {
        let packed = pack_messages(
            &[Message::user_text("current task")],
            "",
            &[],
            256,
            128,
            3,
        );
        assert!(packed.is_empty());
    }

    #[test]
    fn packer_does_not_inflate_a_tiny_positive_window() {
        let messages = vec![Message::user_text("x".repeat(160))];
        let reserve_output = 64;

        let packed = pack_messages(&messages, "", &[], 64, reserve_output, 200_000);

        let output_tokens = effective_output_limit(64, reserve_output) as usize;
        let request_tokens = output_tokens + packed.iter().map(message_tokens).sum::<usize>();
        assert_eq!(output_tokens, 32);
        assert!(
            request_tokens <= 64,
            "request_tokens={request_tokens} window=64"
        );
    }

    #[test]
    fn packer_keeps_tool_pairs() {
        let msgs = pair();
        let packed = pack_messages(&msgs, "s", &[], 200, 32, 200_000);
        let mut check = packed.clone();
        agent::sanitize_tool_pairs(&mut check);
        // sanitize 후에도 tool_use 가 있으면 바로 다음에 result 가 있어야 한다.
        let mut i = 0;
        while i < check.len() {
            if is_tool_use(&check[i]) {
                assert!(
                    i + 1 < check.len() && is_tool_result(&check[i + 1]),
                    "tool pair split at {i}"
                );
                i += 2;
            } else {
                i += 1;
            }
        }
    }

    #[test]
    fn packer_bounds_oversized_latest_inspection_without_splitting_pair() {
        let msgs = vec![
            Message::user_text("현재 파일을 직접 검사하고 판정하라"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "inspect-1".into(),
                    name: "read_file".into(),
                    input: json!({"path": "game.js"}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "inspect-1".into(),
                    content: format!(
                        "HEAD-INSPECTION\n{}\nTAIL-INSPECTION",
                        "x".repeat(256 * 1024)
                    ),
                    is_error: false,
                }],
            },
        ];

        let packed = pack_messages(&msgs, "review", &[], 32_000, 32_768, 200_000);

        let pair_index = packed
            .iter()
            .position(is_tool_use)
            .expect("the verdict-producing request must retain its tool use");
        assert!(
            packed.get(pair_index + 1).is_some_and(is_tool_result),
            "the newest inspection pair must stay atomic"
        );
        let content = packed
            .iter()
            .flat_map(|message| message.content.iter())
            .find_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(content),
                _ => None,
            })
            .expect("the verdict-producing request must retain its tool result");
        assert!(content.contains("HEAD-INSPECTION"));
        assert!(content.contains(TOOL_RESULT_OMIT_NOTICE.trim()));
        assert!(content.contains("TAIL-INSPECTION"));

        let reserved = estimate_tokens("review") + effective_output_limit(32_000, 32_768) as usize;
        let used = packed.iter().map(message_tokens).sum::<usize>();
        assert!(used + reserved <= 32_000, "used={used} reserved={reserved}");
    }

    #[test]
    fn packer_allocates_one_budget_across_sibling_tool_results() {
        let small = "small sibling remains complete";
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "small".into(),
                        name: "read_file".into(),
                        input: json!({"path": "small.txt"}),
                    },
                    ContentBlock::ToolUse {
                        id: "first".into(),
                        name: "read_file".into(),
                        input: json!({"path": "first.txt"}),
                    },
                    ContentBlock::ToolUse {
                        id: "second".into(),
                        name: "read_file".into(),
                        input: json!({"path": "second.txt"}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "small".into(),
                        content: small.into(),
                        is_error: false,
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "first".into(),
                        content: format!("FIRST-HEAD\n{}\nFIRST-TAIL", "a".repeat(256 * 1024)),
                        is_error: false,
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "second".into(),
                        content: format!("SECOND-HEAD\n{}\nSECOND-TAIL", "b".repeat(256 * 1024)),
                        is_error: true,
                    },
                ],
            },
        ];

        let packed = pack_messages(&msgs, "review", &[], 32_000, 32_768, 200_000);
        let pair_index = packed
            .iter()
            .position(is_tool_use)
            .expect("the latest tool use must be retained");
        assert!(
            packed.get(pair_index + 1).is_some_and(is_tool_result),
            "the latest tool use and all sibling results must remain adjacent"
        );

        let results: Vec<(&str, &str, bool)> = packed[pair_index + 1]
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => Some((tool_use_id.as_str(), content.as_str(), *is_error)),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0], ("small", small, false));
        assert_eq!(results[1].0, "first");
        assert!(!results[1].2);
        assert!(results[1].1.contains("FIRST-HEAD"));
        assert!(results[1].1.contains(TOOL_RESULT_OMIT_NOTICE.trim()));
        assert!(results[1].1.contains("FIRST-TAIL"));
        assert_eq!(results[2].0, "second");
        assert!(results[2].2);
        assert!(results[2].1.contains("SECOND-HEAD"));
        assert!(results[2].1.contains(TOOL_RESULT_OMIT_NOTICE.trim()));
        assert!(results[2].1.contains("SECOND-TAIL"));

        let reserved = estimate_tokens("review") + effective_output_limit(32_000, 32_768) as usize;
        let used = packed.iter().map(message_tokens).sum::<usize>();
        assert!(used + reserved <= 32_000, "used={used} reserved={reserved}");
    }

    #[test]
    fn packer_drops_a_tool_pair_when_its_fixed_structure_exceeds_budget() {
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "oversized-input".into(),
                        name: "read_file".into(),
                        input: json!({"payload": "x".repeat(4_096)}),
                    },
                    ContentBlock::ToolUse {
                        id: "oversized-input-2".into(),
                        name: "read_file".into(),
                        input: json!({"path": "second.rs"}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "oversized-input".into(),
                        content: "first result".repeat(128),
                        is_error: false,
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "oversized-input-2".into(),
                        content: "second result".repeat(128),
                        is_error: true,
                    },
                ],
            },
        ];

        let packed = pack_messages(&msgs, "", &[], 256, 0, 4_096);
        assert!(packed.iter().all(|message| !is_tool_use(message)));
        assert!(packed.iter().all(|message| !is_tool_result(message)));
        assert!(has_omit_notice(&packed));

        let window = 256usize;
        let reserved =
            estimate_tokens("") + tools_tokens(&[]) + effective_output_limit(256, 0) as usize;
        let mut budget = window.saturating_sub(reserved).max(256);
        let char_as_tokens = 4_096usize / 4;
        if char_as_tokens < budget {
            budget = char_as_tokens.max(256);
        }
        let used = packed.iter().map(message_tokens).sum::<usize>();
        assert!(used <= budget, "used={used} budget={budget}");

        let repacked = pack_messages(&packed, "", &[], 256, 0, 4_096);
        assert_eq!(
            serde_json::to_value(&repacked).unwrap(),
            serde_json::to_value(&packed).unwrap()
        );
    }

    #[test]
    fn dropping_an_oversized_pair_preserves_remaining_pair_metadata() {
        let mut msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "first".into(),
                    name: "read_file".into(),
                    input: json!({"path": "first.rs"}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "first".into(),
                    content: "first result".into(),
                    is_error: false,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "dropped".into(),
                    name: "read_file".into(),
                    input: json!({"payload": "x".repeat(4_096)}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "dropped".into(),
                    content: "dropped result".repeat(128),
                    is_error: false,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "last".into(),
                    name: "read_file".into(),
                    input: json!({"path": "last.rs"}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "last".into(),
                    content: "last result".into(),
                    is_error: true,
                }],
            },
        ];

        assert!(drop_tool_pair_for_budget(&mut msgs, 256));
        let uses: Vec<&str> = msgs
            .iter()
            .filter(|message| is_tool_use(message))
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                ContentBlock::Text { .. } | ContentBlock::ToolResult { .. } => None,
            })
            .collect();
        assert_eq!(uses, vec!["first", "last"]);
        let results: Vec<(&str, bool)> = msgs
            .iter()
            .filter(|message| is_tool_result(message))
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } => Some((tool_use_id.as_str(), *is_error)),
                ContentBlock::Text { .. } | ContentBlock::ToolUse { .. } => None,
            })
            .collect();
        assert_eq!(results, vec![("first", false), ("last", true)]);
    }

    #[test]
    fn packer_drops_a_fixed_pair_when_the_omission_notice_exceeds_the_budget() {
        let tool_use = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "edge".into(),
                name: "read_file".into(),
                input: json!({"payload": "x".repeat(984)}),
            }],
        };
        let tool_result = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "edge".into(),
                content: String::new(),
                is_error: false,
            }],
        };
        let pair_tokens = message_tokens(&tool_use) + message_tokens(&tool_result);
        let notice_tokens = message_tokens(&Message::user_text("[이전 대화 일부 생략]"));
        assert!(pair_tokens <= 256, "pair_tokens={pair_tokens}");
        assert!(
            pair_tokens + notice_tokens > 256,
            "pair_tokens={pair_tokens} notice_tokens={notice_tokens}"
        );
        let messages = vec![
            Message::user_text("old context ".repeat(256)),
            tool_use,
            tool_result,
        ];

        let packed = pack_messages(&messages, "", &[], 256, 0, 4_096);

        assert_eq!(packed.len(), 1);
        assert!(has_omit_notice(&packed));
        assert!(packed.iter().map(message_tokens).sum::<usize>() <= 256);
    }

    #[test]
    fn truncate_oldest_text_skips_a_candidate_that_would_not_shrink() {
        let original = "x".repeat(132);
        let mut msgs = vec![Message::user_text(original.clone())];
        assert!(!truncate_oldest_text(&mut msgs, 10_000));
        let ContentBlock::Text { text } = &msgs[0].content[0] else {
            panic!("test setup must contain text");
        };
        assert_eq!(text, &original);
    }

    #[test]
    fn auto_compaction_starts_at_eighty_percent() {
        assert!(!needs_auto_compaction(799, 1_000));
        assert!(needs_auto_compaction(800, 1_000));
        assert!(needs_auto_compaction(1_000, 1_000));
        assert!(!needs_auto_compaction(800, 0));
    }
}
