mod approval;
mod observer;
mod params;
mod protocol;
mod server;
mod service;
mod state;
mod writer;

pub use protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION};
pub use server::{run, stdio};

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    #[tokio::test(flavor = "multi_thread")]
    async fn rpc_requires_initialize_then_opens_and_reads_session() {
        let (mut client_in, server_in) = duplex(16 * 1024);
        let (server_out, mut client_out) = duplex(64 * 1024);
        let server = tokio::spawn(super::run(server_in, server_out));

        client_in
            .write_all(
                concat!(
                    "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session.open\"}\n",
                    "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"rafikx.initialize\",\"params\":{\"protocol_version\":\"1\"}}\n",
                    "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.open\",\"params\":{}}\n"
                )
                .as_bytes(),
            )
            .await
            .expect("write requests");
        client_in.shutdown().await.expect("close input");

        let mut output = String::new();
        client_out
            .read_to_string(&mut output)
            .await
            .expect("read responses");
        server.await.expect("server task").expect("rpc server");
        let messages: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).expect("json response"))
            .collect();
        let response = |id| {
            messages
                .iter()
                .find(|message| message["id"] == id)
                .expect("response id")
        };
        assert_eq!(response(1)["error"]["code"], -32002);
        assert_eq!(response(2)["result"]["protocol_version"], "1");
        assert!(
            response(3)["result"]["session_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("draft-"))
        );
    }

    #[test]
    fn rpc_parser_preserves_null_id_and_rejects_wrong_version() {
        let request =
            super::protocol::parse(br#"{"jsonrpc":"2.0","id":null,"method":"rafikx.initialize"}"#)
                .expect("request");
        assert_eq!(request.id, Some(Value::Null));
        assert!(super::protocol::parse(br#"{"jsonrpc":"1.0","id":1,"method":"x"}"#).is_err());
    }
}
