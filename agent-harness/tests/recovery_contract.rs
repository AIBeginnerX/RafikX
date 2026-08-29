use std::fs;
use std::path::{Path, PathBuf};

use rafikx::harness::{TaskClass, classify_rules};
use rafikx::quality::detect_duplicate_blocks;
use rafikx::quality::run_quality_gate;
use rafikx::run::{RunContext, RunId};
use rafikx::tools::{ToolCtx, ToolRegistry};
use serde_json::json;

struct TestWorkspace(PathBuf);

impl TestWorkspace {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rafikx-recovery-{label}-{}",
            rafikx::db::Db::new_id()
        ));
        fs::create_dir_all(&path).expect("create test workspace");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn english_game_creation_routes_to_dev_tools() {
    // Given: an English artifact request with no file extension.
    let prompt = "create a Super Mario browser game";

    // When: rules classify the request.
    let class = classify_rules(prompt, false);

    // Then: it must receive the tool-capable development profile.
    assert_eq!(class, TaskClass::Dev);
    assert_eq!(classify_rules("write a browser game", false), TaskClass::Dev);
    assert_eq!(classify_rules("fix my browser game", false), TaskClass::Dev);
    assert_eq!(classify_rules("make me happy", false), TaskClass::Simple);
}

#[test]
fn legitimate_parallel_initializers_are_not_duplicate_code() {
    // Given: two domain objects that legitimately initialize the same coordinates.
    let source = r#"
class Player {
  reset() {
    this.x = 0;
    this.y = 0;
    this.velocity = 0;
  }
}
class Enemy {
  reset() {
    this.x = 0;
    this.y = 0;
    this.velocity = 0;
  }
}
"#;

    // When: the readability scanner inspects the file.
    let findings = detect_duplicate_blocks("game.js", source);

    // Then: a normal three-line initializer must not fail the whole quality gate.
    assert!(
        findings.is_empty(),
        "false duplicate findings: {findings:?}"
    );
}

