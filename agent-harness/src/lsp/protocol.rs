use std::path::Path;

use serde_json::{Value, json};

pub fn initialize(workspace: &Path, process_id: u32) -> Value {
    json!({
        "processId": process_id,
        "rootPath": workspace.to_string_lossy(),
        "rootUri": file_uri(workspace),
        // pyright 등은 workspaceFolders 없으면 "<default workspace root>" 로 떨어져
        // 진단이 빈 배열로 나온다 (실측).
        "workspaceFolders": [{"uri": file_uri(workspace), "name": workspace.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "workspace".into())}],
        "capabilities": {
            "experimental": {"serverStatusNotification": true},
            "textDocument": {
                "diagnostic": {"dynamicRegistration": false},
                "definition": {"dynamicRegistration": false}
            },
            "workspace": {"configuration": false}
        },
        "clientInfo": {"name":"RafikX","version":env!("CARGO_PKG_VERSION")}
    })
}

pub fn text_document(path: &Path, language_id: &str, text: &str) -> Value {
    json!({
        "textDocument": {
            "uri": file_uri(path),
            "languageId": language_id,
            "version": 1,
            "text": text
        }
    })
}

pub fn file_uri(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let raw = path.to_string_lossy().replace('\\', "/");
    let encoded = raw
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect::<String>();
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

pub fn render_diagnostics(result: &Value) -> String {
    let items = result
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| result.as_array())
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        return "LSP diagnostics: clean".into();
    }
    items
        .iter()
        .map(|item| {
            let start = &item["range"]["start"];
            let line = start["line"].as_u64().unwrap_or(0) + 1;
            let column = start["character"].as_u64().unwrap_or(0) + 1;
            let severity = match item["severity"].as_u64() {
                Some(1) => "error",
                Some(2) => "warning",
                Some(3) => "info",
                Some(4) => "hint",
                _ => "diagnostic",
            };
            let message = item["message"].as_str().unwrap_or("unknown diagnostic");
            format!("{severity} {line}:{column} {message}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_locations(result: &Value, label: &str) -> String {
    let locations = result
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![result.clone()]);
    let rendered = locations
        .iter()
        .filter_map(|location| {
            let uri = location
                .get("uri")
                .or_else(|| location.get("targetUri"))?
                .as_str()?;
            let range = location
                .get("range")
                .or_else(|| location.get("targetSelectionRange"))?;
            let line = range["start"]["line"].as_u64().unwrap_or(0) + 1;
            let column = range["start"]["character"].as_u64().unwrap_or(0) + 1;
            Some(format!("{uri}:{line}:{column}"))
        })
        .collect::<Vec<_>>();
    if rendered.is_empty() {
        format!("LSP {label}: not found")
    } else {
        rendered.join("\n")
    }
}

/// textDocument/hover 결과 — contents 가 문자열·배열·MarkupContent 등 여러 모양이라
/// 전부 문자열로 평탄화한다. 비어 있으면 not found.
pub fn render_hover(result: &Value) -> String {
    let default = Value::Null;
    let contents = result.get("contents").unwrap_or(&default);
    let rendered: Vec<String> = match contents {
        Value::String(_) => vec![hover_piece(contents)],
        Value::Array(items) => items.iter().map(hover_piece).collect(),
        Value::Object(_) => vec![hover_piece(contents)],
        _ => Vec::new(),
    };
    let rendered: Vec<String> = rendered
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect();
    if rendered.is_empty() {
        "LSP hover: not found".into()
    } else {
        rendered.join("\n")
    }
}

fn hover_piece(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(map) => map
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initialize_advertises_rust_analyzer_readiness_status() {
        let params = initialize(Path::new("."), 42);
        assert_eq!(
            params["capabilities"]["experimental"]["serverStatusNotification"],
            true
        );
    }

    #[test]
    fn hover_flattens_all_contents_shapes() {
        // MarkedString 문자열
        assert_eq!(render_hover(&json!({"contents":"fn foo()"})), "fn foo()");
        // MarkupContent
        assert_eq!(
            render_hover(&json!({"contents":{"kind":"markdown","value":"**docs**"}})),
            "**docs**"
        );
        // MarkedString 배열
        assert_eq!(
            render_hover(&json!({"contents":["```rust\nu32\n```","설명"]})),
            "```rust\nu32\n```\n설명"
        );
        assert_eq!(render_hover(&json!({})), "LSP hover: not found");
        assert_eq!(render_locations(&json!([]), "references"), "LSP references: not found");
    }
}
