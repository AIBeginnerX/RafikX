use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::{Tool, ToolCtx, resolve_tool_path};

pub struct LspDiagnostics;
pub struct LspDefinition;

impl Tool for LspDiagnostics {
    fn name(&self) -> &'static str {
        "lsp_diagnostics"
    }

    fn description(&self) -> &'static str {
        "언어 서버의 실제 진단을 읽습니다. Rust, Python, TypeScript/JavaScript, Go를 지원하며 서버를 자동 설치하지 않습니다."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type":"object",
            "properties":{"path":{"type":"string"}},
            "required":["path"]
        })
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path 인자가 필요합니다"))?;
        let resolved = resolve_tool_path(ctx, path)?;
        run_async(crate::lsp::diagnostics(
            &ctx.workspace,
            &resolved,
            ctx.run.as_ref(),
        ))
    }
}

impl Tool for LspDefinition {
    fn name(&self) -> &'static str {
        "lsp_definition"
    }

    fn description(&self) -> &'static str {
        "언어 서버로 심볼 정의 위치를 찾습니다. line과 column은 1부터 시작합니다."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type":"object",
            "properties":{
                "path":{"type":"string"},
                "line":{"type":"integer","minimum":1},
                "column":{"type":"integer","minimum":1}
            },
            "required":["path","line","column"]
        })
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path 인자가 필요합니다"))?;
        let line = input
            .get("line")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| anyhow!("line은 1 이상의 정수여야 합니다"))?;
        let column = input
            .get("column")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| anyhow!("column은 1 이상의 정수여야 합니다"))?;
        let resolved = resolve_tool_path(ctx, path)?;
        run_async(crate::lsp::definition(
            &ctx.workspace,
            &resolved,
            line,
            column,
            ctx.run.as_ref(),
        ))
    }
}

fn run_async<F>(future: F) -> Result<String>
where
    F: std::future::Future<Output = Result<String>>,
{
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| anyhow!("LSP tool requires the RafikX async runtime"))?;
    tokio::task::block_in_place(|| runtime.block_on(future))
}
