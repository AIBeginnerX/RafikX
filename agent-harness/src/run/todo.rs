use std::sync::{Arc, Mutex};

use crate::tools_more::TodoItem;

#[derive(Clone)]
pub(crate) struct TodoStore {
    items: Arc<Mutex<Vec<TodoItem>>>,
}

impl TodoStore {
    pub(crate) fn new() -> Self {
        Self {
            items: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn get(&self) -> Vec<TodoItem> {
        self.items
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
    }

    pub(crate) fn replace(&self, items: &[TodoItem]) {
        if let Ok(mut current) = self.items.lock() {
            *current = items.to_vec();
        }
    }
}
