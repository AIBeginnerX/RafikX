use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde_json::json;

use super::RunContext;
use crate::run::{ContextSourceKind, ContextSourceRecord, RunEventKind};
use crate::tools::workspace_delta::{FileBaseline, fingerprint_paths};

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

    pub(crate) fn record_committed_changes(
        &self,
        changes: impl IntoIterator<Item = FileBaseline>,
    ) {
        match self.committed_baselines.lock() {
            Ok(mut committed) => {
                for change in changes {
                    committed
                        .entry(change.path)
                        .or_insert(change.fingerprint);
                }
            }
            Err(_) => self.mark_change_tracking_incomplete(),
        }
    }

    pub(crate) fn committed_changes(&self) -> Vec<FileBaseline> {
        match self.committed_baselines.lock() {
            Ok(changes) => changes
                .iter()
                .map(|(path, fingerprint)| FileBaseline {
                    path: path.clone(),
                    fingerprint: fingerprint.clone(),
                })
                .collect(),
            Err(_) => {
                self.mark_change_tracking_incomplete();
                Vec::new()
            }
        }
    }

    pub(crate) fn mark_change_tracking_incomplete(&self) {
        self.change_tracking_complete
            .store(false, Ordering::Relaxed);
    }

    pub(crate) fn change_tracking_complete(&self) -> bool {
        self.change_tracking_complete.load(Ordering::Relaxed)
    }

    pub fn committed_paths(&self) -> Vec<PathBuf> {
        if !self.change_tracking_complete() {
            return Vec::new();
        }
        let baselines = match self.committed_baselines.lock() {
            Ok(baselines) => baselines.clone(),
            Err(_) => {
                self.mark_change_tracking_incomplete();
                return Vec::new();
            }
        };
        let current = match fingerprint_paths(baselines.keys().cloned()) {
            Ok(current) => current,
            Err(_) => {
                self.mark_change_tracking_incomplete();
                return Vec::new();
            }
        };
        baselines
            .into_iter()
            .filter_map(|(path, original)| {
                (current.get(&path) != Some(&original)).then_some(path)
            })
            .collect()
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
