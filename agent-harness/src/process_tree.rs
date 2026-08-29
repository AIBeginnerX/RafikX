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

async fn run_killer(program: &str, args: &[String]) {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = tokio::time::timeout(Duration::from_secs(2), command.status()).await;
}

#[cfg(unix)]
async fn descendant_pids(root: u32) -> Vec<u32> {
    let program = if std::path::Path::new("/bin/ps").is_file() {
        "/bin/ps"
    } else {
        "/usr/bin/ps"
    };
    let mut command = Command::new(program);
    command
        .args(["-axo", "pid=,ppid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(2), command.output()).await
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let relations = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((
                fields.next()?.parse::<u32>().ok()?,
                fields.next()?.parse::<u32>().ok()?,
            ))
        })
        .collect::<Vec<_>>();
    let mut descendants = std::collections::BTreeSet::new();
    let mut pending = vec![root];
    while let Some(parent) = pending.pop() {
        for &(pid, ppid) in &relations {
            if ppid == parent && descendants.insert(pid) {
                pending.push(pid);
            }
        }
    }
    descendants.into_iter().collect()
}

#[cfg(unix)]
async fn signal_pids(program: &str, signal: &str, pids: &[u32]) {
    if pids.is_empty() {
        return;
    }
    let mut args = Vec::with_capacity(pids.len() + 1);
    args.push(signal.to_string());
    args.extend(pids.iter().map(u32::to_string));
    run_killer(program, &args).await;
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
            run_killer(
                program,
                &["-STOP".to_string(), "--".to_string(), group.clone()],
            )
            .await;
            let mut descendants = descendant_pids(pid).await;
            signal_pids(program, "-STOP", &descendants).await;
            for descendant in descendant_pids(pid).await {
                if !descendants.contains(&descendant) {
                    descendants.push(descendant);
                }
            }
            signal_pids(program, "-KILL", &descendants).await;
            run_killer(program, &["-KILL".to_string(), "--".to_string(), group]).await;
        }
        #[cfg(windows)]
        {
            let pid = pid.to_string();
            run_killer(
                "taskkill",
                &["/PID".to_string(), pid, "/T".to_string(), "/F".to_string()],
            )
            .await;
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

    #[tokio::test]
    async fn terminate_kills_session_detached_descendants() {
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file());
        let Some(python) = python else { return };
        let root = std::env::temp_dir().join(format!(
            "rafikx-process-session-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let marker = root.join("escaped");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "\"$1\" -c 'import os,sys,time; os.setsid(); time.sleep(1); open(sys.argv[1], \"w\").write(\"escaped\")' \"$2\" & wait",
            "rafikx-process-session",
            python,
            marker.to_str().expect("marker path"),
        ]);
        isolate(&mut command);
        let mut child = command.spawn().expect("spawn detached process");
        tokio::time::sleep(Duration::from_millis(150)).await;
        terminate(&mut child).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
