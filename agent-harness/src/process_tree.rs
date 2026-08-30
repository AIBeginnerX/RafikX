#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::ExitStatus;
use std::process::Stdio;
#[cfg(unix)]
use std::sync::Mutex;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[cfg(windows)]
use windows_job::WindowsJob;

#[cfg(unix)]
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

#[cfg(unix)]
static PROCESS_SCOPE_ID: AtomicU64 = AtomicU64::new(1);
#[cfg(unix)]
static PROCESS_SCOPE_SPAWN_LOCK: Mutex<()> = Mutex::new(());
#[cfg(unix)]
const PROCESS_SCOPE_ENV: &str = "RAFIKX_PROCESS_SCOPE";
#[cfg(unix)]
const MAX_SCOPE_SCAN_BYTES: usize = 64 * 1024 * 1024;
#[cfg(unix)]
const PROCESS_CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(target_os = "linux")]
const PROCESS_STOP_SIGNAL: i32 = 19;
#[cfg(all(unix, not(target_os = "linux")))]
const PROCESS_STOP_SIGNAL: i32 = 17;
#[cfg(unix)]
const PROCESS_KILL_SIGNAL: i32 = 9;

#[cfg(unix)]
trait GenerationBoundProcess {
    fn signal(&self, signal: i32) -> Result<(), String>;
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ProcessIdentity {
    pid: u32,
    pidfd: OwnedFd,
}

#[cfg(target_os = "linux")]
// SAFETY: `syscall` uses the platform C ABI. Calls below pass only integer values and a null
// siginfo pointer for Linux pidfd_open/pidfd_send_signal, and check -1 through errno.
unsafe extern "C" {
    fn syscall(number: std::os::raw::c_long, ...) -> std::os::raw::c_long;
}

#[cfg(target_os = "linux")]
impl ProcessIdentity {
    const PIDFD_OPEN: std::os::raw::c_long = 434;
    const PIDFD_SEND_SIGNAL: std::os::raw::c_long = 424;

    fn capture(pid: u32) -> Result<Option<Self>, String> {
        // SAFETY: pidfd_open takes a numeric pid and zero flags and returns a new descriptor.
        let descriptor = unsafe { syscall(Self::PIDFD_OPEN, pid as i32, 0u32) };
        if descriptor == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(3) {
                return Ok(None);
            }
            return Err(format!("PID {pid} pidfd 획득 실패: {error}"));
        }
        // SAFETY: A successful pidfd_open returns a uniquely owned descriptor.
        let pidfd = unsafe { OwnedFd::from_raw_fd(descriptor as i32) };
        Ok(Some(Self { pid, pidfd }))
    }
}

