use crate::agent;
use crate::config::ProviderConfig;
use crate::provider::{ContentBlock, Message, Role, ToolSpec};

const KEEP_TAIL: usize = 8;
pub const AUTO_COMPACT_PERCENT: u32 = 80;

pub const fn needs_auto_compaction(used: u32, window: u32) -> bool {
    window > 0 && used.saturating_mul(100) >= window.saturating_mul(AUTO_COMPACT_PERCENT)
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
    if is_tool_use(&msgs[0]) && msgs.len() > 2 && is_tool_result(&msgs[1]) {
        msgs.remove(0);
        msgs.remove(0);
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

fn truncate_oldest_text(msgs: &mut [Message], budget: usize) {
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
                *text = format!("{cut}\n[이전 대화 일부 생략]");
                return;
            }
        }
    }
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

    let reserved = estimate_tokens(system) + tools_tokens(tools) + reserve_output as usize;
    let window = (context_window as usize).max(256);
    let mut budget = window.saturating_sub(reserved).max(256);
    let char_cap = (max_context_chars as usize).max(4096);
    // 문자 한도와 토큰 한도 중 더 작은 쪽 (토큰≈문자/4)
    let char_as_tokens = char_cap / 4;
    if char_as_tokens < budget {
        budget = char_as_tokens.max(256);
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
    let mut guard = 0;
    while total(&msgs) > budget && guard < 8 {
        truncate_oldest_text(&mut msgs, budget);
        agent::sanitize_tool_pairs(&mut msgs);
        if total(&msgs) > budget && msgs.len() > 1 {
            drop_oldest_unit(&mut msgs);
            insert_notice(&mut msgs);
        } else {
            break;
        }
        guard += 1;
    }
    agent::sanitize_tool_pairs(&mut msgs);
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
    fn auto_compaction_starts_at_eighty_percent() {
        assert!(!needs_auto_compaction(799, 1_000));
        assert!(needs_auto_compaction(800, 1_000));
        assert!(needs_auto_compaction(1_000, 1_000));
        assert!(!needs_auto_compaction(800, 0));
    }
}
