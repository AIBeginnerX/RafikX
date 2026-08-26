use serde::Deserialize;
use serde_json::{Value, json};

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const PROTOCOL_VERSION: &str = "1";

#[derive(Debug, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
            data: None,
        }
    }

    pub fn not_initialized() -> Self {
        Self {
            code: -32002,
            message: "rafikx.initialize must be called first".into(),
            data: None,
        }
    }

    pub fn server(message: impl Into<String>) -> Self {
        Self {
            code: -32000,
            message: message.into(),
            data: None,
        }
    }
}

pub fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

pub fn error(id: Value, error: RpcError) -> Value {
    let mut body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": error.code, "message": error.message}
    });
    if let Some(data) = error.data {
        body["error"]["data"] = data;
    }
    body
}

pub fn protocol_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    error(
        id,
        RpcError {
            code,
            message: message.into(),
            data: None,
        },
    )
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

pub fn parse(frame: &[u8]) -> Result<Request, Value> {
    let value: Value = serde_json::from_slice(frame)
        .map_err(|error| protocol_error(Value::Null, -32700, error.to_string()))?;
    let has_id = value
        .as_object()
        .is_some_and(|object| object.contains_key("id"));
    let mut request: Request = serde_json::from_value(value)
        .map_err(|error| protocol_error(Value::Null, -32600, error.to_string()))?;
    if has_id && request.id.is_none() {
        request.id = Some(Value::Null);
    }
    if request.jsonrpc != "2.0" || request.method.trim().is_empty() {
        return Err(protocol_error(
            request.id.unwrap_or(Value::Null),
            -32600,
            "invalid JSON-RPC 2.0 request",
        ));
    }
    Ok(request)
}
