use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

const MAX_SNAPSHOT_ENTRIES: usize = 500_000;
const MAX_TRACKED_FILES: usize = 25_000;
const MAX_TRACKED_CHANGES: usize = 25_000;
const MAX_SINGLE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HASH_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SNAPSHOT_DURATION: Duration = Duration::from_secs(3);
const READ_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct SnapshotLimits {
    max_entries: usize,
    max_files: usize,
    max_changes: usize,
    max_single_file_bytes: u64,
    max_hash_bytes: u64,
    max_duration: Duration,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            max_entries: MAX_SNAPSHOT_ENTRIES,
            max_files: MAX_TRACKED_FILES,
            max_changes: MAX_TRACKED_CHANGES,
            max_single_file_bytes: MAX_SINGLE_FILE_BYTES,
            max_hash_bytes: MAX_HASH_BYTES,
            max_duration: MAX_SNAPSHOT_DURATION,
        }
    }
}

struct SnapshotBudget {
    limits: SnapshotLimits,
    started: Instant,
    entries: usize,
    files: usize,
    hash_bytes: u64,
}

impl SnapshotBudget {
    fn new(limits: SnapshotLimits) -> Self {
        Self {
            limits,
            started: Instant::now(),
            entries: 0,
            files: 0,
            hash_bytes: 0,
        }
    }

    fn check_deadline(&self) -> Result<()> {
        if self.started.elapsed() > self.limits.max_duration {
            return Err(anyhow!(
                "워크스페이스 스냅샷 시간 상한({}ms)을 초과했습니다",
                self.limits.max_duration.as_millis()
            ));
        }
        Ok(())
    }

    fn record_entry(&mut self) -> Result<()> {
        self.check_deadline()?;
        self.entries = self.entries.saturating_add(1);
        if self.entries > self.limits.max_entries {
            return Err(anyhow!(
                "워크스페이스 항목 수가 추적 상한({})을 초과했습니다",
                self.limits.max_entries
            ));
        }
        Ok(())
    }

    fn reserve_file(&mut self, len: u64) -> Result<()> {
        self.check_deadline()?;
        self.files = self.files.saturating_add(1);
        if self.files > self.limits.max_files {
            return Err(anyhow!(
                "워크스페이스 파일 수가 추적 상한({})을 초과했습니다",
                self.limits.max_files
            ));
        }
        if len > self.limits.max_single_file_bytes {
            return Err(anyhow!(
                "파일 크기가 변경 추적 상한({} bytes)을 초과했습니다",
                self.limits.max_single_file_bytes
            ));
        }
        let next = self
            .hash_bytes
            .checked_add(len)
            .ok_or_else(|| anyhow!("워크스페이스 해시 바이트 계산이 넘쳤습니다"))?;
        if next > self.limits.max_hash_bytes {
            return Err(anyhow!(
                "워크스페이스 해시 양이 추적 상한({} bytes)을 초과했습니다",
                self.limits.max_hash_bytes
            ));
        }
        self.hash_bytes = next;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileFingerprint {
    len: u64,
    content_hash: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileBaseline {
    pub(crate) path: PathBuf,
    pub(crate) fingerprint: Option<FileFingerprint>,
}

fn hash_chunk(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

pub(crate) fn fingerprint_bytes(bytes: &[u8]) -> FileFingerprint {
    let mut content_hash = 0xcbf29ce484222325u64;
    hash_chunk(&mut content_hash, bytes);
    FileFingerprint {
        len: bytes.len() as u64,
        content_hash,
    }
}

fn fingerprint_file(path: &Path, budget: &mut SnapshotBudget) -> Result<FileFingerprint> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| anyhow!("파일을 열 수 없습니다 ({}): {error}", path.display()))?;
    let metadata = file.metadata().map_err(|error| {
        anyhow!(
            "파일 메타데이터를 읽을 수 없습니다 ({}): {error}",
            path.display()
        )
    })?;
    let len = metadata.len();
    let modified = metadata.modified().ok();
    budget.reserve_file(len)?;

    let mut content_hash = 0xcbf29ce484222325u64;
    let mut buffer = [0u8; READ_BUFFER_BYTES];
    let mut remaining = len;
    while remaining > 0 {
        budget.check_deadline()?;
        let chunk = usize::try_from(remaining.min(READ_BUFFER_BYTES as u64))
            .map_err(|_| anyhow!("파일 읽기 크기를 계산할 수 없습니다"))?;
        let read = file
            .read(&mut buffer[..chunk])
            .map_err(|error| anyhow!("파일을 읽을 수 없습니다 ({}): {error}", path.display()))?;
        if read == 0 {
            return Err(anyhow!(
                "스냅샷 중 파일 크기가 바뀌었습니다 ({})",
                path.display()
            ));
        }
        hash_chunk(&mut content_hash, &buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    budget.check_deadline()?;
    let mut extra = [0u8; 1];
    if file
        .read(&mut extra)
        .map_err(|error| anyhow!("파일을 읽을 수 없습니다 ({}): {error}", path.display()))?
        != 0
    {
        return Err(anyhow!(
            "스냅샷 중 파일 크기가 바뀌었습니다 ({})",
            path.display()
        ));
    }
    let after = file.metadata().map_err(|error| {
        anyhow!(
            "파일 메타데이터를 다시 읽을 수 없습니다 ({}): {error}",
            path.display()
        )
    })?;
    if after.len() != len || after.modified().ok() != modified {
        return Err(anyhow!(
            "스냅샷 중 파일이 바뀌었습니다 ({})",
            path.display()
        ));
    }
    Ok(FileFingerprint { len, content_hash })
}

fn fingerprint_path(path: &Path, budget: &mut SnapshotBudget) -> Result<Option<FileFingerprint>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => fingerprint_file(path, budget).map(Some),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(anyhow!(
            "파일 상태를 읽을 수 없습니다 ({}): {error}",
            path.display()
        )),
    }
}

fn is_excluded_directory_name(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_string_lossy().as_ref(),
        ".cache"
            | ".git"
            | ".gradle"
            | ".next"
            | ".nuxt"
            | ".parcel-cache"
            | ".svelte-kit"
            | ".turbo"
            | ".venv"
            | ".vite"
            | ".yarn"
            | "Pods"
            | "__pycache__"
            | "node_modules"
            | "target"
            | "venv"
            | "vendor"
    )
}

fn has_excluded_directory(path: &Path) -> bool {
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            break;
        }
        if let std::path::Component::Normal(name) = component
            && is_excluded_directory_name(name)
        {
            return true;
        }
    }
    false
}

