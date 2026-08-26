use std::fs;
use std::path::{Path, PathBuf};

struct TestProject(PathBuf);

impl TestProject {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("rafikx-lsp-{}", rafikx::db::Db::new_id()));
        fs::create_dir_all(root.join("src")).expect("create LSP project");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='lsp-smoke'\nversion='0.1.0'\nedition='2024'\n",
        )
        .expect("write Cargo manifest");
        fs::write(
            root.join("src/lib.rs"),
            "pub fn answer() -> u32 { 42 }\npub fn call() -> u32 { answer() }\n",
        )
        .expect("write Rust source");
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "requires a locally installed rust-analyzer"]
async fn lsp_real_rust_analyzer_diagnostics_and_definition() {
    let project = TestProject::new();
    let source = project.path().join("src/lib.rs");
    let diagnostics = rafikx::lsp::diagnostics(project.path(), &source, None)
        .await
        .expect("rust-analyzer diagnostics");
    assert_eq!(diagnostics, "LSP diagnostics: clean");
    let definition = rafikx::lsp::definition(project.path(), &source, 2, 24, None)
        .await
        .expect("rust-analyzer definition");
    assert!(definition.contains("lib.rs:1:"), "{definition}");
}
