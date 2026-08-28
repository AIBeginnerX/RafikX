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

#[tokio::test]
#[ignore = "requires a locally installed rust-analyzer"]
async fn lsp_real_rust_analyzer_hover_and_references() {
    let project = TestProject::new();
    let source = project.path().join("src/lib.rs");

    // hover — 2행 24열은 call() 안의 answer() 호출부다. 타입 정보가 떠야 한다.
    let hover = rafikx::lsp::hover(project.path(), &source, 2, 24, None)
        .await
        .expect("rust-analyzer hover");
    assert!(
        hover.contains("u32") && !hover.contains("LSP hover: not found"),
        "{hover}"
    );

    // references — answer() 정의(1행 8열)를 가리키면 call() 의 호출이 잡혀야 한다.
    let references = rafikx::lsp::references(project.path(), &source, 1, 8, None)
        .await
        .expect("rust-analyzer references");
    assert!(
        references.contains("lib.rs:2:") && !references.contains("not found"),
        "{references}"
    );
}

#[tokio::test]
#[ignore = "requires typescript-language-server"]
async fn lsp_real_tsserver_quick_diagnostics_budget() {
    // RAFIKX_TS_DIR 로 실제 워크스페이스(node_modules 에 typescript 가 있는 곳)를 지정해
    // 실측할 수 있다. 없으면 임시 프로젝트를 만든다.
    let scratch;
    let dir = match std::env::var("RAFIKX_TS_DIR") {
        Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => {
            scratch = std::env::temp_dir().join(format!("rafikx-ts-{}", rafikx::db::Db::new_id()));
            std::fs::create_dir_all(&scratch).expect("create TS project");
            scratch
        }
    };
    let source = dir.join("app.ts");
    if !source.exists() {
        std::fs::write(&source, "interface User { name: string }\nconst u: User = { nam: 1 };\n").expect("write TS source");
    }

    let started = std::time::Instant::now();
    let result = rafikx::lsp::diagnostics_quick(&dir, &source, None).await;
    println!("diagnostics_quick elapsed: {:?}", started.elapsed());
    match result {
        Ok(text) => println!("result: {text}"),
        Err(e) => println!("budget miss (5s 초과 — 자동 진단은 조용히 건너뜀): {e}"),
    }
    if std::env::var("RAFIKX_TS_DIR").is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}