pub(crate) fn fingerprint_paths(
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<BTreeMap<PathBuf, Option<FileFingerprint>>> {
    let limits = SnapshotLimits::default();
    let mut budget = SnapshotBudget::new(limits);
    let mut fingerprints = BTreeMap::new();
    for path in paths {
        budget.record_entry()?;
        if fingerprints.len() >= limits.max_changes {
            return Err(anyhow!(
                "변경 후보 수가 추적 상한({})을 초과했습니다",
                limits.max_changes
            ));
        }
        let fingerprint = fingerprint_path(&path, &mut budget)?;
        fingerprints.insert(path, fingerprint);
    }
    Ok(fingerprints)
}

pub(crate) struct WorkspaceSnapshot {
    root: PathBuf,
    files: BTreeMap<PathBuf, FileFingerprint>,
    limits: SnapshotLimits,
}

impl WorkspaceSnapshot {
    pub(crate) fn capture(root: &Path) -> Result<Self> {
        Self::capture_with_limits(root, SnapshotLimits::default())
    }

    fn capture_with_limits(root: &Path, limits: SnapshotLimits) -> Result<Self> {
        let root = root
            .canonicalize()
            .map_err(|error| anyhow!("워크스페이스를 확인할 수 없습니다: {error}"))?;
        if !root.is_dir() {
            return Err(anyhow!("워크스페이스가 디렉터리가 아닙니다"));
        }
        let mut files = BTreeMap::new();
        let mut budget = SnapshotBudget::new(limits);
        let mut builder = ignore::WalkBuilder::new(&root);
        builder
            .hidden(false)
            .ignore(false)
            .git_global(false)
            .git_ignore(false)
            .git_exclude(false)
            .same_file_system(true)
            .sort_by_file_path(|left, right| left.cmp(right));
        for entry in builder.build() {
            budget.record_entry()?;
            let entry = entry.map_err(|error| anyhow!("워크스페이스 순회 실패: {error}"))?;
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.into_path();
            let relative = path.strip_prefix(&root).map_err(|error| {
                anyhow!("워크스페이스 상대 경로를 확인할 수 없습니다: {error}")
            })?;
            let under_excluded_directory = has_excluded_directory(relative);
            if file_type.is_symlink() {
                let target = path.canonicalize().map_err(|error| {
                    anyhow!("워크스페이스 심볼릭 링크를 확인할 수 없습니다: {error}")
                })?;
                if !target.starts_with(&root) {
                    return Err(anyhow!(
                        "워크스페이스 밖을 가리키는 심볼릭 링크가 있습니다 ({})",
                        path.display()
                    ));
                }
                if under_excluded_directory {
                    continue;
                }
                if target.is_file() && !files.contains_key(&target) {
                    let fingerprint = fingerprint_file(&target, &mut budget)?;
                    files.insert(target, fingerprint);
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if under_excluded_directory {
                continue;
            }
            let normalized = path.canonicalize().map_err(|error| {
                anyhow!("워크스페이스 파일을 확인할 수 없습니다: {error}")
            })?;
            if !normalized.starts_with(&root) {
                return Err(anyhow!(
                    "워크스페이스 파일이 경계를 벗어났습니다 ({})",
                    path.display()
                ));
            }
            if !files.contains_key(&normalized) {
                let fingerprint = fingerprint_file(&normalized, &mut budget)?;
                files.insert(normalized, fingerprint);
            }
        }
        Ok(Self {
            root,
            files,
            limits,
        })
    }

    pub(crate) fn changed_baselines(&self) -> Result<Vec<FileBaseline>> {
        let after = Self::capture_with_limits(&self.root, self.limits)?;
        let paths: BTreeSet<&PathBuf> = self.files.keys().chain(after.files.keys()).collect();
        let mut changes = Vec::new();
        for path in paths {
            if self.files.get(path) == after.files.get(path) {
                continue;
            }
            if changes.len() >= self.limits.max_changes {
                return Err(anyhow!(
                    "변경 파일 수가 추적 상한({})을 초과했습니다",
                    self.limits.max_changes
                ));
            }
            changes.push(FileBaseline {
                path: path.clone(),
                fingerprint: self.files.get(path).cloned(),
            });
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "rafikx-workspace-delta-{label}-{}",
                crate::db::Db::new_id()
            ));
            std::fs::create_dir_all(&path).expect("temp workspace");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn same_length_rewrite_and_ignored_file_are_detected() {
        let root = TestDir::new("content");
        std::fs::write(root.0.join(".gitignore"), ".env\n").expect("gitignore");
        std::fs::write(root.0.join("game.js"), "alpha").expect("source");
        std::fs::write(root.0.join(".env"), "TOKEN=old").expect("ignored source");
        std::fs::create_dir_all(root.0.join(".cache")).expect("cache");
        std::fs::write(root.0.join(".cache/blob"), "old cache").expect("cache fixture");

        let snapshot = WorkspaceSnapshot::capture(&root.0).expect("snapshot");
        std::fs::write(root.0.join("game.js"), "bravo").expect("same length rewrite");
        std::fs::write(root.0.join(".env"), "TOKEN=new").expect("ignored rewrite");
        std::fs::write(root.0.join(".cache/blob"), "new cache").expect("cache rewrite");

        let changed = snapshot.changed_baselines().expect("delta");
        let paths = changed
            .iter()
            .map(|change| change.path.as_path())
            .collect::<Vec<_>>();
        let game = root.0.join("game.js").canonicalize().expect("canonical game");
        let env = root.0.join(".env").canonicalize().expect("canonical env");
        let cache = root
            .0
            .join(".cache/blob")
            .canonicalize()
            .expect("canonical cache");
        assert!(paths.contains(&game.as_path()));
        assert!(paths.contains(&env.as_path()));
        assert!(!paths.contains(&cache.as_path()));
    }

    #[test]
    fn metadata_only_touch_is_not_a_content_change() {
        let root = TestDir::new("touch");
        let source = root.0.join("game.js");
        std::fs::write(&source, "same content").expect("source");
        let snapshot = WorkspaceSnapshot::capture(&root.0).expect("snapshot");

        let status = std::process::Command::new("touch")
            .arg(&source)
            .status()
            .expect("touch command");
        assert!(status.success());
        assert!(snapshot.changed_baselines().expect("delta").is_empty());
    }

    #[test]
    fn aggregate_limits_fail_closed() {
        let root = TestDir::new("limits");
        std::fs::write(root.0.join("one"), "12345").expect("fixture");
        let entry_error = WorkspaceSnapshot::capture_with_limits(
            &root.0,
            SnapshotLimits {
                max_entries: 0,
                ..SnapshotLimits::default()
            },
        )
        .err()
        .expect("entry overflow must fail");
        assert!(entry_error.to_string().contains("항목 수"));

        let file_error = WorkspaceSnapshot::capture_with_limits(
            &root.0,
            SnapshotLimits {
                max_files: 0,
                ..SnapshotLimits::default()
            },
        )
        .err()
        .expect("file overflow must fail");
        assert!(file_error.to_string().contains("파일 수"));

        let limits = SnapshotLimits {
            max_hash_bytes: 4,
            ..SnapshotLimits::default()
        };
        let error = WorkspaceSnapshot::capture_with_limits(&root.0, limits)
            .err()
            .expect("oversized snapshot must fail");
        assert!(error.to_string().contains("해시 양"));
    }

    #[cfg(unix)]
    #[test]
    fn external_symlink_fails_before_a_command_can_run() {
        let workspace = TestDir::new("external-link-workspace");
        let outside = TestDir::new("external-link-target");
        std::fs::write(outside.0.join("target.txt"), "outside").expect("outside fixture");
        std::os::unix::fs::symlink(&outside.0, workspace.0.join("linked"))
            .expect("external symlink");

        let error = WorkspaceSnapshot::capture(&workspace.0)
            .err()
            .expect("external symlink must fail closed");
        assert!(error.to_string().contains("워크스페이스 밖"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn external_symlink_under_excluded_directory_fails_closed() {
        let workspace = TestDir::new("excluded-external-link-workspace");
        let outside = TestDir::new("excluded-external-link-target");
        std::fs::create_dir_all(workspace.0.join(".cache")).expect("excluded directory");
        std::fs::write(outside.0.join("target.txt"), "outside").expect("outside fixture");
        std::os::unix::fs::symlink(&outside.0, workspace.0.join(".cache/linked"))
            .expect("external symlink under excluded directory");

        let error = WorkspaceSnapshot::capture(&workspace.0)
            .err()
            .expect("excluded-directory external symlink must fail closed");
        assert!(error.to_string().contains("워크스페이스 밖"), "{error}");
    }
}
