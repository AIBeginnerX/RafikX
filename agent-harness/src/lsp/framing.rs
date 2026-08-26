use anyhow::{Result, anyhow};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

pub async fn write_message<W>(writer: &mut W, value: &serde_json::Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value)?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(anyhow!("LSP message exceeds {MAX_MESSAGE_BYTES} bytes"));
    }
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_message<R>(reader: &mut R) -> Result<serde_json::Value>
where
    R: AsyncBufRead + Unpin,
{
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return Err(anyhow!("LSP server closed stdout"));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(anyhow!("invalid LSP header"));
        };
        if name.eq_ignore_ascii_case("Content-Length") {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = content_length.ok_or_else(|| anyhow!("LSP Content-Length is missing"))?;
    if length > MAX_MESSAGE_BYTES {
        return Err(anyhow!("LSP message exceeds {MAX_MESSAGE_BYTES} bytes"));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn lsp_frame_roundtrip() {
        let (mut writer, reader) = tokio::io::duplex(512);
        let expected = json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        let write = tokio::spawn({
            let expected = expected.clone();
            async move { write_message(&mut writer, &expected).await }
        });
        let actual = read_message(&mut BufReader::new(reader))
            .await
            .expect("read frame");
        write.await.expect("writer task").expect("write frame");
        assert_eq!(actual, expected);
    }
}
