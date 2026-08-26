use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde_json::json;

use super::RunContext;
use crate::run::{ContextSourceKind, ContextSourceRecord, RunEventKind};

impl RunContext {
    pub fn approve_run_tree(&self) {
        self.approval_all.store(true, Ordering::Relaxed);
    }

    pub fn run_tree_approved(&self) -> bool {
        self.approval_all.load(Ordering::Relaxed)
    }

    pub fn todos(&self) -> Vec<crate::tools_more::TodoItem> {
        self.todos.get()
    }

    pub fn replace_todos(&self, items: &[crate::tools_more::TodoItem]) {
        self.todos.replace(items);
        self.emit(RunEventKind::Todo, json!({"count": items.len()}));
    }

    pub fn record_committed_paths(&self, paths: impl IntoIterator<Item = PathBuf>) {
        if let Ok(mut committed) = self.committed_paths.lock() {
            for path in paths {
                if !committed.contains(&path) {
                    committed.push(path);
                }
            }
        }
    }

    pub fn committed_paths(&self) -> Vec<PathBuf> {
        self.committed_paths
            .lock()
            .map(|paths| paths.clone())
            .unwrap_or_default()
    }

    pub fn record_context_source(
        &self,
        kind: ContextSourceKind,
        source_id: impl Into<String>,
        budget_tokens: u32,
        used_tokens: u32,
    ) {
        let record = ContextSourceRecord {
            kind,
            source_id: source_id.into(),
            budget_tokens,
            used_tokens,
        };
        if let Ok(mut sources) = self.context_sources.lock() {
            sources.push(record.clone());
        }
        self.emit(RunEventKind::Context, json!(record));
    }

    pub fn context_sources(&self) -> Vec<ContextSourceRecord> {
        self.context_sources
            .lock()
            .map(|sources| sources.clone())
            .unwrap_or_default()
    }
}