#[test]
fn substantial_copied_block_is_reported() {
    // Given: a substantial five-line implementation copied verbatim.
    let block = r#"
const horizontal = input.left - input.right;
player.velocity += horizontal * acceleration;
player.position += player.velocity;
player.velocity *= friction;
renderPlayer(player.position, player.velocity);
"#;
    let source = format!("function first() {{\n{block}}}\nfunction second() {{\n{block}}}\n");

    // When: the scanner inspects the copied implementation.
    let findings = detect_duplicate_blocks("game.js", &source);

    // Then: meaningful copy/paste remains detectable.
    assert_eq!(findings.len(), 1, "duplicate findings: {findings:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bash_nonzero_exit_is_an_error_result() {
    // Given: the real Bash tool in an isolated workspace.
    let workspace = TestWorkspace::new("bash-exit");
    let context = ToolCtx::new(workspace.path().to_path_buf());
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    // When: a command exits unsuccessfully.
    let result = bash.run(json!({"command": "printf failure >&2; exit 7"}), &context);

    // Then: the agent loop must receive a tool error, including its evidence.
    let error = result.expect_err("nonzero exit must be a tool error");
    let message = error.to_string();
    assert!(message.contains("failure"), "missing stderr: {message}");
    assert!(message.contains('7'), "missing exit code: {message}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bash_created_files_are_recorded_as_execution_evidence() {
    let workspace = TestWorkspace::new("bash-mutation");
    let run = RunContext::isolated(
        RunId::new("bash-mutation-test"),
        workspace.path().to_path_buf(),
    );
    let mut context = ToolCtx::new(workspace.path().to_path_buf());
    context.run = Some(run.clone());
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    bash.run(
        json!({"command": "printf '<canvas></canvas>' > game.html"}),
        &context,
    )
    .expect("bash write");

    assert_eq!(
        run.committed_paths(),
        vec![workspace
            .path()
            .join("game.html")
            .canonicalize()
            .expect("canonical game")]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reverted_bash_changes_are_not_execution_evidence() {
    let workspace = TestWorkspace::new("bash-revert");
    fs::write(workspace.path().join("game.js"), "original").expect("seed source");
    let run = RunContext::isolated(
        RunId::new("bash-revert-test"),
        workspace.path().to_path_buf(),
    );
    let mut context = ToolCtx::new(workspace.path().to_path_buf());
    context.run = Some(run.clone());
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    bash.run(
        json!({"command": "printf changed > game.js"}),
        &context,
    )
    .expect("bash change");
    assert_eq!(
        run.committed_paths(),
        vec![workspace
            .path()
            .join("game.js")
            .canonicalize()
            .expect("canonical game")]
    );

    bash.run(
        json!({"command": "printf original > game.js"}),
        &context,
    )
    .expect("bash revert");
    assert!(run.committed_paths().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untrackable_workspace_prevents_bash_execution() {
    let workspace = TestWorkspace::new("bash-tracking-cap");
    let oversized = fs::File::create(workspace.path().join("oversized.bin"))
        .expect("oversized fixture");
    oversized
        .set_len(64 * 1024 * 1024 + 1)
        .expect("sparse oversized fixture");
    let run = RunContext::isolated(
        RunId::new("bash-tracking-cap-test"),
        workspace.path().to_path_buf(),
    );
    let mut context = ToolCtx::new(workspace.path().to_path_buf());
    context.run = Some(run);
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    let error = bash
        .run(json!({"command": "printf ran > marker.txt"}), &context)
        .expect_err("untrackable workspace must block bash");
    assert!(error.to_string().contains("실행 전 변경 추적 실패"));
    assert!(!workspace.path().join("marker.txt").exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_workspace_symlink_prevents_bash_execution() {
    let workspace = TestWorkspace::new("bash-external-link");
    let outside = TestWorkspace::new("bash-external-target");
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("linked"))
        .expect("external symlink");
    let run = RunContext::isolated(
        RunId::new("bash-external-link-test"),
        workspace.path().to_path_buf(),
    );
    let mut context = ToolCtx::new(workspace.path().to_path_buf());
    context.run = Some(run);
    let registry = ToolRegistry::all();
    let bash = registry.get("bash").expect("bash tool");

    let error = bash
        .run(
            json!({"command": "printf escaped > linked/outside.txt"}),
            &context,
        )
        .expect_err("external symlink must block bash before execution");
    assert!(error.to_string().contains("실행 전 변경 추적 실패"));
    assert!(!outside.path().join("outside.txt").exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_symlink_alias_tools_share_one_final_baseline() {
    let workspace = TestWorkspace::new("mixed-alias-real");
    let alias = workspace.path().with_extension("alias");
    std::os::unix::fs::symlink(workspace.path(), &alias).expect("workspace alias");
    fs::write(workspace.path().join("game.js"), "original").expect("seed source");
    let run = RunContext::isolated(RunId::new("mixed-alias-test"), alias.clone());
    let mut context = ToolCtx::new(alias.clone());
    context.run = Some(run.clone());
    let registry = ToolRegistry::all();

    registry
        .get("bash")
        .expect("bash tool")
        .run(json!({"command": "printf changed > game.js"}), &context)
        .expect("bash change");
    registry
        .get("write_file")
        .expect("write tool")
        .run(
            json!({"path": "game.js", "content": "original"}),
            &context,
        )
        .expect("file-tool revert");

    assert!(run.committed_paths().is_empty());
    let _ = fs::remove_file(alias);
}

#[tokio::test]
async fn duplicate_advice_does_not_fail_a_valid_artifact() {
    // Given: a valid artifact with a substantial repeated implementation.
    let workspace = TestWorkspace::new("duplicate-advice");
    let block = r#"
const horizontal = input.left - input.right;
player.velocity += horizontal * acceleration;
player.position += player.velocity;
player.velocity *= friction;
renderPlayer(player.position, player.velocity);
"#;
    fs::write(
        workspace.path().join("game.txt"),
        format!("first {{\n{block}}}\nsecond {{\n{block}}}\n"),
    )
    .expect("write fixture");

    // When: the quality gate finds only heuristic duplication advice.
    let report = run_quality_gate(workspace.path(), &["game.txt".to_string()]).await;

    // Then: advice is visible but cannot reject an otherwise valid artifact.
    assert!(
        report.passed,
        "advisory caused failure: {:?}",
        report.findings
    );
    assert!(
        report
            .steps
            .iter()
            .any(|step| step.stage == "S7-duplication")
    );
}
