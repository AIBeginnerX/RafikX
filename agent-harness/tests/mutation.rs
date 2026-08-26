use std::fs;
use std::path::{Path, PathBuf};

use rafikx::run::{RunContext, RunId};
use rafikx::tools::mutation::{MutationPlan, MutationState};
use rafikx::tools::{ToolCtx, ToolRegistry};
use serde_json::json;

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("rafikx-{label}-{}", rafikx::db::Db::new_id()));
        fs::create_dir_all(&path).expect("create test workspace");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn mutation_multi_file_commit_returns_receipt() {
    let workspace = TestDir::new("mutation-commit");
    let update = workspace.path().join("update.txt");
    let delete = workspace.path().join("delete.txt");
    let create = workspace.path().join("nested/create.txt");
    fs::write(&update, b"before").expect("seed update");
    fs::write(&delete, b"remove me").expect("seed delete");

    let mut plan = MutationPlan::new(workspace.path()).expect("valid workspace");
    plan.replace(
        &update,
        MutationState::Present(b"before".to_vec()),
        b"after".to_vec(),
    )
    .expect("stage update");
    plan.delete(&delete, MutationState::Present(b"remove me".to_vec()))
        .expect("stage delete");
    plan.replace(&create, MutationState::Missing, b"created".to_vec())
        .expect("stage create");

    let receipt = plan.commit().expect("transaction commits");
    assert_eq!(fs::read(&update).expect("updated bytes"), b"after");
    assert!(!delete.exists());
    assert_eq!(fs::read(&create).expect("created bytes"), b"created");
    assert!(receipt.committed);
    assert_eq!(
        receipt.created,
        vec![create.canonicalize().expect("canonical create")]
    );
    assert_eq!(
        receipt.updated,
        vec![update.canonicalize().expect("canonical update")]
    );
    assert_eq!(
        receipt.deleted,
        vec![
            workspace
                .path()
                .canonicalize()
                .expect("canonical workspace")
                .join("delete.txt")
        ]
    );
    assert_eq!(receipt.changed.len(), 3);
}

#[test]
fn mutation_stale_precondition_changes_nothing() {
    let workspace = TestDir::new("mutation-stale");
    let first = workspace.path().join("first.txt");
    let second = workspace.path().join("second.txt");
    fs::write(&first, b"first-before").expect("seed first");
    fs::write(&second, b"second-current").expect("seed second");

    let mut plan = MutationPlan::new(workspace.path()).expect("valid workspace");
    plan.replace(
        &first,
        MutationState::Present(b"first-before".to_vec()),
        b"first-after".to_vec(),
    )
    .expect("stage first");
    plan.replace(
        &second,
        MutationState::Present(b"stale".to_vec()),
        b"second-after".to_vec(),
    )
    .expect("stage second");

    let error = plan.commit().expect_err("stale transaction must fail");
    assert!(error.to_string().contains("precondition"));
    assert_eq!(fs::read(&first).expect("first unchanged"), b"first-before");
    assert_eq!(
        fs::read(&second).expect("second unchanged"),
        b"second-current"
    );
    let leftovers = fs::read_dir(workspace.path())
        .expect("list workspace")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(".rafikx-txn-"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "transaction leftovers: {leftovers:?}");
}

#[test]
fn mutation_tools_commit_receipts_and_failed_patch_is_atomic() {
    let workspace = TestDir::new("mutation-tools");
    let edited = workspace.path().join("edited.txt");
    let multi = workspace.path().join("multi.txt");
    let duplicate = workspace.path().join("duplicate.txt");
    fs::write(&edited, "alpha").expect("seed edit");
    fs::write(&multi, "one two").expect("seed multi edit");
    fs::write(&duplicate, "x\nx\n").expect("seed duplicate");

    let run = RunContext::isolated(RunId::new("tool-run"), workspace.path().to_path_buf());
    let mut ctx = ToolCtx::new(workspace.path().to_path_buf());
    ctx.run = Some(run.clone());
    let registry = ToolRegistry::all();

    registry
        .get("edit_file")
        .expect("edit tool")
        .run(
            json!({"path":"edited.txt","old_str":"alpha","new_str":"beta"}),
            &ctx,
        )
        .expect("edit commits");
    registry
        .get("multi_edit")
        .expect("multi edit tool")
        .run(
            json!({"path":"multi.txt","edits":[
                {"old_str":"one","new_str":"ONE"},
                {"old_str":"two","new_str":"TWO"}
            ]}),
            &ctx,
        )
        .expect("multi edit commits");
    registry
        .get("write_file")
        .expect("write tool")
        .run(json!({"path":"nested/new.txt","content":"new"}), &ctx)
        .expect("write commits");

    let error = registry
        .get("apply_patch")
        .expect("patch tool")
        .run(
            json!({"patch":"*** Begin Patch\n*** Add File: should-not-exist.txt\n+staged\n*** Update File: duplicate.txt\n@@\n-x\n+y\n*** End Patch"}),
            &ctx,
        )
        .expect_err("invalid patch is rejected");
    assert!(error.to_string().contains("2번"));
    assert!(!workspace.path().join("should-not-exist.txt").exists());
    assert_eq!(
        fs::read_to_string(&duplicate).expect("duplicate unchanged"),
        "x\nx\n"
    );
    assert_eq!(fs::read_to_string(&edited).expect("edited value"), "beta");
    assert_eq!(fs::read_to_string(&multi).expect("multi value"), "ONE TWO");
    assert_eq!(
        fs::read_to_string(workspace.path().join("nested/new.txt")).expect("new value"),
        "new"
    );
    assert_eq!(run.committed_paths().len(), 3);
}
