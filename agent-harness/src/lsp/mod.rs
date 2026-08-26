mod framing;
mod position;
mod process;
mod protocol;

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::json;

use crate::run::RunContext;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_DOCUMENT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RESULT_CHARS: usize = 8_000;

pub async fn diagnostics(
    workspace: &Path,
    path: &Path,
    run: Option<&RunContext>,
) -> Result<String> {
    let result = run_query(workspace, path, run, Query::Diagnostics).await?;
    let output = bounded(protocol::render_diagnostics(&result));
    record_source(run, path, "diagnostics", &output);
    Ok(output)
}

pub async fn definition(
    workspace: &Path,
    path: &Path,
    line: u32,
    column: u32,
    run: Option<&RunContext>,
) -> Result<String> {
    let result = run_query(workspace, path, run, Query::Definition { line, column }).await?;
    let output = bounded(protocol::render_locations(&result));
    record_source(run, path, "definition", &output);
    Ok(output)
}

enum Query {
    Diagnostics,
    Definition { line: u32, column: u32 },
}

async fn run_query(
    workspace: &Path,
    path: &Path,
    run: Option<&RunContext>,
    query: Query,
) -> Result<serde_json::Value> {
    let metadata = tokio::fs::metadata(path).await?;
    if metadata.len() > MAX_DOCUMENT_BYTES {
        return Err(anyhow!("LSP document exceeds {MAX_DOCUMENT_BYTES} bytes"));
    }
    let text = tokio::fs::read_to_string(path).await?;
    let spec = process::server_for(path)?;
    let future = async {
        let mut server = process::LspProcess::start(&spec, workspace).await?;
        server.open(path, spec.language_id, &text).await?;
        if spec.wait_for_status {
            server.wait_until_ready().await?;
        }
        let result = match query {
            Query::Diagnostics => {
                server
                    .request(
                        "textDocument/diagnostic",
                        json!({"textDocument":{"uri":protocol::file_uri(path)}}),
                    )
                    .await
            }
            Query::Definition { line, column } => {
                let position = position::position(&text, line, column)?;
                server
                    .request(
                        "textDocument/definition",
                        json!({
                            "textDocument":{"uri":protocol::file_uri(path)},
                            "position":position
                        }),
                    )
                    .await
            }
        };
        server.shutdown().await;
        result
    };
    if let Some(run) = run {
        tokio::select! {
            result = tokio::time::timeout(REQUEST_TIMEOUT, future) => timeout_result(result),
            _ = run.cancelled_reason() => Err(anyhow!("LSP request cancelled")),
        }
    } else {
        timeout_result(tokio::time::timeout(REQUEST_TIMEOUT, future).await)
    }
}

fn timeout_result(
    result: std::result::Result<Result<serde_json::Value>, tokio::time::error::Elapsed>,
) -> Result<serde_json::Value> {
    result.map_err(|_| {
        anyhow!(
            "LSP request timed out after {} seconds",
            REQUEST_TIMEOUT.as_secs()
        )
    })?
}

fn bounded(value: String) -> String {
    if value.chars().count() <= MAX_RESULT_CHARS {
        value
    } else {
        let prefix = value.chars().take(MAX_RESULT_CHARS).collect::<String>();
        format!("{prefix}\n[LSP result truncated]")
    }
}

fn record_source(run: Option<&RunContext>, path: &Path, operation: &str, output: &str) {
    if let Some(run) = run {
        run.record_context_source(
            crate::run::ContextSourceKind::Lsp,
            format!("{operation}:{}", path.display()),
            (MAX_RESULT_CHARS / 4) as u32,
            crate::context::tokens(output),
        );
    }
}
