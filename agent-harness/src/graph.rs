//! Append-only run graph (DeepSeek Harness: every run is traceable).
//!
//! Applied from public DeepSeek Harness docs (2026):
//! - Thin loop stays “call model → run tools → loop”; extra behavior is recorded
//!   as named checkpoints, not a second agent
//!   ([agent-loop](https://deepseekdocs.com/en/docs/learn/core/agent-loop),
//!   [deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness)).
//! - Session/turn/step as durable events: classify → bind → context → pre_step
//!   → request → tool_pre/tool_post → verify → persist.
//! - `agent/pre-step` analogue: lessons inject + pack before the request.
//! - `tools/pre-execute` / `tools/post-execute` analogue: tool_pre / tool_post nodes
//!   (approval remains the existing LocalAsk/RemoteApproval gate).
//!
//! Deferred (not a Cordis/plugin rewrite): plugin runtime, Code mode, HMR patches,
//! `maxParallelToolCalls`, Claude Code/Codex shell-hook bridges.
//! Nodes are classify → bind → context → pre_step → request → tool_* → verify → persist.

use std::sync::Mutex;

use anyhow::Result;
use serde::Serialize;

use crate::db::Db;

static CURRENT: Mutex<Option<String>> = Mutex::new(None);

pub struct Scope {
    prev: Option<String>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        if let Ok(mut g) = CURRENT.lock() {
            *g = self.prev.take();
        }
    }
}

pub fn scope(run_id: impl Into<String>) -> Scope {
    let mut g = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    let prev = g.clone();
    *g = Some(run_id.into());
    Scope { prev }
}

pub fn current_run() -> Option<String> {
    CURRENT.lock().ok().and_then(|g| g.clone())
}

/// Classify → bind → optional Obsidian context. Call while a [`scope`] is active.
pub fn trace_start(class: &str, profile: &str, provider: &str, model: &str, obsidian: bool) {
    node("classify", class, "", None);
    node(
        "bind",
        profile,
        &format!("{provider} · {model}"),
        Some("classify"),
    );
    if obsidian {
        node("context", "obsidian", "vault search", Some("bind"));
    } else {
        node("context", "none", "", Some("bind"));
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub seq: i64,
    pub kind: String,
    pub label: String,
    pub detail: String,
    pub parent: Option<String>,
}

/// Record a node. Never logs secrets. Failures are silent so the loop never depends on the graph.
pub fn node(kind: &str, label: &str, detail: &str, parent: Option<&str>) {
    let Some(run_id) = current_run() else {
        return;
    };
    let detail: String = detail.chars().take(400).collect();
    let _ = persist(&run_id, kind, label, &detail, parent);
}

fn persist(run_id: &str, kind: &str, label: &str, detail: &str, parent: Option<&str>) -> Result<()> {
    let db = Db::open(&Db::db_path()?)?;
    db.push_graph_event(run_id, kind, label, detail, parent)
}

pub fn for_run(run_id: &str) -> Result<Vec<GraphNode>> {
    let db = Db::open(&Db::db_path()?)?;
    Ok(db
        .graph_events(run_id)?
        .into_iter()
        .map(|r| GraphNode {
            seq: r.seq,
            kind: r.kind,
            label: r.label,
            detail: r.detail,
            parent: r.parent,
        })
        .collect())
}

pub fn latest() -> Result<Option<(String, Vec<GraphNode>)>> {
    let db = Db::open(&Db::db_path()?)?;
    let Some(id) = db.latest_run_id()? else {
        return Ok(None);
    };
    Ok(Some((id.clone(), for_run(&id)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_restores_previous() {
        let a = scope("run-a");
        assert_eq!(current_run().as_deref(), Some("run-a"));
        {
            let _b = scope("run-b");
            assert_eq!(current_run().as_deref(), Some("run-b"));
        }
        assert_eq!(current_run().as_deref(), Some("run-a"));
        drop(a);
        assert!(current_run().is_none());
    }
}
