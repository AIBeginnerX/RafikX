use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct ProgressState {
    answer_started: AtomicBool,
    running: AtomicBool,
    label: Mutex<Option<String>>,
    notes: Mutex<Vec<String>>,
}

impl ProgressState {
    pub(crate) fn new() -> Self {
        Self {
            answer_started: AtomicBool::new(false),
            running: AtomicBool::new(false),
            label: Mutex::new(None),
            notes: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn mark_answer_started(&self) {
        self.answer_started.store(true, Ordering::Relaxed);
    }

    pub(crate) fn answer_started(&self) -> bool {
        self.answer_started.load(Ordering::Relaxed)
    }

    pub(crate) fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::Relaxed);
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub(crate) fn set_label(&self, label: &str) {
        if let Ok(mut current) = self.label.lock() {
            *current = Some(label.chars().take(56).collect());
        }
    }

    pub(crate) fn label(&self) -> Option<String> {
        self.label.lock().ok().and_then(|label| label.clone())
    }

    pub(crate) fn push_note(&self, note: &str) {
        if let Ok(mut notes) = self.notes.lock() {
            notes.push(note.to_string());
        }
    }

    pub(crate) fn drain_notes(&self) -> Vec<String> {
        self.notes
            .lock()
            .map(|mut notes| std::mem::take(&mut *notes))
            .unwrap_or_default()
    }

    pub(crate) fn reset(&self) {
        self.answer_started.store(false, Ordering::Relaxed);
        self.set_running(false);
        if let Ok(mut notes) = self.notes.lock() {
            notes.clear();
        }
    }
}
