use crate::config::Config;
use crate::run::{ContextSourceKind, RunContext};

const SYSTEM_BUDGET_TOKENS: u32 = 8_192;
const PROJECT_RULES_CHARS: usize = 8_000;

pub fn record_system_sources(run: &RunContext, cfg: &Config, system: &str, lessons: &str) {
    let memory_tokens = tokens(lessons);
    let rule = ["AGENTS.md", "RAFIKX.md"].into_iter().find_map(|name| {
        let path = cfg.workspace.join(name);
        std::fs::read_to_string(&path).ok().map(|body| {
            let body = body
                .trim()
                .chars()
                .take(PROJECT_RULES_CHARS)
                .collect::<String>();
            (path, tokens(&body))
        })
    });
    let rule_tokens = rule.as_ref().map(|(_, used)| *used).unwrap_or(0);
    let total = tokens(system);
    run.record_context_source(
        ContextSourceKind::System,
        "rafikx.system.v1",
        SYSTEM_BUDGET_TOKENS,
        total
            .saturating_sub(rule_tokens)
            .saturating_sub(memory_tokens),
    );
    if let Some((path, used)) = rule {
        run.record_context_source(
            ContextSourceKind::ProjectRules,
            path.display().to_string(),
            (PROJECT_RULES_CHARS / 4) as u32,
            used,
        );
    }
    if memory_tokens > 0 {
        run.record_context_source(
            ContextSourceKind::ProjectMemory,
            cfg.workspace.display().to_string(),
            cfg.file.memory.inject_limit_chars.saturating_add(3) / 4,
            memory_tokens,
        );
    }
}

pub fn record_plan(run: &RunContext, plan: &str, budget_tokens: u32) {
    run.record_context_source(
        ContextSourceKind::Plan,
        format!("{}:plan", run.run_id()),
        budget_tokens,
        tokens(plan),
    );
}

pub fn tokens(text: &str) -> u32 {
    crate::packer::estimate_tokens(text).min(u32::MAX as usize) as u32
}
