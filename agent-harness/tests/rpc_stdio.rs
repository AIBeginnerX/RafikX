use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

#[test]
fn rpc_stdout_stays_json_when_cli_login_is_auto_imported() {
    let root = std::env::temp_dir().join(format!("rafikx-rpc-stdio-{}", rafikx::db::Db::new_id()));
    let home = root.join("home");
    let rafikx_home = root.join("rafikx");
    let claude = home.join(".claude");
    fs::create_dir_all(&claude).expect("create fake CLI home");
    fs::write(
        claude.join(".credentials.json"),
        serde_json::to_vec(&json!({
            "claudeAiOauth": {
                "accessToken": "qa-access-token",
                "refreshToken": "qa-refresh-token",
                "expiresAt": 4_102_444_800_000_i64
            }
        }))
        .expect("serialize fake credentials"),
    )
    .expect("write fake credentials");

    let mut child = Command::new(env!("CARGO_BIN_EXE_rafikx"))
        .arg("rpc")
        .env("HOME", &home)
        .env("RAFIKX_HOME", &rafikx_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn RPC process");
    child
        .stdin
        .as_mut()
        .expect("RPC stdin")
        .write_all(
            concat!(
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"rafikx.initialize\",\"params\":{\"protocol_version\":\"1\"}}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session.open\",\"params\":{}}\n"
            )
            .as_bytes(),
        )
        .expect("write RPC requests");
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for RPC process");
    let _ = fs::remove_dir_all(&root);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 RPC output");
    let responses: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("every stdout line is JSON"))
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"]["protocol_version"], "1");
    assert!(
        responses[1]["result"]["session_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("draft-"))
    );
    assert!(!stdout.contains("qa-access-token"));
}
