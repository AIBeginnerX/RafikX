use std::sync::{Arc, Mutex, Weak};

use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalState {
    Succeeded,
    Limited,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishResult {
    Finished(TerminalState),
    AlreadyFinished(TerminalState),
}

#[derive(Clone)]
pub struct RunControl {
    node: Arc<CancelNode>,
    terminal: Arc<Mutex<Option<TerminalState>>>,
}

struct CancelNode {
    reason: Mutex<Option<String>>,
    notify: Notify,
    children: Mutex<Vec<Weak<CancelNode>>>,
}

impl RunControl {
    pub fn new() -> Self {
        Self {
            node: Arc::new(CancelNode::new()),
            terminal: Arc::new(Mutex::new(None)),
        }
    }

    pub fn child(&self) -> Self {
        let node = Arc::new(CancelNode::new());
        if let Ok(mut children) = self.node.children.lock() {
            children.push(Arc::downgrade(&node));
        }
        if let Some(reason) = self.cancel_reason() {
            node.cancel(reason);
        }
        Self {
            node,
            terminal: Arc::new(Mutex::new(None)),
        }
    }

    pub fn cancel(&self, reason: impl Into<String>) -> bool {
        self.node.cancel(reason.into())
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_reason().is_some()
    }

    pub fn cancel_reason(&self) -> Option<String> {
        self.node
            .reason
            .lock()
            .ok()
            .and_then(|reason| reason.clone())
    }

    pub async fn cancelled_reason(&self) -> String {
        loop {
            let notified = self.node.notify.notified();
            if let Some(reason) = self.cancel_reason() {
                return reason;
            }
            notified.await;
        }
    }

    pub fn finish(&self, state: TerminalState) -> FinishResult {
        let Ok(mut terminal) = self.terminal.lock() else {
            return FinishResult::AlreadyFinished(state);
        };
        if let Some(existing) = *terminal {
            return FinishResult::AlreadyFinished(existing);
        }
        *terminal = Some(state);
        FinishResult::Finished(state)
    }

    pub fn terminal_state(&self) -> Option<TerminalState> {
        self.terminal.lock().ok().and_then(|terminal| *terminal)
    }
}

impl Default for RunControl {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelNode {
    fn new() -> Self {
        Self {
            reason: Mutex::new(None),
            notify: Notify::new(),
            children: Mutex::new(Vec::new()),
        }
    }

    fn cancel(&self, reason: String) -> bool {
        let first = if let Ok(mut stored) = self.reason.lock() {
            if stored.is_some() {
                false
            } else {
                *stored = Some(reason.clone());
                true
            }
        } else {
            false
        };
        if !first {
            return false;
        }
        self.notify.notify_waiters();
        let children = self
            .children
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default();
        for child in children.into_iter().filter_map(|child| child.upgrade()) {
            child.cancel(reason.clone());
        }
        true
    }
}
