use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};
use tokio::sync::mpsc;

use super::protocol::MAX_FRAME_BYTES;

pub type Outbound = mpsc::Sender<Value>;

pub async fn run<W>(writer: W, mut messages: mpsc::Receiver<Value>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    while let Some(mut value) = messages.recv().await {
        let mut encoded = serde_json::to_vec(&value)?;
        if encoded.len() > MAX_FRAME_BYTES {
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            value = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32603, "message": "response exceeds 1 MiB"}
            });
            encoded = serde_json::to_vec(&value)?;
        }
        writer.write_all(&encoded).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
    Ok(())
}
