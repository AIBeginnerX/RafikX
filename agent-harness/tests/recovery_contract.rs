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
        vec![workspace.path().join("game.html")]
    );
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
