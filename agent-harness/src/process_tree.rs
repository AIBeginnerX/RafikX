#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::Stdio;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

#[cfg(unix)]
static PROCESS_SCOPE_ID: AtomicU64 = AtomicU64::new(1);
#[cfg(unix)]
const PROCESS_SCOPE_ENV: &str = "RAFIKX_PROCESS_SCOPE";
#[cfg(unix)]
const MAX_SCOPE_SCAN_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct ProcessScope {
    #[cfg(unix)]
    marker: String,
    #[cfg(unix)]
    _marker_file: File,
    #[cfg(unix)]
    marker_path: PathBuf,
}

#[cfg(unix)]
unsafe extern "C" {
    fn fcntl(fd: std::os::raw::c_int, command: std::os::raw::c_int, ...) -> std::os::raw::c_int;
}

#[cfg(unix)]
const F_SETFD: std::os::raw::c_int = 2;

#[cfg(unix)]
fn create_scope_file(marker: &str) -> std::io::Result<(File, PathBuf)> {
    let directory = std::env::temp_dir().join("rafikx-process-scopes");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(marker);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    Ok((file, path))
}

pub(crate) fn spawn_scoped(command: &mut Command) -> std::io::Result<(Child, ProcessScope)> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let id = PROCESS_SCOPE_ID.fetch_add(1, Ordering::Relaxed);
        let marker = format!(
            "{}-{id:020}-{}",
            std::process::id(),
            crate::db::Db::new_id()
        );
        let (marker_file, marker_path) = create_scope_file(&marker)?;
        let marker_fd = marker_file.as_raw_fd();
        command.as_std_mut().process_group(0);
        command.env(PROCESS_SCOPE_ENV, &marker);
        unsafe {
            command.as_std_mut().pre_exec(move || {
                if fcntl(marker_fd, F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let scope = ProcessScope {
            marker,
            _marker_file: marker_file,
            marker_path,
        };
        match command.spawn() {
            Ok(child) => Ok((child, scope)),
            Err(error) => {
                let _ = std::fs::remove_file(&scope.marker_path);
                Err(error)
            }
        }
    }
    #[cfg(not(unix))]
    {
        command.spawn().map(|child| (child, ProcessScope {}))
    }
}

impl Drop for ProcessScope {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.marker_path);
        }
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
async fn scoped_pids(scope: &ProcessScope) -> Vec<u32> {
    let program = if std::path::Path::new("/bin/ps").is_file() {
        "/bin/ps"
    } else {
        "/usr/bin/ps"
    };
    let mut command = Command::new(program);
    command
        .args(["eww", "-axo", "pid=,command="])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return Vec::new();
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill().await;
        return Vec::new();
    };
    let needle = format!("{PROCESS_SCOPE_ENV}={}", scope.marker).into_bytes();
    let scan = async move {
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        let mut total = 0usize;
        let mut matches = std::collections::BTreeSet::new();
        loop {
            line.clear();
            let read = match reader.read_until(b'\n', &mut line).await {
                Ok(read) => read,
                Err(_) => break,
            };
            if read == 0 {
                break;
            }
            total = total.saturating_add(read);
            if total > MAX_SCOPE_SCAN_BYTES {
                break;
            }
            if contains_scope_marker(&line, &needle)
                && let Some(pid) = String::from_utf8_lossy(&line)
                    .split_whitespace()
                    .next()
                    .and_then(|field| field.parse::<u32>().ok())
            {
                matches.insert(pid);
            }
        }
        matches.into_iter().collect::<Vec<_>>()
    };
    let matches = match tokio::time::timeout(Duration::from_secs(2), scan).await {
        Ok(matches) => matches,
        Err(_) => {
            let _ = child.kill().await;
            Vec::new()
        }
    };
    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    matches
}

