use std::io;
use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use super::protocol::{self, MAX_FRAME_BYTES};
use super::service::Service;

const MAX_CONCURRENT_REQUESTS: usize = 8;
const WRITER_CAPACITY: usize = 256;

enum Frame {
    Data(Vec<u8>),
    TooLarge,
    Eof,
}

pub async fn stdio() -> Result<()> {
    crate::ui::set_live(Some(Arc::new(|_| {})));
    let result = run(tokio::io::stdin(), tokio::io::stdout()).await;
    crate::ui::set_live(None);
    result
}

pub async fn run<R, W>(reader: R, writer: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (outbound, receiver) = mpsc::channel(WRITER_CAPACITY);
    let writer_task = tokio::spawn(super::writer::run(writer, receiver));
    let service = Service::new(outbound.clone());
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));
    let mut tasks = JoinSet::new();
    let mut reader = BufReader::new(reader);

    loop {
        let frame = read_frame(&mut reader).await?;
        let request = match frame {
            Frame::Eof => break,
            Frame::TooLarge => {
                outbound
                    .send(protocol::protocol_error(
                        Value::Null,
                        -32600,
                        "request exceeds 1 MiB",
                    ))
                    .await?;
                continue;
            }
            Frame::Data(frame) => match protocol::parse(&frame) {
                Ok(request) => request,
                Err(error) => {
                    outbound.send(error).await?;
                    continue;
                }
            },
        };

        if request.method == "rafikx.initialize" {
            respond(&service, &outbound, request).await?;
            continue;
        }
        if !service.is_initialized() {
            respond(&service, &outbound, request).await?;
            continue;
        }

        let permit = semaphore.clone().acquire_owned().await?;
        let service = service.clone();
        let outbound = outbound.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let _ = respond(&service, &outbound, request).await;
        });
    }

    service.state().cancel_all("rpc input closed");
    while tasks.join_next().await.is_some() {}
    drop(service);
    drop(outbound);
    writer_task.await??;
    Ok(())
}

async fn respond(
    service: &Service,
    outbound: &mpsc::Sender<Value>,
    request: protocol::Request,
) -> Result<()> {
    let id = request.id;
    let result = service.handle(&request.method, request.params).await;
    let Some(id) = id else {
        return Ok(());
    };
    let response = match result {
        Ok(result) => protocol::success(id, result),
        Err(error) => protocol::error(id, error),
    };
    outbound.send(response).await?;
    Ok(())
}

async fn read_frame<R>(reader: &mut R) -> io::Result<Frame>
where
    R: AsyncBufRead + Unpin,
{
    let mut frame = Vec::new();
    let mut overflow = false;
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return if frame.is_empty() && !overflow {
                Ok(Frame::Eof)
            } else if overflow {
                Ok(Frame::TooLarge)
            } else {
                Ok(Frame::Data(frame))
            };
        }
        if let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
            append_bounded(&mut frame, &buffer[..index], &mut overflow);
            reader.consume(index + 1);
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return if overflow {
                Ok(Frame::TooLarge)
            } else {
                Ok(Frame::Data(frame))
            };
        }
        let length = buffer.len();
        append_bounded(&mut frame, buffer, &mut overflow);
        reader.consume(length);
    }
}

fn append_bounded(target: &mut Vec<u8>, bytes: &[u8], overflow: &mut bool) {
    if *overflow || target.len().saturating_add(bytes.len()) > MAX_FRAME_BYTES {
        *overflow = true;
        return;
    }
    target.extend_from_slice(bytes);
}
