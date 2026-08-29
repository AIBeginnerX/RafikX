use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

mod data;
mod lifecycle_api;

use super::events::{EventBus, EventTap};
use super::{
    AgentId, ContextSourceRecord, EventReceiver, ProgressState, RunControl, RunEventKind, RunId,
    TodoStore,
};
use crate::lifecycle::{LifecycleRuntime, LifecycleStore};
use crate::tools::workspace_delta::FileFingerprint;

pub type RunLiveSink = Arc<dyn Fn(crate::ui::Live) + Send + Sync>;

struct ModelIterationBudget {
    limit: AtomicU32,
    used: AtomicU32,
}

#[derive(Default)]
pub struct RunMetrics {
    context_window: AtomicU32,
    fallback_quiet: AtomicBool,
}

impl RunMetrics {
    pub fn context_window(&self) -> u32 {
        self.context_window.load(Ordering::Relaxed)
    }

    pub fn set_context_window(&self, value: u32) {
        self.context_window.store(value, Ordering::Relaxed);
    }

    pub fn fallback_quiet(&self) -> bool {
        self.fallback_quiet.load(Ordering::Relaxed)
    }

    pub fn set_fallback_quiet(&self, value: bool) {
        self.fallback_quiet.store(value, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct RunContext {
    run_id: RunId,
    parent_run_id: Option<RunId>,
    agent_id: Option<AgentId>,
    workspace: Arc<PathBuf>,
    config: Option<Arc<crate::config::Config>>,
    events: EventBus,
    control: RunControl,
    todos: TodoStore,
    progress: Arc<ProgressState>,
    metrics: Arc<RunMetrics>,
    live_sink: Option<RunLiveSink>,
    committed_baselines: Arc<Mutex<BTreeMap<PathBuf, Option<FileFingerprint>>>>,
    legacy_committed_paths: Arc<Mutex<Vec<PathBuf>>>,
    change_tracking_complete: Arc<AtomicBool>,
    context_sources: Arc<Mutex<Vec<ContextSourceRecord>>>,
    lifecycle: Arc<LifecycleRuntime>,
    approval_all: Arc<AtomicBool>,
    model_iterations: Arc<ModelIterationBudget>,
    unresolved_child_tasks: Arc<Mutex<BTreeMap<String, String>>>,
}

impl RunContext {
    pub fn isolated(run_id: RunId, workspace: PathBuf) -> Self {
        Self::new(run_id, workspace, None, None, EventBus::new(), None)
    }

    pub fn for_config(run_id: RunId, config: Arc<crate::config::Config>) -> Self {
        let store = LifecycleStore::open(&config.data_dir.join("data.db")).ok();
        Self::new(
            run_id,
            config.workspace.clone(),
            Some(config),
            None,
            EventBus::new(),
            store,
        )
    }

    pub fn with_live_sink(mut self, sink: Option<RunLiveSink>) -> Self {
        self.live_sink = sink;
        self
    }

    pub fn with_event_tap(mut self, tap: EventTap) -> Self {
        self.events = self.events.with_tap(tap);
        self
    }

    pub fn child(&self, run_id: RunId, agent_id: AgentId) -> Self {
        let lifecycle = self.lifecycle.child(run_id.clone(), agent_id.clone());
        Self {
            run_id,
            parent_run_id: Some(self.run_id.clone()),
            agent_id: Some(agent_id),
            workspace: Arc::clone(&self.workspace),
            config: self.config.clone(),
            events: self.events.clone(),
            control: self.control.child(),
            todos: TodoStore::new(),
            progress: Arc::new(ProgressState::new()),
            metrics: Arc::new(RunMetrics::default()),
            live_sink: self.live_sink.clone(),
            committed_baselines: Arc::new(Mutex::new(BTreeMap::new())),
            legacy_committed_paths: Arc::new(Mutex::new(Vec::new())),
            change_tracking_complete: Arc::new(AtomicBool::new(true)),
            context_sources: Arc::new(Mutex::new(Vec::new())),
            lifecycle,
            approval_all: Arc::clone(&self.approval_all),
            model_iterations: Arc::clone(&self.model_iterations),
            unresolved_child_tasks: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn parent_run_id(&self) -> Option<&RunId> {
        self.parent_run_id.as_ref()
    }

    pub fn agent_id(&self) -> Option<&AgentId> {
        self.agent_id.as_ref()
    }

    pub fn workspace(&self) -> &Path {
        self.workspace.as_path()
    }

    pub(crate) fn workspace_relative(&self, path: &Path) -> PathBuf {
        let canonical = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.as_ref().clone());
        let normalized = self
            .normalize_workspace_path(path)
            .unwrap_or_else(|| path.to_path_buf());
        normalized
            .strip_prefix(&canonical)
            .or_else(|_| path.strip_prefix(self.workspace.as_path()))
            .unwrap_or(&normalized)
            .to_path_buf()
    }

    pub(crate) fn normalize_workspace_path(&self, path: &Path) -> Option<PathBuf> {
        let workspace = self.workspace.canonicalize().ok()?;
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace.join(path)
        };
        if joined
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return None;
        }
        let mut ancestor = joined.parent();
        let mut resolved = None;
        while let Some(path) = ancestor {
            if path.exists() {
                let canonical = path.canonicalize().ok()?;
                let suffix = joined.strip_prefix(path).ok()?;
                resolved = Some(canonical.join(suffix));
                break;
            }
            ancestor = path.parent();
        }
        let normalized = resolved?;
        normalized.starts_with(&workspace).then_some(normalized)
    }

    pub fn config(&self) -> Option<Arc<crate::config::Config>> {
        self.config.clone()
    }

    pub fn metrics(&self) -> &RunMetrics {
        &self.metrics
    }

    pub fn subscribe(&self) -> EventReceiver {
        self.events.subscribe()
    }

    pub fn emit(&self, kind: RunEventKind, payload: Value) {
        self.events.emit(
            self.run_id.clone(),
            self.parent_run_id.clone(),
            self.agent_id.clone(),
            kind,
            payload,
        );
    }

    pub fn emit_live(&self, event: crate::ui::Live) -> bool {
        let Some(sink) = &self.live_sink else {
            return false;
        };
        sink(event);
        true
    }

    pub fn has_live_sink(&self) -> bool {
        self.live_sink.is_some()
    }

    pub(crate) fn progress(&self) -> &ProgressState {
        &self.progress
    }

    pub(crate) fn ensure_model_iteration_limit(&self, limit: u32) -> u32 {
        let limit = limit.max(1);
        let _ = self.model_iterations.limit.compare_exchange(
            0,
            limit,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.model_iterations.limit.load(Ordering::Acquire)
    }

    pub(crate) fn claim_model_iteration(&self) -> bool {
        let limit = self.model_iterations.limit.load(Ordering::Acquire);
        if limit == 0 {
            return false;
        }
        let mut used = self.model_iterations.used.load(Ordering::Acquire);
        loop {
            if used >= limit {
                return false;
            }
            match self.model_iterations.used.compare_exchange_weak(
                used,
                used + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => used = actual,
            }
        }
    }

    pub(crate) fn model_iterations_used(&self) -> u32 {
        self.model_iterations.used.load(Ordering::Acquire)
    }

    pub(crate) fn model_iteration_limit(&self) -> u32 {
        self.model_iterations.limit.load(Ordering::Acquire)
    }

    pub(crate) fn mark_unresolved_child_task(&self, key: String, failure: String) {
        self.unresolved_child_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, failure);
    }

    pub(crate) fn clear_unresolved_child_task(&self, key: &str) {
        self.unresolved_child_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(key);
    }

    pub(crate) fn unresolved_child_task_summary(&self) -> Option<String> {
        let unresolved = self
            .unresolved_child_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let first = unresolved
            .values()
            .next()?
            .chars()
            .take(300)
            .collect::<String>();
        Some(format!(
            "미해결 위임 작업 {}건이 남아 있습니다: {first}",
            unresolved.len()
        ))
    }

    fn new(
        run_id: RunId,
        workspace: PathBuf,
        config: Option<Arc<crate::config::Config>>,
        live_sink: Option<RunLiveSink>,
        events: EventBus,
        store: Option<LifecycleStore>,
    ) -> Self {
        let lifecycle = LifecycleRuntime::root(run_id.clone(), store);
        Self {
            run_id,
            parent_run_id: None,
            agent_id: None,
            workspace: Arc::new(workspace),
            config,
            events,
            control: RunControl::new(),
            todos: TodoStore::new(),
            progress: Arc::new(ProgressState::new()),
            metrics: Arc::new(RunMetrics::default()),
            live_sink,
            committed_baselines: Arc::new(Mutex::new(BTreeMap::new())),
            legacy_committed_paths: Arc::new(Mutex::new(Vec::new())),
            change_tracking_complete: Arc::new(AtomicBool::new(true)),
            context_sources: Arc::new(Mutex::new(Vec::new())),
            lifecycle,
            approval_all: Arc::new(AtomicBool::new(false)),
            model_iterations: Arc::new(ModelIterationBudget {
                limit: AtomicU32::new(0),
                used: AtomicU32::new(0),
            }),
            unresolved_child_tasks: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn workspace_relative_resolves_a_symlinked_workspace_root() {
        let parent = std::env::temp_dir().join(format!(
            "rafikx-context-relative-{}",
            crate::db::Db::new_id()
        ));
        let real = parent.join("real");
        let alias = parent.join("alias");
        std::fs::create_dir_all(&real).expect("real workspace");
        std::os::unix::fs::symlink(&real, &alias).expect("workspace alias");
        let source = real.join("game.js");
        std::fs::write(&source, "game").expect("source");
        let run = RunContext::isolated(crate::run::RunId::new("relative-test"), alias);

        assert_eq!(run.workspace_relative(&source), PathBuf::from("game.js"));
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn model_iteration_budget_is_shared_by_the_run_tree() {
        let root = RunContext::isolated(
            crate::run::RunId::new("iteration-root"),
            std::env::temp_dir(),
        );
        assert_eq!(root.ensure_model_iteration_limit(50), 50);
        let child = root.child(
            crate::run::RunId::new("iteration-child"),
            crate::run::AgentId::new("child"),
        );
        let sibling = root.child(
            crate::run::RunId::new("iteration-sibling"),
            crate::run::AgentId::new("sibling"),
        );
        let mut workers = Vec::new();
        for index in 0..80 {
            let run = if index % 2 == 0 {
                child.clone()
            } else {
                sibling.clone()
            };
            workers.push(std::thread::spawn(move || run.claim_model_iteration()));
        }
        let claimed = workers
            .into_iter()
            .map(|worker| usize::from(worker.join().expect("budget worker")))
            .sum::<usize>();
        assert_eq!(claimed, 50);
        assert_eq!(root.model_iterations_used(), 50);
        assert!(!root.claim_model_iteration());

        let independent = RunContext::isolated(
            crate::run::RunId::new("iteration-independent"),
            std::env::temp_dir(),
        );
        independent.ensure_model_iteration_limit(2);
        assert!(independent.claim_model_iteration());
        assert_eq!(independent.model_iterations_used(), 1);
    }
}