#[cfg(target_os = "linux")]
fn file_scoped_pids(scope: &ProcessScope) -> Result<Vec<u32>, String> {
    let metadata = scope
        ._marker_file
        .metadata()
        .map_err(|error| format!("프로세스 scope 파일 metadata 실패: {error}"))?;
    let device = metadata.dev();
    let inode = metadata.ino();
    let entries =
        std::fs::read_dir("/proc").map_err(|error| format!("/proc 조회 실패: {error}"))?;
    let mut matches = std::collections::BTreeSet::new();
    let mut scanned = 0usize;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(descriptors) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for descriptor in descriptors.flatten() {
            scanned = scanned.saturating_add(1);
            if scanned > 1_000_000 {
                return Err("프로세스 scope 파일 조회 상한을 초과했습니다".into());
            }
            let Ok(candidate) = descriptor.path().metadata() else {
                continue;
            };
            if candidate.dev() == device && candidate.ino() == inode {
                matches.insert(pid);
                break;
            }
        }
    }
    Ok(matches.into_iter().collect())
}

#[cfg(all(unix, not(target_os = "linux")))]
async fn file_scoped_pids(scope: &ProcessScope) -> Result<Vec<u32>, String> {
    use std::os::unix::process::ExitStatusExt as _;

    let program = ["/usr/sbin/lsof", "/usr/bin/lsof"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
        .ok_or_else(|| "프로세스 scope 확인용 lsof를 찾을 수 없습니다".to_string())?;
    let mut last_failure = String::new();
    for attempt in 0..2 {
        let mut command = Command::new(program);
        command
            .args(["-Fp", "--"])
            .arg(&scope.marker_path)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let output = tokio::time::timeout(Duration::from_secs(3), command.output())
            .await
            .map_err(|_| "프로세스 scope lsof 시간 초과".to_string())?
            .map_err(|error| format!("프로세스 scope lsof 실패: {error}"))?;
        if output.stdout.len() > MAX_SCOPE_SCAN_BYTES {
            return Err("프로세스 scope lsof 출력 상한을 초과했습니다".into());
        }
        if output.status.success() || output.status.code() == Some(1) {
            return Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.strip_prefix('p')?.parse::<u32>().ok())
                .filter(|pid| *pid != std::process::id())
                .collect());
        }
        last_failure = format!(
            "종료 코드 {:?}, 신호 {:?}",
            output.status.code(),
            output.status.signal()
        );
        if attempt == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    Err(format!("프로세스 scope lsof 반복 실패: {last_failure}"))
}

#[cfg(target_os = "linux")]
async fn inherited_scope_pids(scope: &ProcessScope) -> Result<Vec<u32>, String> {
    file_scoped_pids(scope)
}

#[cfg(all(unix, not(target_os = "linux")))]
async fn inherited_scope_pids(scope: &ProcessScope) -> Result<Vec<u32>, String> {
    file_scoped_pids(scope).await
}

