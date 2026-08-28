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
/// 편집 후 자동 진단 예산 — 느린 서버 콜드 스타트를 기다리지 않는다.
const AUTO_DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_DOCUMENT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RESULT_CHARS: usize = 8_000;

pub async fn diagnostics(
    workspace: &Path,
    path: &Path,
    run: Option<&RunContext>,
) -> Result<String> {
    diagnostics_within(workspace, path, run, REQUEST_TIMEOUT).await
}

/// 편집 직후 자동 진단용 — 예산 안에 서버가 답하지 않으면 Err 로 포기한다.
pub async fn diagnostics_quick(
    workspace: &Path,
    path: &Path,
    run: Option<&RunContext>,
) -> Result<String> {
    diagnostics_within(workspace, path, run, AUTO_DIAGNOSTICS_TIMEOUT).await
}

async fn diagnostics_within(
    workspace: &Path,
    path: &Path,
    run: Option<&RunContext>,
    timeout: Duration,
) -> Result<String> {
    let result = run_query(workspace, path, run, Query::Diagnostics, timeout).await?;
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
    let result = run_query(
        workspace,
        path,
        run,
        Query::Definition { line, column },
        REQUEST_TIMEOUT,
    )
    .await?;
    let output = bounded(protocol::render_locations(&result, "definition"));
    record_source(run, path, "definition", &output);
    Ok(output)
}

pub async fn hover(
    workspace: &Path,
    path: &Path,
    line: u32,
    column: u32,
    run: Option<&RunContext>,
) -> Result<String> {
    let result = run_query(
        workspace,
        path,
        run,
        Query::Hover { line, column },
        REQUEST_TIMEOUT,
    )
    .await?;
    let output = bounded(protocol::render_hover(&result));
    record_source(run, path, "hover", &output);
    Ok(output)
}

pub async fn references(
    workspace: &Path,
    path: &Path,
    line: u32,
    column: u32,
    run: Option<&RunContext>,
) -> Result<String> {
    let result = run_query(
        workspace,
        path,
        run,
        Query::References { line, column },
        REQUEST_TIMEOUT,
    )
    .await?;
    let output = bounded(protocol::render_locations(&result, "references"));
    record_source(run, path, "references", &output);
    Ok(output)
}

/// 이 확장자를 다룰 언어 서버가 설정돼 있는지 — 실제 설치 여부는 스폰 시점에 판명난다.
pub fn has_server(path: &Path) -> bool {
    process::server_for(path).is_ok()
}

enum Query {
    Diagnostics,
    Definition { line: u32, column: u32 },
    Hover { line: u32, column: u32 },
    References { line: u32, column: u32 },
}

async fn run_query(
    workspace: &Path,
    path: &Path,
    run: Option<&RunContext>,
    query: Query,
    timeout: Duration,
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
                // pull(3.17) 우선 — 미지원 서버(-32601)는 push(publishDiagnostics)로 받는다.
                let uri = protocol::file_uri(path);
                match server
                    .request(
                        "textDocument/diagnostic",
                        json!({"textDocument":{"uri":uri}}),
                    )
                    .await
                {
                    Ok(v) => Ok(v),
                    Err(e) if is_method_missing(&e) => {
                        server.collect_diagnostics(&uri, 2000).await
                    }
                    Err(e) => Err(e),
                }
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
            Query::Hover { line, column } => {
                let position = position::position(&text, line, column)?;
                server
                    .request(
                        "textDocument/hover",
                        json!({
                            "textDocument":{"uri":protocol::file_uri(path)},
                            "position":position
                        }),
                    )
                    .await
            }
            Query::References { line, column } => {
                let position = position::position(&text, line, column)?;
                server
                    .request(
                        "textDocument/references",
                        json!({
                            "textDocument":{"uri":protocol::file_uri(path)},
                            "position":position,
                            "context":{"includeDeclaration":false}
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
            result = tokio::time::timeout(timeout, future) => timeout_result(result, timeout),
            _ = run.cancelled_reason() => Err(anyhow!("LSP request cancelled")),
        }
    } else {
        timeout_result(tokio::time::timeout(timeout, future).await, timeout)
    }
}

fn timeout_result(
    result: std::result::Result<Result<serde_json::Value>, tokio::time::error::Elapsed>,
    timeout: Duration,
) -> Result<serde_json::Value> {
    result.map_err(|_| {
        anyhow!(
            "LSP request timed out after {} seconds",
            timeout.as_secs()
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

/// LSP -32601(Method not found)·"not supported" 계열 오류인가.
fn is_method_missing(error: &anyhow::Error) -> bool {
    let text = format!("{error}");
    text.contains("-32601") || text.contains("not supported") || text.contains("Method not found")
}

#[cfg(test)]
mod tests {
    #[test]
    fn detects_method_missing_errors() {
        assert!(super::is_method_missing(&anyhow::anyhow!("LSP textDocument/diagnostic failed: {{\"code\":-32601}}")));
        assert!(super::is_method_missing(&anyhow::anyhow!("method is not supported")));
        assert!(!super::is_method_missing(&anyhow::anyhow!("connection reset")));
    }
}
