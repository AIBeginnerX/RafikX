use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

pub(crate) fn isolate(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.as_std_mut().process_group(0);
    }
}

async fn run_killer(program: &str, args: &[&str]) {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = tokio::time::timeout(Duration::from_secs(2), command.status()).await;
}

pub(crate) async fn terminate(child: &mut Child) {
    if let Some(pid) = child.id() {
        #[cfg(unix)]
        {
            let group = format!("-{pid}");
            let program = if std::path::Path::new("/bin/kill").is_file() {
                "/bin/kill"
            } else {
                "/usr/bin/kill"
            };
            run_killer(program, &["-KILL", "--", &group]).await;
        }
        #[cfg(windows)]
        {
            let pid = pid.to_string();
            run_killer("taskkill", &["/PID", &pid, "/T", "/F"]).await;
        }
    }
    let _ = child.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn terminate_kills_descendants() {
        let root =
            std::env::temp_dir().join(format!("rafikx-process-tree-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&root).expect("test directory");
        let marker = root.join("survived");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "(sleep 1; printf survived > \"$1\") & wait",
            "rafikx-process-tree",
            marker.to_str().expect("marker path"),
        ]);
        isolate(&mut command);
        let mut child = command.spawn().expect("spawn process tree");
        tokio::time::sleep(Duration::from_millis(50)).await;
        terminate(&mut child).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
