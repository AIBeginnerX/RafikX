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

#[cfg(windows)]
use windows_job::WindowsJob;

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
    #[cfg(windows)]
    job: WindowsJob,
    #[cfg(test)]
    force_discovery_failure: bool,
}

#[cfg(unix)]
// SAFETY: `fcntl` is declared with the POSIX ABI and the only call below passes an owned,
// open descriptor plus the integer-only F_SETFD command. The call runs before exec and does
// not retain pointers or Rust references.
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
        // SAFETY: `pre_exec` runs after fork. Its closure performs only the async-signal-safe
        // `fcntl(F_SETFD)` syscall on the still-open marker descriptor and returns errno by value.
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
            #[cfg(test)]
            force_discovery_failure: false,
        };
        match command.spawn() {
            Ok(child) => Ok((child, scope)),
            Err(error) => {
                let _ = std::fs::remove_file(&scope.marker_path);
                Err(error)
            }
        }
    }
    #[cfg(windows)]
    {
        let job = WindowsJob::create()?;
        WindowsJob::configure_suspended(command);
        let mut child = command.spawn()?;
        if let Err(error) = job.assign_and_resume(&child) {
            let _ = child.start_kill();
            return Err(error);
        }
        Ok((
            child,
            ProcessScope {
                job,
                #[cfg(test)]
                force_discovery_failure: false,
            },
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        command.spawn().map(|child| {
            (
                child,
                ProcessScope {
                    #[cfg(test)]
                    force_discovery_failure: false,
                },
            )
        })
    }
}

#[cfg(windows)]
mod windows_job {
    use std::ffi::c_void;
    use std::io;
    use std::mem::size_of;
    use std::os::windows::process::CommandExt as _;

    use tokio::process::{Child, Command};

    type Handle = *mut c_void;

    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
    const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;
    const THREAD_SUSPEND_RESUME: u32 = 0x0000_0002;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimitInformation {
        basic_limit_information: BasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ThreadEntry32 {
        size: u32,
        usage: u32,
        thread_id: u32,
        owner_process_id: u32,
        base_priority: i32,
        delta_priority: i32,
        flags: u32,
    }

    #[link(name = "kernel32")]
    // SAFETY: These signatures and constants mirror the stable Win32 Job Object and Toolhelp
    // APIs. Handles are checked for null/INVALID_HANDLE_VALUE and closed exactly once below.
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            class: i32,
            information: *const c_void,
            length: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
        fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry32) -> i32;
        fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry32) -> i32;
        fn OpenThread(access: u32, inherit: i32, thread_id: u32) -> Handle;
        fn ResumeThread(thread: Handle) -> u32;
    }

    #[derive(Debug)]
    pub(super) struct WindowsJob {
        handle: isize,
    }

    impl WindowsJob {
        pub(super) fn create() -> io::Result<Self> {
            // SAFETY: Null security attributes and name request an unnamed job owned by this
            // process. The returned handle is validated and transferred into `WindowsJob`.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut limits = ExtendedLimitInformation::default();
            limits.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` has the documented repr(C) layout and remains alive for the call.
            let configured = unsafe {
                SetInformationJobObject(
                    handle,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    (&raw const limits).cast(),
                    size_of::<ExtendedLimitInformation>() as u32,
                )
            };
            if configured == 0 {
                let error = io::Error::last_os_error();
                // SAFETY: `handle` is a valid, uniquely owned job handle on this branch.
                unsafe { CloseHandle(handle) };
                return Err(error);
            }
            Ok(Self {
                handle: handle as isize,
            })
        }

        pub(super) fn configure_suspended(command: &mut Command) {
            command.as_std_mut().creation_flags(CREATE_SUSPENDED);
        }

        pub(super) fn assign_and_resume(&self, child: &Child) -> io::Result<()> {
            let process_id = child
                .id()
                .ok_or_else(|| io::Error::other("spawned process has no process id"))?;
            let process = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("spawned process has no process handle"))?
                as Handle;
            let thread = primary_thread(process_id)?;
            // SAFETY: Both handles are live. The child is still suspended, so it cannot create
            // an unassigned descendant between assignment and resume.
            let assigned = unsafe { AssignProcessToJobObject(self.raw(), process) };
            if assigned == 0 {
                let error = io::Error::last_os_error();
                // SAFETY: `thread` was opened by `primary_thread` and is uniquely owned here.
                unsafe { CloseHandle(thread) };
                return Err(error);
            }
            // SAFETY: `thread` identifies the suspended primary thread of `child`.
            let resumed = unsafe { ResumeThread(thread) };
            let resume_error = (resumed == u32::MAX).then(io::Error::last_os_error);
            // SAFETY: `thread` was opened by `primary_thread` and is uniquely owned here.
            unsafe { CloseHandle(thread) };
            if let Some(error) = resume_error {
                return Err(error);
            }
            Ok(())
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            // SAFETY: `self.raw()` remains owned by this value for the duration of the call.
            if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        fn raw(&self) -> Handle {
            self.handle as Handle
        }
    }

    impl Drop for WindowsJob {
        fn drop(&mut self) {
            // SAFETY: The handle was created once and is closed only from this Drop. The
            // kill-on-close limit makes this a final fail-safe for every associated descendant.
            unsafe { CloseHandle(self.raw()) };
        }
    }

    fn primary_thread(process_id: u32) -> io::Result<Handle> {
        // SAFETY: The snapshot call takes integers only and returns an owned handle.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot as isize == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry = ThreadEntry32 {
            size: size_of::<ThreadEntry32>() as u32,
            ..ThreadEntry32::default()
        };
        let mut found = std::ptr::null_mut();
        // SAFETY: `entry` is correctly sized and writable for the duration of enumeration.
        let mut more = unsafe { Thread32First(snapshot, &raw mut entry) } != 0;
        while more {
            if entry.owner_process_id == process_id {
                // SAFETY: `entry.thread_id` came from the current Toolhelp snapshot.
                found = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.thread_id) };
                break;
            }
            // SAFETY: `entry` remains correctly sized and writable.
            more = unsafe { Thread32Next(snapshot, &raw mut entry) } != 0;
        }
        // SAFETY: `snapshot` is a valid, uniquely owned snapshot handle.
        unsafe { CloseHandle(snapshot) };
        if found.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(found)
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

async fn run_killer(program: &str, args: &[String]) -> Result<(), String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(2), command.status())
        .await
        .map_err(|_| format!("{program} 시간 초과"))?
        .map_err(|error| format!("{program} 실행 실패: {error}"))?;
    if !status.success() {
        return Err(format!(
            "{program} 실패 (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
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
    #[cfg(test)]
    if scope.force_discovery_failure {
        return Err("강제된 프로세스 scope 조회 실패".into());
    }
    file_scoped_pids(scope)
}

#[cfg(all(unix, not(target_os = "linux")))]
async fn inherited_scope_pids(scope: &ProcessScope) -> Result<Vec<u32>, String> {
    #[cfg(test)]
    if scope.force_discovery_failure {
        return Err("강제된 프로세스 scope 조회 실패".into());
    }
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
async fn signal_pids(program: &str, signal: &str, pids: &[u32]) -> Result<(), String> {
    if pids.is_empty() {
        return Ok(());
    }
    let mut args = Vec::with_capacity(pids.len() + 1);
    args.push(signal.to_string());
    args.extend(pids.iter().map(u32::to_string));
    run_killer(program, &args).await
}

#[cfg(unix)]
async fn deliver_sigkill(
    program: &str,
    pids: &[u32],
    delivered: &mut std::collections::BTreeSet<u32>,
    failures: &mut std::collections::BTreeMap<u32, String>,
) {
    for pid in pids.iter().copied() {
        if delivered.contains(&pid) {
            continue;
        }
        match run_killer(program, &["-KILL".into(), pid.to_string()]).await {
            Ok(()) => {
                delivered.insert(pid);
                failures.remove(&pid);
            }
            Err(error) => {
                failures.insert(pid, error);
            }
        }
    }
}

#[cfg(unix)]
fn unresolved_kill_failures(
    delivered: &std::collections::BTreeSet<u32>,
    failures: &std::collections::BTreeMap<u32, String>,
    currently_scoped: &[u32],
) -> Vec<String> {
    currently_scoped
        .iter()
        .filter(|pid| !delivered.contains(pid))
        .map(|pid| {
            failures.get(pid).map_or_else(
                || format!("PID {pid}에 SIGKILL을 전달하지 못했습니다"),
                |error| format!("PID {pid} SIGKILL 실패: {error}"),
            )
        })
        .collect()
}

pub(crate) async fn terminate(child: &mut Child, scope: &ProcessScope) -> Result<(), String> {
    let mut cleanup_errors = Vec::new();
    #[cfg(unix)]
    {
        let mut delivered = std::collections::BTreeSet::new();
        let mut kill_failures = std::collections::BTreeMap::new();
        let root = child.id();
        let program = if std::path::Path::new("/bin/kill").is_file() {
            "/bin/kill"
        } else {
            "/usr/bin/kill"
        };
        if let Some(pid) = root {
            let group = format!("-{pid}");
            let _ = run_killer(
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
        match inherited_scope_pids(scope).await {
            Ok(pids) => {
                for pid in pids {
                    if !targets.contains(&pid) {
                        targets.push(pid);
                    }
                }
            }
            Err(error) => cleanup_errors.push(error),
        }
        for pid in scoped_pids(scope).await {
            if !targets.contains(&pid) {
                targets.push(pid);
            }
        }
        let _ = signal_pids(program, "-STOP", &targets).await;
        for _ in 0..2 {
            let mut discovered = scoped_pids(scope).await;
            match inherited_scope_pids(scope).await {
                Ok(pids) => discovered.extend(pids),
                Err(error) => cleanup_errors.push(error),
            }
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
            let _ = signal_pids(program, "-STOP", &added).await;
            tokio::task::yield_now().await;
        }
        deliver_sigkill(program, &targets, &mut delivered, &mut kill_failures).await;
        if let Some(pid) = root {
            let group = format!("-{pid}");
            let _ = run_killer(program, &["-KILL".to_string(), "--".to_string(), group]).await;
        }
        for _ in 0..2 {
            let mut remaining = scoped_pids(scope).await;
            match inherited_scope_pids(scope).await {
                Ok(pids) => remaining.extend(pids),
                Err(error) => cleanup_errors.push(error),
            }
            remaining.sort_unstable();
            remaining.dedup();
            if remaining.is_empty() {
                break;
            }
            deliver_sigkill(program, &remaining, &mut delivered, &mut kill_failures).await;
        }
        if child.id().is_some() {
            let _ = child.kill().await;
            match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => cleanup_errors.push(format!("루트 프로세스 대기 실패: {error}")),
                Err(_) => cleanup_errors.push("루트 프로세스 종료 확인 시간 초과".into()),
            }
        }
        for _ in 0..4 {
            let mut remaining = scoped_pids(scope).await;
            match inherited_scope_pids(scope).await {
                Ok(pids) => remaining.extend(pids),
                Err(error) => cleanup_errors.push(error),
            }
            remaining.sort_unstable();
            remaining.dedup();
            if remaining.iter().all(|pid| delivered.contains(pid)) {
                break;
            }
            deliver_sigkill(program, &remaining, &mut delivered, &mut kill_failures).await;
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let mut remaining = scoped_pids(scope).await;
        match inherited_scope_pids(scope).await {
            Ok(pids) => remaining.extend(pids),
            Err(error) => cleanup_errors.push(error),
        }
        remaining.sort_unstable();
        remaining.dedup();
        deliver_sigkill(program, &remaining, &mut delivered, &mut kill_failures).await;
        let mut currently_scoped = scoped_pids(scope).await;
        match inherited_scope_pids(scope).await {
            Ok(pids) => currently_scoped.extend(pids),
            Err(error) => cleanup_errors.push(error),
        }
        currently_scoped.sort_unstable();
        currently_scoped.dedup();
        deliver_sigkill(
            program,
            &currently_scoped,
            &mut delivered,
            &mut kill_failures,
        )
        .await;
        let mut verdict_scope = scoped_pids(scope).await;
        match inherited_scope_pids(scope).await {
            Ok(pids) => verdict_scope.extend(pids),
            Err(error) => cleanup_errors.push(error),
        }
        verdict_scope.sort_unstable();
        verdict_scope.dedup();
        cleanup_errors.extend(unresolved_kill_failures(
            &delivered,
            &kill_failures,
            &verdict_scope,
        ));
    }
    #[cfg(windows)]
    {
        if let Err(error) = scope.job.terminate() {
            cleanup_errors.push(format!("Windows Job Object 종료 실패: {error}"));
        }
        if child.id().is_some() {
            let _ = child.kill().await;
            match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => cleanup_errors.push(format!("루트 프로세스 대기 실패: {error}")),
                Err(_) => cleanup_errors.push("루트 프로세스 종료 확인 시간 초과".into()),
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    if child.id().is_some() {
        let _ = child.kill().await;
        match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => cleanup_errors.push(format!("루트 프로세스 대기 실패: {error}")),
            Err(_) => cleanup_errors.push("루트 프로세스 종료 확인 시간 초과".into()),
        }
    }
    cleanup_errors.sort();
    cleanup_errors.dedup();
    if cleanup_errors.is_empty() {
        Ok(())
    } else {
        Err(cleanup_errors.join("; "))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn successful_sigkill_delivery_is_not_failed_by_a_stale_process_listing() {
        let delivered = std::collections::BTreeSet::from([42]);
        let failures = std::collections::BTreeMap::from([
            (43, "permission denied".to_string()),
            (45, "already gone".to_string()),
        ]);
        assert!(unresolved_kill_failures(&delivered, &failures, &[42]).is_empty());
        assert_eq!(
            unresolved_kill_failures(&delivered, &failures, &[43]),
            ["PID 43 SIGKILL 실패: permission denied"]
        );
        assert!(unresolved_kill_failures(&delivered, &failures, &[]).is_empty());
        assert_eq!(
            unresolved_kill_failures(&delivered, &failures, &[44]),
            ["PID 44에 SIGKILL을 전달하지 못했습니다"]
        );
    }

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

    #[tokio::test]
    async fn discovery_failure_still_kills_the_root_group() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-process-discovery-failure-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let marker = root.join("survived");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "(sleep 1; printf survived > \"$1\") & wait",
            "rafikx-process-discovery-failure",
            marker.to_str().expect("marker path"),
        ]);
        let (mut child, mut scope) = spawn_scoped(&mut command).expect("spawn process tree");
        scope.force_discovery_failure = true;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let error = terminate(&mut child, &scope)
            .await
            .expect_err("discovery failure must be reported");
        assert!(error.contains("강제된"));
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[tokio::test]
    async fn job_object_kills_detached_descendants() {
        let root =
            std::env::temp_dir().join(format!("rafikx-windows-job-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&root).expect("test directory");
        let marker = root.join("survived");
        let nested = format!(
            "$p='{}'; Start-Sleep -Milliseconds 900; Set-Content -LiteralPath $p -Value survived",
            marker.display()
        );
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "Start-Process powershell.exe -ArgumentList @('-NoLogo','-NoProfile','-NonInteractive','-Command',\"{}\"); Start-Sleep -Seconds 5",
                nested.replace('"', "`\"")
            ),
        ]);
        let (mut child, scope) = spawn_scoped(&mut command).expect("spawn Windows process tree");
        tokio::time::sleep(Duration::from_millis(200)).await;
        terminate(&mut child, &scope).await.expect("terminate job");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
