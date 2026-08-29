use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    content_hash: u64,
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut content_hash = 0xcbf29ce484222325u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            content_hash ^= u64::from(*byte);
            content_hash = content_hash.wrapping_mul(0x100000001b3);
        }
    }
    Some(FileStamp { len, content_hash })
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
            .ignore(false)
            .git_global(false)
            .git_ignore(false)
            .git_exclude(false)
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
            let Some(stamp) = file_stamp(&path) else {
                continue;
            };
            files.insert(path, stamp);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_length_rewrite_and_ignored_file_are_detected() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-workspace-delta-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("temp workspace");
        std::fs::write(root.join(".gitignore"), ".env\n").expect("gitignore");
        std::fs::write(root.join("game.js"), "alpha").expect("source");
        std::fs::write(root.join(".env"), "TOKEN=old").expect("ignored source");

        let snapshot = WorkspaceSnapshot::capture(&root);
        std::fs::write(root.join("game.js"), "bravo").expect("same length rewrite");
        std::fs::write(root.join(".env"), "TOKEN=new").expect("ignored rewrite");

        let changed = snapshot.changed_paths();
        assert!(changed.contains(&root.join("game.js")));
        assert!(changed.contains(&root.join(".env")));
        let _ = std::fs::remove_dir_all(root);
    }
}
