mod commit;

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, anyhow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationState {
    Missing,
    Present(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationReceipt {
    pub committed: bool,
    pub changed: Vec<PathBuf>,
    pub created: Vec<PathBuf>,
    pub updated: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct MutationPlan {
    workspace: PathBuf,
    operations: Vec<MutationOp>,
}

#[derive(Debug)]
pub(super) struct MutationOp {
    pub(super) target: PathBuf,
    pub(super) before: MutationState,
    pub(super) after: Option<Vec<u8>>,
}

impl MutationPlan {
    pub fn new(workspace: &Path) -> Result<Self> {
        let workspace = workspace
            .canonicalize()
            .map_err(|error| anyhow!("workspace cannot be resolved: {error}"))?;
        if !workspace.is_dir() {
            return Err(anyhow!(
                "workspace is not a directory: {}",
                workspace.display()
            ));
        }
        Ok(Self {
            workspace,
            operations: Vec::new(),
        })
    }

    pub fn replace(&mut self, target: &Path, before: MutationState, after: Vec<u8>) -> Result<()> {
        self.push(target, before, Some(after))
    }

    pub fn delete(&mut self, target: &Path, before: MutationState) -> Result<()> {
        if matches!(before, MutationState::Missing) {
            return Err(anyhow!("delete precondition requires an existing file"));
        }
        self.push(target, before, None)
    }

    pub fn commit(self) -> Result<MutationReceipt> {
        commit::execute(self, None)
    }

    fn push(&mut self, target: &Path, before: MutationState, after: Option<Vec<u8>>) -> Result<()> {
        let target = resolve_target(&self.workspace, target)?;
        if self
            .operations
            .iter()
            .any(|operation| operation.target == target)
        {
            return Err(anyhow!("duplicate mutation target: {}", target.display()));
        }
        self.operations.push(MutationOp {
            target,
            before,
            after,
        });
        Ok(())
    }
}

pub fn read_state(path: &Path) -> Result<MutationState> {
    match fs::read(path) {
        Ok(bytes) => Ok(MutationState::Present(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MutationState::Missing),
        Err(error) => Err(error.into()),
    }
}

fn resolve_target(workspace: &Path, target: &Path) -> Result<PathBuf> {
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        workspace.join(target)
    };
    if joined
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(anyhow!(
            "mutation path may not contain '..': {}",
            target.display()
        ));
    }
    if joined.exists() {
        let canonical = joined.canonicalize()?;
        if !canonical.starts_with(workspace) {
            return Err(anyhow!(
                "mutation path escapes workspace: {}",
                target.display()
            ));
        }
        if canonical.is_dir() {
            return Err(anyhow!(
                "mutation target is a directory: {}",
                target.display()
            ));
        }
        return Ok(canonical);
    }

    let mut ancestor = joined.parent();
    while let Some(path) = ancestor {
        if path.exists() {
            let canonical = path.canonicalize()?;
            if !canonical.starts_with(workspace) {
                return Err(anyhow!(
                    "mutation path escapes workspace: {}",
                    target.display()
                ));
            }
            let suffix = joined.strip_prefix(path).map_err(|_| {
                anyhow!("mutation target cannot be normalized: {}", target.display())
            })?;
            return Ok(canonical.join(suffix));
        }
        ancestor = path.parent();
    }
    Err(anyhow!(
        "mutation target has no existing workspace ancestor"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rafikx-mutation-rollback-{}",
                crate::db::Db::new_id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mutation_failure_restores_every_original() {
        let root = TestDir::new();
        let first = root.0.join("first.txt");
        let second = root.0.join("second.txt");
        let deleted = root.0.join("deleted.txt");
        fs::write(&first, b"one").expect("seed first");
        fs::write(&second, b"two").expect("seed second");
        fs::write(&deleted, b"three").expect("seed deleted");

        let mut plan = MutationPlan::new(&root.0).expect("plan");
        plan.replace(
            &first,
            read_state(&first).expect("first state"),
            b"ONE".to_vec(),
        )
        .expect("first op");
        plan.delete(&deleted, read_state(&deleted).expect("delete state"))
            .expect("delete op");
        plan.replace(
            &second,
            read_state(&second).expect("second state"),
            b"TWO".to_vec(),
        )
        .expect("second op");

        let error = commit::execute(plan, Some(2)).expect_err("injected failure");
        assert!(error.to_string().contains("injected"));
        assert_eq!(fs::read(first).expect("first restored"), b"one");
        assert_eq!(fs::read(second).expect("second restored"), b"two");
        assert_eq!(fs::read(deleted).expect("delete restored"), b"three");
        let leftovers = fs::read_dir(&root.0)
            .expect("list root")
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with(".rafikx-txn-"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
    }
}
