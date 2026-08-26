use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use super::{MutationOp, MutationPlan, MutationReceipt, MutationState, read_state};

struct Prepared {
    operation: MutationOp,
    staged: Option<PathBuf>,
    backup: PathBuf,
}

struct Applied {
    target: PathBuf,
    backup: Option<PathBuf>,
    installed: bool,
}

pub(super) fn execute(plan: MutationPlan, fail_after: Option<usize>) -> Result<MutationReceipt> {
    validate_preconditions(&plan.operations)?;
    let transaction_dir = create_transaction_dir(&plan.workspace)?;
    let result = execute_in_dir(plan.operations, &transaction_dir, fail_after);
    let cleanup = fs::remove_dir_all(&transaction_dir);
    match (result, cleanup) {
        (Ok(receipt), _) => Ok(receipt),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(anyhow!(
            "{error}; transaction cleanup failed: {cleanup_error}"
        )),
    }
}

fn execute_in_dir(
    operations: Vec<MutationOp>,
    transaction_dir: &Path,
    fail_after: Option<usize>,
) -> Result<MutationReceipt> {
    let staged_dir = transaction_dir.join("staged");
    let backup_dir = transaction_dir.join("backup");
    fs::create_dir_all(&staged_dir)?;
    fs::create_dir_all(&backup_dir)?;
    let prepared = prepare(operations, &staged_dir, &backup_dir)?;
    let mut applied = Vec::new();
    let mut created_dirs = Vec::new();

    for (index, item) in prepared.iter().enumerate() {
        if fail_after == Some(index) {
            rollback(&applied, &created_dirs)?;
            return Err(anyhow!("injected mutation failure after {index} commits"));
        }
        if let Err(error) = verify_state(&item.operation) {
            rollback(&applied, &created_dirs)?;
            return Err(error);
        }
        match apply_one(item, &mut created_dirs) {
            Ok(step) => applied.push(step),
            Err(error) => {
                rollback(&applied, &created_dirs)?;
                return Err(error);
            }
        }
    }

    Ok(receipt(&prepared))
}

fn validate_preconditions(operations: &[MutationOp]) -> Result<()> {
    for operation in operations {
        verify_state(operation)?;
    }
    Ok(())
}

fn verify_state(operation: &MutationOp) -> Result<()> {
    let current = read_state(&operation.target)?;
    if current != operation.before {
        return Err(anyhow!(
            "mutation precondition changed for {}",
            operation.target.display()
        ));
    }
    Ok(())
}

fn prepare(
    operations: Vec<MutationOp>,
    staged_dir: &Path,
    backup_dir: &Path,
) -> Result<Vec<Prepared>> {
    let mut prepared = Vec::with_capacity(operations.len());
    for (index, operation) in operations.into_iter().enumerate() {
        let staged = if let Some(bytes) = &operation.after {
            let path = staged_dir.join(index.to_string());
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(bytes)?;
            if let Ok(metadata) = fs::metadata(&operation.target) {
                fs::set_permissions(&path, metadata.permissions())?;
            }
            file.sync_all()?;
            Some(path)
        } else {
            None
        };
        prepared.push(Prepared {
            operation,
            staged,
            backup: backup_dir.join(index.to_string()),
        });
    }
    Ok(prepared)
}

fn apply_one(item: &Prepared, created_dirs: &mut Vec<PathBuf>) -> Result<Applied> {
    let existed = matches!(item.operation.before, MutationState::Present(_));
    if existed {
        fs::rename(&item.operation.target, &item.backup)?;
    }
    if let Some(staged) = &item.staged {
        if let Some(parent) = item.operation.target.parent() {
            create_missing_dirs(parent, created_dirs)?;
        }
        if let Err(error) = fs::rename(staged, &item.operation.target) {
            if existed {
                let _ = fs::rename(&item.backup, &item.operation.target);
            }
            return Err(error.into());
        }
    }
    sync_parent(&item.operation.target);
    Ok(Applied {
        target: item.operation.target.clone(),
        backup: existed.then(|| item.backup.clone()),
        installed: item.staged.is_some(),
    })
}

fn rollback(applied: &[Applied], created_dirs: &[PathBuf]) -> Result<()> {
    let mut failures = Vec::new();
    for step in applied.iter().rev() {
        if step.installed
            && step.target.exists()
            && let Err(error) = fs::remove_file(&step.target)
        {
            failures.push(error.to_string());
        }
        if let Some(backup) = &step.backup
            && backup.exists()
            && let Err(error) = fs::rename(backup, &step.target)
        {
            failures.push(error.to_string());
        }
        sync_parent(&step.target);
    }
    for directory in created_dirs.iter().rev() {
        let _ = fs::remove_dir(directory);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("mutation rollback failed: {}", failures.join("; ")))
    }
}

fn create_missing_dirs(path: &Path, created: &mut Vec<PathBuf>) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let mut missing = Vec::new();
    let mut cursor = Some(path);
    while let Some(current) = cursor {
        if current.exists() {
            break;
        }
        missing.push(current.to_path_buf());
        cursor = current.parent();
    }
    for directory in missing.into_iter().rev() {
        fs::create_dir(&directory)?;
        created.push(directory);
    }
    Ok(())
}

fn create_transaction_dir(workspace: &Path) -> Result<PathBuf> {
    for _ in 0..16 {
        let candidate = workspace.join(format!(".rafikx-txn-{}", crate::db::Db::new_id()));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(anyhow!("could not allocate mutation transaction directory"))
}

fn receipt(prepared: &[Prepared]) -> MutationReceipt {
    let mut created = Vec::new();
    let mut updated = Vec::new();
    let mut deleted = Vec::new();
    let mut changed = Vec::new();
    for item in prepared {
        let target = item.operation.target.clone();
        changed.push(target.clone());
        match (&item.operation.before, &item.operation.after) {
            (MutationState::Missing, Some(_)) => created.push(target),
            (MutationState::Present(_), Some(_)) => updated.push(target),
            (MutationState::Present(_), None) => deleted.push(target),
            (MutationState::Missing, None) => {}
        }
    }
    MutationReceipt {
        committed: true,
        changed,
        created,
        updated,
        deleted,
    }
}

fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
}