#[cfg(target_os = "linux")]
impl GenerationBoundProcess for ProcessIdentity {
    fn signal(&self, signal: i32) -> Result<(), String> {
        // SAFETY: The pidfd is live and owned by `self`; null siginfo with zero flags requests the
        // ordinary signal operation for exactly the process generation captured by this fd.
        let result = unsafe {
            syscall(
                Self::PIDFD_SEND_SIGNAL,
                self.pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<std::ffi::c_void>(),
                0u32,
            )
        };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(3) {
                return Ok(());
            }
            return Err(format!(
                "PID {} pidfd 신호 {signal} 실패: {error}",
                self.pid
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AuditToken {
    value: [u32; 8],
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct ProcessIdentity {
    pid: u32,
    audit_token: AuditToken,
    unique_id: u64,
    parent_unique_id: u64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Debug, Default)]
struct ProcUniqueIdentifierInfo {
    executable_uuid: [u8; 16],
    unique_id: u64,
    parent_unique_id: u64,
    pid_version: i32,
    reserved_2: u32,
    reserved_3: u64,
    reserved_4: u64,
}

#[cfg(target_os = "macos")]
// SAFETY: These signatures and layouts match libproc's proc_pidinfo and audit-token APIs.
unsafe extern "C" {
    fn proc_pidinfo(
        pid: i32,
        flavor: i32,
        arg: u64,
        buffer: *mut std::ffi::c_void,
        size: i32,
    ) -> i32;
    fn proc_signal_with_audittoken(token: *mut AuditToken, signal: i32) -> i32;
    fn proc_listpidspath(
        process_type: u32,
        type_info: u32,
        path: *const std::os::raw::c_char,
        path_flags: u32,
        buffer: *mut std::ffi::c_void,
        buffer_size: i32,
    ) -> i32;
    fn proc_listallpids(buffer: *mut std::ffi::c_void, buffer_size: i32) -> i32;
}

#[cfg(target_os = "macos")]
impl ProcessIdentity {
    const PROC_PID_UNIQUE_IDENTIFIER_INFO: i32 = 17;

    fn capture(pid: u32) -> Result<Option<Self>, String> {
        let mut identity = ProcUniqueIdentifierInfo::default();
        let expected = std::mem::size_of::<ProcUniqueIdentifierInfo>() as i32;
        // SAFETY: `identity` is a writable buffer with the size and repr(C) layout for flavor 17.
        let read = unsafe {
            proc_pidinfo(
                pid as i32,
                Self::PROC_PID_UNIQUE_IDENTIFIER_INFO,
                0,
                (&raw mut identity).cast(),
                expected,
            )
        };
        if read != expected || identity.unique_id == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(3) {
                return Ok(None);
            }
            return Err(format!("PID {pid} 고유 신원 조회 실패: {error}"));
        }
        let mut token = AuditToken { value: [0; 8] };
        token.value[5] = pid;
        token.value[7] = identity.pid_version as u32;
        Ok(Some(Self {
            pid,
            audit_token: token,
            unique_id: identity.unique_id,
            parent_unique_id: identity.parent_unique_id,
        }))
    }
}

#[cfg(target_os = "macos")]
impl GenerationBoundProcess for ProcessIdentity {
    fn signal(&self, signal: i32) -> Result<(), String> {
        let mut token = self.audit_token;
        // SAFETY: `token` contains the captured pid-version and remains writable for libproc.
        let error = unsafe { proc_signal_with_audittoken(&raw mut token, signal) };
        if error != 0 {
            if error == 3 {
                return Ok(());
            }
            return Err(format!(
                "PID {} audit-token 신호 {signal} 실패: {}",
                self.pid,
                std::io::Error::from_raw_os_error(error)
            ));
        }
        Ok(())
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
#[derive(Debug)]
struct ProcessIdentity {
    pid: u32,
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
impl ProcessIdentity {
    fn capture(pid: u32) -> Result<Option<Self>, String> {
        Err(format!(
            "PID {pid}: 이 Unix 플랫폼에는 세대 고정 프로세스 신원 API가 없습니다"
        ))
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
impl GenerationBoundProcess for ProcessIdentity {
    fn signal(&self, _signal: i32) -> Result<(), String> {
        Err(format!(
            "PID {}: 세대가 확인되지 않은 프로세스에는 신호를 보내지 않습니다",
            self.pid
        ))
    }
}

#[derive(Debug)]
pub(crate) struct ProcessScope {
    #[cfg(unix)]
    marker: String,
    #[cfg(unix)]
    marker_path: PathBuf,
    #[cfg(target_os = "macos")]
    root_unique_id: Option<u64>,
    #[cfg(target_os = "linux")]
    marker_device: u64,
    #[cfg(target_os = "linux")]
    marker_inode: u64,
    #[cfg(windows)]
    job: WindowsJob,
    #[cfg(test)]
    force_discovery_failure: bool,
}

#[cfg(unix)]
// SAFETY: `fcntl` and `kill` use the POSIX ABI. Calls below pass only owned descriptors or
// integer process-group/signal values, check errno, and retain no pointers or Rust references.
unsafe extern "C" {
    fn fcntl(fd: std::os::raw::c_int, command: std::os::raw::c_int, ...) -> std::os::raw::c_int;
    fn kill(pid: std::os::raw::c_int, signal: std::os::raw::c_int) -> std::os::raw::c_int;
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
        let _spawn_guard = PROCESS_SCOPE_SPAWN_LOCK
            .lock()
            .map_err(|_| std::io::Error::other("process scope spawn lock poisoned"))?;
        let id = PROCESS_SCOPE_ID.fetch_add(1, Ordering::Relaxed);
        let marker = format!(
            "{}-{id:020}-{}",
            std::process::id(),
            crate::db::Db::new_id()
        );
        let (marker_file, marker_path) = create_scope_file(&marker)?;
        #[cfg(target_os = "linux")]
        let marker_metadata = marker_file.metadata()?;
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
        match command.spawn() {
            Ok(child) => {
                #[cfg(target_os = "macos")]
                let root_unique_id = child.id().and_then(|pid| {
                    ProcessIdentity::capture(pid)
                        .ok()
                        .flatten()
                        .map(|identity| identity.unique_id)
                });
                drop(marker_file);
                Ok((
                    child,
                    ProcessScope {
                        marker,
                        marker_path,
                        #[cfg(target_os = "macos")]
                        root_unique_id,
                        #[cfg(target_os = "linux")]
                        marker_device: marker_metadata.dev(),
                        #[cfg(target_os = "linux")]
                        marker_inode: marker_metadata.ino(),
                        #[cfg(test)]
                        force_discovery_failure: false,
                    },
                ))
            }
            Err(error) => {
                drop(marker_file);
                let _ = std::fs::remove_file(&marker_path);
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

#[cfg(unix)]
fn signal_process_group(root: u32, signal: i32) -> Result<(), String> {
    let group = i32::try_from(root).map_err(|_| format!("프로세스 그룹 ID 범위 초과: {root}"))?;
    // SAFETY: The caller owns the unreaped group leader while this negative PGID is used.
    if unsafe { kill(-group, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(3) {
        Ok(())
    } else {
        Err(format!("프로세스 그룹 {root} 신호 {signal} 실패: {error}"))
    }
}

#[cfg(unix)]
fn active_root(child: &mut Child, cleanup_errors: &mut Vec<String>) -> Option<u32> {
    match child.try_wait() {
        Ok(Some(_)) => None,
        Ok(None) => child.id(),
        Err(error) => {
            cleanup_errors.push(format!("루트 프로세스 상태 확인 실패: {error}"));
            child.id()
        }
    }
}

#[cfg(unix)]
async fn read_bounded_output<R>(mut reader: R) -> std::io::Result<(Vec<u8>, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    let mut overflow = false;
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_SCOPE_SCAN_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..read.min(remaining)]);
        overflow |= read > remaining;
    }
    Ok((output, overflow))
}

#[cfg(unix)]
async fn run_bounded_utility(
    command: &mut Command,
    label: &str,
    deadline: Duration,
) -> Result<(ExitStatus, Vec<u8>), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("{label} 실행 실패: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{label} 표준 출력을 열 수 없습니다"))?;
    let reader = tokio::spawn(read_bounded_output(stdout));
    let status = match tokio::time::timeout(deadline, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            let _ = child.kill().await;
            let _ = reader.await;
            return Err(format!("{label} 대기 실패: {error}"));
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = reader.await;
            return Err(format!("{label} 시간 초과"));
        }
    };
    let (output, overflow) = reader
        .await
        .map_err(|error| format!("{label} 출력 작업 실패: {error}"))?
        .map_err(|error| format!("{label} 출력 수집 실패: {error}"))?;
    if overflow {
        return Err(format!("{label} 출력 상한을 초과했습니다"));
    }
    Ok((status, output))
}

#[cfg(unix)]
async fn descendant_pids(root: u32) -> Result<Vec<u32>, String> {
    let program = if std::path::Path::new("/bin/ps").is_file() {
        "/bin/ps"
    } else {
        "/usr/bin/ps"
    };
    let mut command = Command::new(program);
    command.args(["-axo", "pid=,ppid="]);
    let (status, output) =
        run_bounded_utility(&mut command, "프로세스 트리 조회", Duration::from_secs(2)).await?;
    if !status.success() {
        return Err(format!(
            "프로세스 트리 조회 실패 (exit {:?})",
            status.code()
        ));
    }
    let relations = String::from_utf8_lossy(&output)
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
    Ok(descendants.into_iter().collect())
}

#[cfg(unix)]
async fn scoped_pids(scope: &ProcessScope) -> Result<Vec<u32>, String> {
    let program = if std::path::Path::new("/bin/ps").is_file() {
        "/bin/ps"
    } else {
        "/usr/bin/ps"
    };
    let mut command = Command::new(program);
    command.args(["eww", "-axo", "pid=,command="]);
    let (status, output) = run_bounded_utility(
        &mut command,
        "프로세스 scope 환경 조회",
        Duration::from_secs(2),
    )
    .await?;
    if !status.success() {
        return Err(format!(
            "프로세스 scope 환경 조회 실패 (exit {:?})",
            status.code()
        ));
    }
    let needle = format!("{PROCESS_SCOPE_ENV}={}", scope.marker).into_bytes();
    Ok(output
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            contains_scope_marker(line, &needle)
                .then(|| {
                    String::from_utf8_lossy(line)
                        .split_whitespace()
                        .next()
                        .and_then(|field| field.parse::<u32>().ok())
                })
                .flatten()
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect())
}

#[cfg(target_os = "linux")]
fn file_scoped_pids(device: u64, inode: u64, budget: Duration) -> Result<Vec<u32>, String> {
    let deadline = std::time::Instant::now() + budget;
    let entries =
        std::fs::read_dir("/proc").map_err(|error| format!("/proc 조회 실패: {error}"))?;
    let mut matches = std::collections::BTreeSet::new();
    let mut scanned = 0usize;
    for entry in entries.flatten() {
        if std::time::Instant::now() >= deadline {
            return Err("프로세스 scope 파일 조회 시간 상한을 초과했습니다".into());
        }
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
            if std::time::Instant::now() >= deadline {
                return Err("프로세스 scope 파일 조회 시간 상한을 초과했습니다".into());
            }
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

#[cfg(target_os = "macos")]
async fn file_scoped_pids(scope: &ProcessScope) -> Result<Vec<u32>, String> {
    use std::os::unix::ffi::OsStrExt as _;

    const MAX_MATCHING_PIDS: usize = 4096;
    let path = std::ffi::CString::new(scope.marker_path.as_os_str().as_bytes())
        .map_err(|_| "프로세스 scope 경로에 NUL 문자가 있습니다".to_string())?;
    let mut pids = vec![0i32; MAX_MATCHING_PIDS];
    // SAFETY: `path` is NUL-terminated and `pids` is a writable, correctly sized int buffer.
    let bytes = unsafe {
        proc_listpidspath(
            1,
            0,
            path.as_ptr(),
            0,
            pids.as_mut_ptr().cast(),
            std::mem::size_of_val(pids.as_slice()) as i32,
        )
    };
    if bytes == -1 {
        return Err(format!(
            "프로세스 scope 파일 조회 실패: {}",
            std::io::Error::last_os_error()
        ));
    }
    let count = (bytes as usize) / std::mem::size_of::<i32>();
    if count >= pids.len() {
        return Err("프로세스 scope PID 조회 상한을 초과했습니다".into());
    }
    pids.truncate(count);
    Ok(pids
        .into_iter()
        .filter_map(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0 && *pid != std::process::id())
        .collect())
}

#[cfg(target_os = "macos")]
fn lineage_scoped_pids(scope: &ProcessScope) -> Result<Vec<u32>, String> {
    const MAX_LIVE_PIDS: usize = 4096;
    let Some(root_unique_id) = scope.root_unique_id else {
        return Ok(Vec::new());
    };
    let mut pids = vec![0i32; MAX_LIVE_PIDS];
    // SAFETY: `pids` is a writable int buffer whose byte size is passed exactly.
    let count = unsafe {
        proc_listallpids(
            pids.as_mut_ptr().cast(),
            std::mem::size_of_val(pids.as_slice()) as i32,
        )
    };
    if count == -1 {
        return Err(format!(
            "프로세스 고유 계보 조회 실패: {}",
            std::io::Error::last_os_error()
        ));
    }
    let count = count as usize;
    if count >= pids.len() {
        return Err("프로세스 고유 계보 PID 상한을 초과했습니다".into());
    }
    pids.truncate(count);
    let identities = pids
        .into_iter()
        .filter_map(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0 && *pid != std::process::id())
        .filter_map(|pid| ProcessIdentity::capture(pid).ok().flatten())
        .collect::<Vec<_>>();
    let mut lineage = std::collections::BTreeSet::from([root_unique_id]);
    let mut matches = std::collections::BTreeSet::new();
    loop {
        let mut changed = false;
        for identity in &identities {
            if lineage.contains(&identity.parent_unique_id)
                && lineage.insert(identity.unique_id)
            {
                matches.insert(identity.pid);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(matches.into_iter().collect())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
async fn file_scoped_pids(scope: &ProcessScope) -> Result<Vec<u32>, String> {
    use std::os::unix::process::ExitStatusExt as _;

    let program = ["/usr/sbin/lsof", "/usr/bin/lsof"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
        .ok_or_else(|| "프로세스 scope 확인용 lsof를 찾을 수 없습니다".to_string())?;
    let mut last_failure = String::new();
    for attempt in 0..2 {
        let mut command = Command::new(program);
        command.args(["-Fp", "--"]).arg(&scope.marker_path);
        let (status, output) =
            run_bounded_utility(&mut command, "프로세스 scope lsof", Duration::from_secs(3))
                .await?;
        if status.success() || status.code() == Some(1) {
            return Ok(String::from_utf8_lossy(&output)
                .lines()
                .filter_map(|line| line.strip_prefix('p')?.parse::<u32>().ok())
                .filter(|pid| *pid != std::process::id())
                .collect());
        }
        last_failure = format!("종료 코드 {:?}, 신호 {:?}", status.code(), status.signal());
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
    let device = scope.marker_device;
    let inode = scope.marker_inode;
    let scan = tokio::task::spawn_blocking(move || {
        file_scoped_pids(device, inode, Duration::from_secs(1))
    });
    tokio::time::timeout(Duration::from_secs(2), scan)
        .await
        .map_err(|_| "프로세스 scope 파일 조회 시간 초과".to_string())?
        .map_err(|error| format!("프로세스 scope 파일 조회 작업 실패: {error}"))?
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
async fn discover_scope_pids(
    scope: &ProcessScope,
    root: Option<u32>,
    cleanup_errors: &mut Vec<String>,
) -> Vec<u32> {
    let mut discovered = match scoped_pids(scope).await {
        Ok(pids) => pids,
        Err(error) => {
            cleanup_errors.push(error);
            Vec::new()
        }
    };
    match inherited_scope_pids(scope).await {
        Ok(pids) => discovered.extend(pids),
        Err(error) => cleanup_errors.push(error),
    }
    #[cfg(target_os = "macos")]
    match lineage_scoped_pids(scope) {
        Ok(pids) => discovered.extend(pids),
        Err(error) => cleanup_errors.push(error),
    }
    if let Some(pid) = root {
        match descendant_pids(pid).await {
            Ok(pids) => discovered.extend(pids),
            Err(error) => cleanup_errors.push(error),
        }
    }
    discovered.retain(|pid| Some(*pid) != root && *pid != std::process::id());
    discovered.sort_unstable();
    discovered.dedup();
    discovered
}

#[cfg(unix)]
async fn capture_scoped_identities(
    scope: &ProcessScope,
    root: Option<u32>,
    cleanup_errors: &mut Vec<String>,
) -> (Vec<ProcessIdentity>, Vec<u32>) {
    let candidates = discover_scope_pids(scope, root, cleanup_errors).await;
    if candidates.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut captured = Vec::new();
    for &pid in &candidates {
        match ProcessIdentity::capture(pid) {
            Ok(Some(identity)) => captured.push(identity),
            Ok(None) => {}
            Err(error) => {
                cleanup_errors.push(error);
            }
        }
    }
    (captured, candidates)
}

#[cfg(unix)]
fn signal_captured_processes<T: GenerationBoundProcess>(
    identities: &[T],
    signal: i32,
    cleanup_errors: &mut Vec<String>,
) {
    for identity in identities {
        if let Err(error) = identity.signal(signal) {
            cleanup_errors.push(error);
        }
    }
}

#[cfg(unix)]
async fn terminate_unix_inner(child: &mut Child, scope: &ProcessScope) -> Vec<String> {
    let mut cleanup_errors = Vec::new();
    let mut root = active_root(child, &mut cleanup_errors);
    if let Some(pid) = root
        && let Err(error) = signal_process_group(pid, PROCESS_STOP_SIGNAL)
    {
        if active_root(child, &mut cleanup_errors).is_none() {
            root = None;
        } else {
            cleanup_errors.push(error);
        }
    }

    let (first, first_visible) =
        capture_scoped_identities(scope, root, &mut cleanup_errors).await;
    if root.is_none() && first_visible.is_empty() {
        return cleanup_errors;
    }

    let mut captured = first;
    signal_captured_processes(&captured, PROCESS_STOP_SIGNAL, &mut cleanup_errors);
    let mut visible = !first_visible.is_empty();
    for _ in 0..2 {
        if !visible {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        let (round, revalidated) =
            capture_scoped_identities(scope, root, &mut cleanup_errors).await;
        visible = !revalidated.is_empty();
        signal_captured_processes(&round, PROCESS_STOP_SIGNAL, &mut cleanup_errors);
        captured.extend(round);
    }
    signal_captured_processes(&captured, PROCESS_KILL_SIGNAL, &mut cleanup_errors);
    if let Some(pid) = root
        && let Err(error) = signal_process_group(pid, PROCESS_KILL_SIGNAL)
        && active_root(child, &mut cleanup_errors).is_some()
    {
        cleanup_errors.push(error);
    }

    if child.id().is_some() {
        if let Err(error) = child.start_kill() {
            cleanup_errors.push(format!("루트 프로세스 종료 요청 실패: {error}"));
        }
        match tokio::time::timeout(Duration::from_millis(500), child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => cleanup_errors.push(format!("루트 프로세스 대기 실패: {error}")),
            Err(_) => cleanup_errors.push("루트 프로세스 종료 확인 시간 초과".into()),
        }
    }

    for _ in 0..2 {
        let (remaining, revalidated) =
            capture_scoped_identities(scope, None, &mut cleanup_errors).await;
        if revalidated.is_empty() {
            break;
        }
        signal_captured_processes(&remaining, PROCESS_STOP_SIGNAL, &mut cleanup_errors);
        signal_captured_processes(&remaining, PROCESS_KILL_SIGNAL, &mut cleanup_errors);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let verdict_scope = discover_scope_pids(scope, None, &mut cleanup_errors).await;
    cleanup_errors.extend(
        verdict_scope
            .into_iter()
            .map(|pid| format!("PID {pid}가 프로세스 scope에 남아 있습니다")),
    );
    cleanup_errors
}

pub(crate) async fn terminate(child: &mut Child, scope: &ProcessScope) -> Result<(), String> {
    let mut cleanup_errors = Vec::new();
    #[cfg(unix)]
    {
        match tokio::time::timeout(PROCESS_CLEANUP_TIMEOUT, terminate_unix_inner(child, scope)).await
        {
            Ok(errors) => cleanup_errors.extend(errors),
            Err(_) => {
                cleanup_errors.push("프로세스 scope 정리 전체 시간 초과".into());
                if let Some(pid) = active_root(child, &mut cleanup_errors) {
                    let _ = signal_process_group(pid, PROCESS_KILL_SIGNAL);
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
                }
            }
        }
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

    struct FakeIdentity<'a> {
        generation: u64,
        current_generation: &'a std::cell::Cell<u64>,
        signaled_generations: &'a std::cell::RefCell<Vec<u64>>,
    }

    impl GenerationBoundProcess for FakeIdentity<'_> {
        fn signal(&self, _signal: i32) -> Result<(), String> {
            if self.current_generation.get() == self.generation {
                self.signaled_generations.borrow_mut().push(self.generation);
            }
            Ok(())
        }
    }

    #[test]
    fn captured_identity_does_not_signal_a_same_pid_replacement() {
        let current_generation = std::cell::Cell::new(1);
        let signaled_generations = std::cell::RefCell::new(Vec::new());
        let original = FakeIdentity {
            generation: 1,
            current_generation: &current_generation,
            signaled_generations: &signaled_generations,
        };
        current_generation.set(2);
        let mut errors = Vec::new();

        signal_captured_processes(&[original], PROCESS_KILL_SIGNAL, &mut errors);

        assert!(errors.is_empty());
        assert!(signaled_generations.borrow().is_empty());
        let replacement = FakeIdentity {
            generation: 2,
            current_generation: &current_generation,
            signaled_generations: &signaled_generations,
        };
        signal_captured_processes(&[replacement], PROCESS_KILL_SIGNAL, &mut errors);
        assert_eq!(*signaled_generations.borrow(), [2]);
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

    #[cfg(target_os = "linux")]
    #[test]
    fn expired_proc_scope_budget_fails_immediately() {
        let started = std::time::Instant::now();
        let error = file_scoped_pids(0, 0, Duration::ZERO).expect_err("expired budget must fail");

        assert!(error.contains("시간 상한"), "{error}");
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn completed_command_cleanup_stays_within_global_budget() {
        let mut command = Command::new("true");
        let (mut child, scope) = spawn_scoped(&mut command).expect("spawn completed command");
        child.wait().await.expect("wait completed command");
        let started = std::time::Instant::now();

        terminate(&mut child, &scope)
            .await
            .expect("clean completed command");

        assert!(
            started.elapsed() < PROCESS_CLEANUP_TIMEOUT + Duration::from_secs(1),
            "cleanup exceeded its global budget: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn parent_closes_the_scope_marker_after_spawn() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);
        let (mut child, scope) = spawn_scoped(&mut command).expect("spawn scoped child");

        #[cfg(target_os = "linux")]
        {
            let inherited = std::fs::read_dir("/proc/self/fd")
                .expect("parent descriptors")
                .flatten()
                .filter_map(|entry| entry.path().metadata().ok())
                .any(|metadata| {
                    metadata.dev() == scope.marker_device && metadata.ino() == scope.marker_inode
                });
            assert!(!inherited, "parent retained the process scope descriptor");
        }
        #[cfg(not(target_os = "linux"))]
        {
            let Some(program) = ["/usr/sbin/lsof", "/usr/bin/lsof"]
                .into_iter()
                .find(|path| std::path::Path::new(path).is_file())
            else {
                terminate(&mut child, &scope)
                    .await
                    .expect("terminate child");
                return;
            };
            let status = std::process::Command::new(program)
                .args(["-a", "-p", &std::process::id().to_string(), "--"])
                .arg(&scope.marker_path)
                .status()
                .expect("inspect parent descriptors");
            assert!(
                !status.success(),
                "parent retained the process scope descriptor"
            );
        }

        terminate(&mut child, &scope)
            .await
            .expect("terminate child");
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
        let ready = root.join("ready");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "\"$1\" -c 'import os,sys,time\nchild=os.fork()\nif child == 0:\n os.setsid()\n if os.fork() > 0: os._exit(0)\n open(sys.argv[1], \"w\").write(str(os.getpid()))\n time.sleep(60)\nelse:\n time.sleep(60)' \"$2\"",
            "rafikx-process-session",
            python,
            ready.to_str().expect("ready path"),
        ]);
        let (mut child, scope) = spawn_scoped(&mut command).expect("spawn detached process");
        let daemon_pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&ready)
                    && let Ok(pid) = pid.parse::<u32>()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("detached daemon ready");
        terminate(&mut child, &scope)
            .await
            .expect("terminate detached tree");
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let output = std::process::Command::new("ps")
                    .args(["-p", &daemon_pid.to_string(), "-o", "stat="])
                    .output()
                    .expect("inspect detached daemon");
                let state = String::from_utf8_lossy(&output.stdout);
                if !output.status.success()
                    || state.trim().is_empty()
                    || state.trim().starts_with('Z')
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("detached daemon terminated");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn retained_scope_fd_survives_environment_clearing_and_reparenting() {
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
