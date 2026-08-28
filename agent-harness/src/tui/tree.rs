//! 파일 탐색기 패널 — 워크스페이스 트리를 모달로 보여준다.
//! 편집 입력과 키를 공유하지 않는다: 패널이 열려 있는 동안에는
//! 이동·펼치기·미리보기 키만 먹고, Esc 로 닫으면 원래 입력으로 돌아간다.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 탐색기에 보이지 않을 디렉터리 — 빌드 산출물·VCS 는 소음일 뿐이다.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
];

const MAX_PREVIEW_LINES: usize = 40;

pub struct FileTree {
    pub root: PathBuf,
    pub rows: Vec<TreeRow>,
    pub cursor: usize,
    expanded: HashSet<PathBuf>,
    pub preview: Option<Preview>,
}

pub struct TreeRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

pub struct Preview {
    pub name: String,
    pub lines: Vec<String>,
}

impl FileTree {
    pub fn open(root: &Path) -> Self {
        let mut tree = Self {
            root: root.to_path_buf(),
            rows: Vec::new(),
            cursor: 0,
            expanded: HashSet::new(),
            preview: None,
        };
        tree.expanded.insert(root.to_path_buf());
        tree.rebuild();
        tree
    }

    /// 펼쳐진 디렉터만 재귀해 보이는 줄을 다시 만든다. 스캔은 항상 처음부터 —
    /// 디렉터 규모가 탐색기를 쓸 만한 수준이면 재구축 비용은 무시할 수 있다.
    fn rebuild(&mut self) {
        self.rows.clear();
        self.walk(&self.root.clone(), 0);
        if self.cursor >= self.rows.len() {
            self.cursor = self.rows.len().saturating_sub(1);
        }
    }

    fn walk(&mut self, dir: &Path, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut children: Vec<(PathBuf, String, bool)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    return None;
                }
                let path = entry.path();
                let is_dir = path.is_dir();
                if is_dir && SKIP_DIRS.contains(&name.as_str()) {
                    return None;
                }
                Some((path, name, is_dir))
            })
            .collect();
        // 디렉터 먼저, 이름은 대소문자 무시 오름차순.
        children.sort_by(|a, b| {
            b.2.cmp(&a.2).then_with(|| {
                a.1.to_lowercase()
                    .cmp(&b.1.to_lowercase())
                    .then_with(|| a.1.cmp(&b.1))
            })
        });
        for (path, name, is_dir) in children {
            let expanded = is_dir && self.expanded.contains(&path);
            self.rows.push(TreeRow {
                path: path.clone(),
                name,
                depth,
                is_dir,
                expanded,
            });
            if expanded {
                self.walk(&path, depth + 1);
            }
        }
    }

    pub fn selected(&self) -> Option<&TreeRow> {
        self.rows.get(self.cursor)
    }

    pub fn move_down(&mut self) {
        if self.cursor + 1 < self.rows.len() {
            self.cursor += 1;
        }
    }

    pub fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Enter/→ — 디렉터는 펼치기·접기, 파일은 미리보기를 연다.
    pub fn activate(&mut self) {
        let Some(row) = self.selected() else {
            return;
        };
        if row.is_dir {
            let path = row.path.clone();
            if !self.expanded.remove(&path) {
                self.expanded.insert(path);
            }
            self.rebuild();
            return;
        }
        let path = row.path.clone();
        let name = row.name.clone();
        let lines: Vec<String> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .take(MAX_PREVIEW_LINES)
            .map(str::to_string)
            .collect();
        self.preview = Some(Preview { name, lines });
    }

    /// ← — 펼쳐진 디렉터면 접고, 아니면 커서를 한 칸 올린다.
    pub fn collapse_or_up(&mut self) {
        if let Some(row) = self.selected()
            && row.is_dir
            && row.expanded
        {
            let path = row.path.clone();
            self.expanded.remove(&path);
            self.rebuild();
            return;
        }
        self.move_up();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> (PathBuf, FileTree) {
        // 테스트는 병렬로 돈다 — 디렉터를 호출마다 고유하게 만들어 경합을 없앤다.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rafikx-tree-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/agent")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("README.md"), "# sample").unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\nfn second() {}").unwrap();
        std::fs::write(dir.join("src/agent/deep.rs"), "deep").unwrap();
        let tree = FileTree::open(&dir);
        (dir, tree)
    }

    #[test]
    fn open_lists_root_children_and_skips_noise() {
        let (dir, tree) = sample_tree();
        let names: Vec<&str> = tree.rows.iter().map(|r| r.name.as_str()).collect();
        // 숨김·SKIP_DIRS 는 처음부터 보이지 않는다.
        assert!(!names.contains(&"target"));
        assert!(names.contains(&"README.md"));
        assert!(names.contains(&"src"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn activate_expands_and_collapses_directory() {
        let (dir, mut tree) = sample_tree();
        let idx = tree
            .rows
            .iter()
            .position(|r| r.name == "src")
            .expect("src row");
        tree.cursor = idx;
        tree.activate();
        assert!(tree.rows.iter().any(|r| r.name == "agent"));
        assert!(tree.rows.iter().any(|r| r.name == "main.rs"));
        // 접으면 하위가 사라진다.
        tree.activate();
        assert!(!tree.rows.iter().any(|r| r.name == "main.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_activation_opens_bounded_preview() {
        let (dir, mut tree) = sample_tree();
        let idx = tree
            .rows
            .iter()
            .position(|r| r.name == "README.md")
            .expect("readme row");
        tree.cursor = idx;
        tree.activate();
        let preview = tree.preview.expect("preview opened");
        assert_eq!(preview.lines, vec!["# sample"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