#[cfg(unix)]
fn contains_scope_marker(line: &[u8], needle: &[u8]) -> bool {
    line.windows(needle.len())
        .enumerate()
        .any(|(index, window)| {
            let end = index + needle.len();
            window == needle
                && (index == 0 || line[index - 1].is_ascii_whitespace())
                && (end == line.len() || line[end].is_ascii_whitespace())
        })
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

pub(crate) async fn terminate(child: &mut Child, scope: &ProcessScope) -> Result<(), String> {
    #[cfg(not(unix))]
    let _ = scope;
    #[cfg(unix)]
    {
        let root = child.id();
        let program = if std::path::Path::new("/bin/kill").is_file() {
            "/bin/kill"
        } else {
            "/usr/bin/kill"
        };
        if let Some(pid) = root {
            let group = format!("-{pid}");
            run_killer(
                program,
                &["-STOP".to_string(), "--".to_string(), group.clone()],
            )
            .await;
        }
        let mut targets = if let Some(pid) = root {
            descendant_pids(pid).await
        } else {
            Vec::new()
        };
        for pid in inherited_scope_pids(scope).await? {
            if !targets.contains(&pid) {
                targets.push(pid);
            }
        }
        for pid in scoped_pids(scope).await {
            if !targets.contains(&pid) {
                targets.push(pid);
            }
        }
        signal_pids(program, "-STOP", &targets).await;
        for _ in 0..2 {
            let mut discovered = scoped_pids(scope).await;
            discovered.extend(inherited_scope_pids(scope).await?);
            if let Some(pid) = root {
                discovered.extend(descendant_pids(pid).await);
            }
            let mut added = Vec::new();
            for pid in discovered {
                if !targets.contains(&pid) {
                    targets.push(pid);
                    added.push(pid);
                }
            }
            signal_pids(program, "-STOP", &added).await;
            tokio::task::yield_now().await;
        }
        signal_pids(program, "-KILL", &targets).await;
        if let Some(pid) = root {
            let group = format!("-{pid}");
            run_killer(program, &["-KILL".to_string(), "--".to_string(), group]).await;
        }
        for _ in 0..2 {
            let mut remaining = scoped_pids(scope).await;
            remaining.extend(inherited_scope_pids(scope).await?);
            remaining.sort_unstable();
            remaining.dedup();
            if remaining.is_empty() {
                break;
            }
            signal_pids(program, "-KILL", &remaining).await;
        }
    }
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let pid = pid.to_string();
        run_killer(
            "taskkill",
            &["/PID".to_string(), pid, "/T".to_string(), "/F".to_string()],
        )
        .await;
    }
    let _ = child.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    #[cfg(unix)]
    {
        for _ in 0..3 {
            let mut remaining = scoped_pids(scope).await;
            remaining.extend(inherited_scope_pids(scope).await?);
            remaining.sort_unstable();
            remaining.dedup();
            if remaining.is_empty() {
                return Ok(());
            }
            let program = if std::path::Path::new("/bin/kill").is_file() {
                "/bin/kill"
            } else {
                "/usr/bin/kill"
            };
            signal_pids(program, "-KILL", &remaining).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut remaining = scoped_pids(scope).await;
        remaining.extend(inherited_scope_pids(scope).await?);
        remaining.sort_unstable();
        remaining.dedup();
        if !remaining.is_empty() {
            return Err(format!(
                "프로세스 scope 정리 후에도 {}개 자식이 남았습니다",
                remaining.len()
            ));
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn process_scope_matching_requires_a_complete_environment_token() {
        let needle = b"RAFIKX_PROCESS_SCOPE=42-00000000000000000001";
        assert!(contains_scope_marker(
            b"123 command RAFIKX_PROCESS_SCOPE=42-00000000000000000001 OTHER=value\n",
            needle
        ));
        assert!(!contains_scope_marker(
            b"123 command RAFIKX_PROCESS_SCOPE=42-000000000000000000010 OTHER=value\n",
            needle
        ));
    }

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
        let (mut child, scope) = spawn_scoped(&mut command).expect("spawn process tree");
        tokio::time::sleep(Duration::from_millis(50)).await;
        terminate(&mut child, &scope).await.expect("terminate tree");
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
            "\"$1\" -c 'import os,sys,time\nchild=os.fork()\nif child == 0:\n os.setsid()\n if os.fork() > 0: os._exit(0)\n time.sleep(1)\n open(sys.argv[1], \"w\").write(\"escaped\")\nelse:\n time.sleep(5)' \"$2\"",
            "rafikx-process-session",
            python,
            marker.to_str().expect("marker path"),
        ]);
        let (mut child, scope) = spawn_scoped(&mut command).expect("spawn detached process");
        tokio::time::sleep(Duration::from_millis(150)).await;
        terminate(&mut child, &scope)
            .await
            .expect("terminate detached tree");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn inherited_scope_survives_environment_clearing_and_reparenting() {
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file());
        let Some(python) = python else { return };
        let root = std::env::temp_dir().join(format!(
            "rafikx-process-clearenv-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let marker = root.join("escaped");
        let script = "import os,sys,time\nos.environ.clear()\nchild=os.fork()\nif child == 0:\n os.setsid()\n if os.fork() > 0: os._exit(0)\n for fd in (0,1,2):\n  try: os.close(fd)\n  except OSError: pass\n time.sleep(1)\n open(sys.argv[1], 'w').write('escaped')\nos._exit(0)";
        let mut command = Command::new("env");
        command.args([
            "-u",
            PROCESS_SCOPE_ENV,
            python,
            "-c",
            script,
            marker.to_str().expect("marker path"),
        ]);
        let (mut child, scope) = spawn_scoped(&mut command).expect("spawn clearenv process");
        tokio::time::sleep(Duration::from_millis(150)).await;
        terminate(&mut child, &scope)
            .await
            .expect("terminate clearenv tree");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
