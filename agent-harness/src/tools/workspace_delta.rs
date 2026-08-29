use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

pub(super) struct WorkspaceSnapshot {
    root: PathBuf,
    files: BTreeMap<PathBuf, FileStamp>,
}

impl WorkspaceSnapshot {
    pub(super) fn capture(root: &Path) -> Self {
        let mut files = BTreeMap::new();
        let walker = ignore::WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .filter_entry(|entry| {
                if entry.depth() == 0 || !entry.file_type().is_some_and(|kind| kind.is_dir()) {
                    return true;
                }
                !matches!(
                    entry.file_name().to_string_lossy().as_ref(),
                    ".git" | ".venv" | "__pycache__" | "node_modules" | "target" | "venv"
                )
            })
            .build();
        for entry in walker.flatten() {
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let path = entry.into_path();
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            files.insert(
                path,
                FileStamp {
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                },
            );
        }
        Self {
            root: root.to_path_buf(),
            files,
        }
    }

    pub(super) fn changed_paths(&self) -> Vec<PathBuf> {
        let after = Self::capture(&self.root);
        let paths: BTreeSet<&PathBuf> = self.files.keys().chain(after.files.keys()).collect();
        paths
            .into_iter()
            .filter(|path| self.files.get(*path) != after.files.get(*path))
            .cloned()
            .collect()
    }
}
