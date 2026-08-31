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
use std::sync::Arc;
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
static PROCESS_DISCOVERY_PERMITS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)));
#[cfg(unix)]
static PROCESS_CLEANUP_ADMISSION_PERMITS: std::sync::LazyLock<
    std::sync::Arc<tokio::sync::Semaphore>,
> = std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)));
#[cfg(unix)]
const PROCESS_SCOPE_ENV: &str = "RAFIKX_PROCESS_SCOPE";
#[cfg(unix)]
const MAX_SCOPE_SCAN_BYTES: usize = 64 * 1024 * 1024;
#[cfg(unix)]
const PROCESS_CLEANUP_TIMEOUT: Duration = Duration::from_secs(8);
#[cfg(unix)]
const PROCESS_FALLBACK_TIMEOUT: Duration = Duration::from_secs(3);

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

    fn captured_generation_is_gone(&self) -> Result<bool, String> {
        // SAFETY: The null signal (0) delivers nothing and only probes liveness of exactly the
        // generation this pidfd names; null siginfo with zero flags is the ordinary probe form.
        // ESRCH means that captured generation is already gone.
        let result = unsafe {
            syscall(
                Self::PIDFD_SEND_SIGNAL,
                self.pidfd.as_raw_fd(),
                0i32,
                std::ptr::null::<std::ffi::c_void>(),
                0u32,
            )
        };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(3) {
                return Ok(true);
            }
            return Err(format!("PID {} pidfd 세대 재검증 실패: {error}", self.pid));
        }
        Ok(false)
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
    pid_version: u32,
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

    fn matches_current_generation(&self, current: Option<&Self>) -> bool {
        current.is_some_and(|current| {
            self.pid == current.pid
                && self.unique_id == current.unique_id
                && self.pid_version == current.pid_version
        })
    }

    fn captured_generation_is_gone(&self) -> Result<bool, String> {
        Ok(!self.matches_current_generation(Self::capture(self.pid)?.as_ref()))
    }

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
        let pid_version = identity.pid_version as u32;
        token.value[7] = pid_version;
        Ok(Some(Self {
            pid,
            audit_token: token,
            unique_id: identity.unique_id,
            pid_version,
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
            let signal_error = std::io::Error::from_raw_os_error(error);
            if signal_error.kind() == std::io::ErrorKind::PermissionDenied {
                // EPERM can race with target exit. Suppress it only when an immediate unique-id +
                // pid-version lookup proves that the exact captured generation is already gone.
                match self.captured_generation_is_gone() {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(revalidation_error) => {
                        return Err(format!(
                            "PID {} audit-token 신호 {signal} 실패: {signal_error}; 세대 재검증 실패: {revalidation_error}",
                            self.pid
                        ));
                    }
                }
            }
            return Err(format!(
                "PID {} audit-token 신호 {signal} 실패: {}",
                self.pid, signal_error
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
    #[cfg(unix)]
    discovery_reservation: Arc<tokio::sync::OwnedSemaphorePermit>,
    #[cfg(unix)]
    discovery_operation: Arc<tokio::sync::Mutex<()>>,
    #[cfg(windows)]
    job: WindowsJob,
    #[cfg(test)]
    force_discovery_failure: bool,
    #[cfg(test)]
    force_cleanup_timeout: bool,
    #[cfg(all(test, unix))]
    cleanup_probe: Option<std::sync::Arc<CleanupProbe>>,
}

pub(crate) struct ScopedProcess {
    child: Option<Child>,
    scope: Option<ProcessScope>,
}

impl ScopedProcess {
    pub(crate) fn new(child: Child, scope: ProcessScope) -> Self {
        Self {
            child: Some(child),
            scope: Some(scope),
        }
    }

    pub(crate) fn child_mut(&mut self) -> Result<&mut Child, String> {
        self.child
            .as_mut()
            .ok_or_else(|| "프로세스 소유권이 이미 정리 작업으로 이동했습니다".to_string())
    }

    pub(crate) async fn terminate(mut self) -> Result<(), String> {
        let child = self
            .child
            .take()
            .ok_or_else(|| "프로세스 소유권이 이미 정리 작업으로 이동했습니다".to_string())?;
        let scope = self
            .scope
            .take()
            .ok_or_else(|| "프로세스 scope가 이미 정리 작업으로 이동했습니다".to_string())?;
        terminate(child, scope).await
    }
}

impl Drop for ScopedProcess {
    fn drop(&mut self) {
        let (Some(child), Some(scope)) = (self.child.take(), self.scope.take()) else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(terminate_owned(child, scope));
            return;
        }
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let _ = runtime.block_on(terminate_owned(child, scope));
        });
    }
}

#[cfg(all(test, unix))]
#[derive(Debug, Default)]
pub(crate) struct CleanupProbe {
    spawn_reservation_waiting: tokio::sync::Notify,
    worker_started: tokio::sync::Notify,
    cleanup_admission_acquired: tokio::sync::Notify,
    cleanup_admission_calls: std::sync::atomic::AtomicUsize,
    scope_discovery_started: tokio::sync::Notify,
    scope_discovery_calls: std::sync::atomic::AtomicUsize,
    scope_quiesced: tokio::sync::Notify,
    cleanup_admission_permits: Option<std::sync::Arc<tokio::sync::Semaphore>>,
    discovery_permits: Option<std::sync::Arc<tokio::sync::Semaphore>>,
}

#[cfg(all(test, unix))]
impl CleanupProbe {
    pub(crate) fn with_discovery_capacity(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            discovery_permits: Some(Arc::new(tokio::sync::Semaphore::new(capacity))),
            ..Self::default()
        })
    }

    pub(crate) fn discovery_permits(&self) -> Arc<tokio::sync::Semaphore> {
        Arc::clone(
            self.discovery_permits
                .as_ref()
                .expect("test discovery permits must be configured"),
        )
    }

    pub(crate) async fn wait_for_spawn_reservation(&self) {
        self.spawn_reservation_waiting.notified().await;
    }
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

pub(crate) async fn spawn_scoped(command: &mut Command) -> std::io::Result<(Child, ProcessScope)> {
    spawn_scoped_inner(
        command,
        #[cfg(all(test, unix))]
        None,
    )
    .await
}

#[cfg(all(test, unix))]
pub(crate) async fn spawn_scoped_with_probe(
    command: &mut Command,
    probe: Arc<CleanupProbe>,
) -> std::io::Result<(Child, ProcessScope)> {
    spawn_scoped_inner(command, Some(probe)).await
}

async fn spawn_scoped_inner(
    command: &mut Command,
    #[cfg(all(test, unix))] probe: Option<Arc<CleanupProbe>>,
) -> std::io::Result<(Child, ProcessScope)> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        #[cfg(test)]
        if let Some(probe) = &probe {
            probe.spawn_reservation_waiting.notify_one();
        }
        let discovery_permits = {
            #[cfg(test)]
            if let Some(permits) = probe
                .as_ref()
                .and_then(|probe| probe.discovery_permits.as_ref())
            {
                Arc::clone(permits)
            } else {
                Arc::clone(&PROCESS_DISCOVERY_PERMITS)
            }
            #[cfg(not(test))]
            Arc::clone(&PROCESS_DISCOVERY_PERMITS)
        };
        let discovery_reservation = Arc::new(
            discovery_permits
                .acquire_owned()
                .await
                .map_err(|_| std::io::Error::other("process discovery reservation is closed"))?,
        );
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
                        discovery_reservation,
                        discovery_operation: Arc::new(tokio::sync::Mutex::new(())),
                        #[cfg(test)]
                        force_discovery_failure: false,
                        #[cfg(test)]
                        force_cleanup_timeout: false,
                        #[cfg(all(test, unix))]
                        cleanup_probe: probe,
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
                #[cfg(test)]
                force_cleanup_timeout: false,
                #[cfg(all(test, unix))]
                cleanup_probe: None,
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
                    #[cfg(test)]
                    force_cleanup_timeout: false,
                    #[cfg(all(test, unix))]
                    cleanup_probe: None,
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
    // BSD·procps 모두 수용하는 형태 — BSD 전용 `-axo` 는 Linux procps 가 personality
    // 오류로 거부한다. 환경 변수(e)는 command 열 뒤에 붙어 scope 표식 매칭에 쓰인다.
    command.args(["axeww", "-o", "pid=,command="]);
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
            if lineage.contains(&identity.parent_unique_id) && lineage.insert(identity.unique_id) {
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

#[cfg(any(target_os = "linux", all(test, unix)))]
struct MarkerScanSpawnError<P> {
    ownership: Option<P>,
    error: std::io::Error,
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn take_marker_scan_ownership<P>(ownership: &Mutex<Option<P>>) -> Option<P> {
    match ownership.lock() {
        Ok(mut ownership) => ownership.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    }
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn spawn_marker_fd_scan<T, F, P>(
    ownership: P,
    scan: F,
) -> Result<tokio::sync::oneshot::Receiver<(P, T)>, MarkerScanSpawnError<P>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    P: Send + 'static,
{
    spawn_marker_fd_scan_inner(ownership, scan, false)
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn spawn_marker_fd_scan_inner<T, F, P>(
    ownership: P,
    scan: F,
    force_start_failure: bool,
) -> Result<tokio::sync::oneshot::Receiver<(P, T)>, MarkerScanSpawnError<P>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    P: Send + 'static,
{
    if force_start_failure {
        return Err(MarkerScanSpawnError {
            ownership: Some(ownership),
            error: std::io::Error::other("forced marker scan thread start failure"),
        });
    }
    let ownership = Arc::new(Mutex::new(Some(ownership)));
    let thread_ownership = Arc::clone(&ownership);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    match std::thread::Builder::new()
        .name("rafikx-marker-scan".into())
        .spawn(move || {
            let Some(ownership) = take_marker_scan_ownership(&thread_ownership) else {
                return;
            };
            let result = scan();
            let _ = sender.send((ownership, result));
        }) {
        Ok(_thread) => Ok(receiver),
        Err(error) => Err(MarkerScanSpawnError {
            ownership: take_marker_scan_ownership(&ownership),
            error,
        }),
    }
}

#[cfg(target_os = "linux")]
async fn inherited_scope_pids(
    scope: &ProcessScope,
    operation: Arc<tokio::sync::OwnedMutexGuard<()>>,
    reservation: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Result<Vec<u32>, String> {
    #[cfg(test)]
    if scope.force_discovery_failure {
        return Err("강제된 프로세스 scope 조회 실패".into());
    }
    let device = scope.marker_device;
    let inode = scope.marker_inode;
    let ownership = (Arc::clone(&operation), reservation);
    let scan = match spawn_marker_fd_scan(ownership, move || {
        file_scoped_pids(device, inode, Duration::from_secs(1))
    }) {
        Ok(scan) => scan,
        Err(error) => {
            drop(error.ownership);
            return Err(format!(
                "프로세스 scope 파일 조회 스레드 시작 실패: {}",
                error.error
            ));
        }
    };
    match tokio::time::timeout(Duration::from_secs(2), scan).await {
        Ok(Ok(((_operation, _reservation), result))) => result,
        Ok(Err(_)) => Err("프로세스 scope 파일 조회 작업이 중단되었습니다".into()),
        Err(_) => Err("프로세스 scope 파일 조회 시간 초과".to_string()),
    }
}

#[cfg(unix)]
fn process_cleanup_admission_permits(
    _scope: &ProcessScope,
) -> std::sync::Arc<tokio::sync::Semaphore> {
    #[cfg(test)]
    if let Some(permits) = _scope
        .cleanup_probe
        .as_ref()
        .and_then(|probe| probe.cleanup_admission_permits.as_ref())
    {
        return std::sync::Arc::clone(permits);
    }
    std::sync::Arc::clone(&PROCESS_CLEANUP_ADMISSION_PERMITS)
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
fn try_discovery_operation(
    operation: &Arc<tokio::sync::Mutex<()>>,
) -> Option<Arc<tokio::sync::OwnedMutexGuard<()>>> {
    Arc::clone(operation).try_lock_owned().ok().map(Arc::new)
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
    let operation = try_discovery_operation(&scope.discovery_operation);
    #[cfg(test)]
    if let Some(probe) = &scope.cleanup_probe {
        probe
            .scope_discovery_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        probe.scope_discovery_started.notify_one();
    }
    let mut discovered = match scoped_pids(scope).await {
        Ok(pids) => pids,
        Err(error) => {
            cleanup_errors.push(error);
            Vec::new()
        }
    };
    if let Some(operation) = operation.as_ref() {
        #[cfg(not(target_os = "linux"))]
        let _operation = operation;
        #[cfg(target_os = "linux")]
        let inherited = inherited_scope_pids(
            scope,
            Arc::clone(operation),
            Arc::clone(&scope.discovery_reservation),
        )
        .await;
        #[cfg(not(target_os = "linux"))]
        let inherited = inherited_scope_pids(scope).await;
        match inherited {
            Ok(pids) => discovered.extend(pids),
            Err(error) => cleanup_errors.push(error),
        }
    }
    let _operation = operation;
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
fn capture_and_quiesce_candidate_identities(
    candidates: Vec<u32>,
    root: Option<u32>,
    captured: &mut Vec<ProcessIdentity>,
    visible: &mut Vec<u32>,
    cleanup_errors: &mut Vec<String>,
) {
    for pid in candidates {
        if Some(pid) == root || pid == std::process::id() {
            continue;
        }
        if visible.contains(&pid) {
            continue;
        }
        visible.push(pid);
        match ProcessIdentity::capture(pid) {
            Ok(Some(identity)) => {
                if let Err(error) = identity.signal(PROCESS_STOP_SIGNAL) {
                    cleanup_errors.push(error);
                }
                captured.push(identity);
            }
            Ok(None) => {}
            Err(error) => cleanup_errors.push(error),
        }
    }
}

#[cfg(unix)]
async fn capture_scoped_identities(
    scope: &ProcessScope,
    root: Option<u32>,
    captured: &mut Vec<ProcessIdentity>,
    cleanup_errors: &mut Vec<String>,
) -> Vec<u32> {
    let operation = try_discovery_operation(&scope.discovery_operation);
    #[cfg(test)]
    if let Some(probe) = &scope.cleanup_probe {
        probe
            .scope_discovery_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        probe.scope_discovery_started.notify_one();
    }
    let mut visible = Vec::new();
    if let Some(operation) = operation.as_ref() {
        #[cfg(not(target_os = "linux"))]
        let _operation = operation;
        #[cfg(target_os = "linux")]
        let inherited = inherited_scope_pids(
            scope,
            Arc::clone(operation),
            Arc::clone(&scope.discovery_reservation),
        )
        .await;
        #[cfg(not(target_os = "linux"))]
        let inherited = inherited_scope_pids(scope).await;
        match inherited {
            Ok(pids) => capture_and_quiesce_candidate_identities(
                pids,
                root,
                captured,
                &mut visible,
                cleanup_errors,
            ),
            Err(error) => cleanup_errors.push(error),
        }
    }
    let _operation = operation;
    match scoped_pids(scope).await {
        Ok(pids) => capture_and_quiesce_candidate_identities(
            pids,
            root,
            captured,
            &mut visible,
            cleanup_errors,
        ),
        Err(error) => cleanup_errors.push(error),
    }
    #[cfg(target_os = "macos")]
    match lineage_scoped_pids(scope) {
        Ok(pids) => capture_and_quiesce_candidate_identities(
            pids,
            root,
            captured,
            &mut visible,
            cleanup_errors,
        ),
        Err(error) => cleanup_errors.push(error),
    }
    if let Some(pid) = root {
        match descendant_pids(pid).await {
            Ok(pids) => capture_and_quiesce_candidate_identities(
                pids,
                root,
                captured,
                &mut visible,
                cleanup_errors,
            ),
            Err(error) => cleanup_errors.push(error),
        }
    }
    visible.sort_unstable();
    visible.dedup();
    visible
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
fn quiesce_root_before_cleanup(child: &mut Child, cleanup_errors: &mut Vec<String>) {
    let root = active_root(child, cleanup_errors);
    if let Some(pid) = root
        && let Err(error) = signal_process_group(pid, PROCESS_STOP_SIGNAL)
        && active_root(child, cleanup_errors).is_some()
    {
        cleanup_errors.push(error);
    }
}

#[cfg(unix)]
async fn terminate_unix_inner(
    child: &mut Child,
    scope: &ProcessScope,
    root: Option<u32>,
    first_visible: Vec<u32>,
    captured: &mut Vec<ProcessIdentity>,
    cleanup_errors: &mut Vec<String>,
) {
    if root.is_none() && first_visible.is_empty() {
        return;
    }

    #[cfg(test)]
    if scope.force_cleanup_timeout {
        tokio::time::sleep(PROCESS_CLEANUP_TIMEOUT + Duration::from_millis(100)).await;
    }
    signal_captured_processes(captured, PROCESS_KILL_SIGNAL, cleanup_errors);
    if let Some(pid) = root
        && let Err(error) = signal_process_group(pid, PROCESS_KILL_SIGNAL)
        && active_root(child, cleanup_errors).is_some()
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

    let remaining_start = captured.len();
    let revalidated = capture_scoped_identities(scope, None, captured, cleanup_errors).await;
    if !revalidated.is_empty() {
        signal_captured_processes(
            &captured[remaining_start..],
            PROCESS_KILL_SIGNAL,
            cleanup_errors,
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let verdict_scope = discover_scope_pids(scope, None, cleanup_errors).await;
    cleanup_errors.extend(
        verdict_scope
            .into_iter()
            .map(|pid| format!("PID {pid}가 프로세스 scope에 남아 있습니다")),
    );
}

async fn terminate_owned(mut child: Child, scope: ProcessScope) -> Result<(), String> {
    let mut cleanup_errors = Vec::new();
    #[cfg(unix)]
    {
        let _discovery_reservation = Arc::clone(&scope.discovery_reservation);
        #[cfg(test)]
        if let Some(probe) = &scope.cleanup_probe {
            probe.worker_started.notify_one();
        }
        quiesce_root_before_cleanup(&mut child, &mut cleanup_errors);
        let mut captured = Vec::new();
        let root = active_root(&mut child, &mut cleanup_errors);
        let first_visible =
            capture_scoped_identities(&scope, root, &mut captured, &mut cleanup_errors).await;
        #[cfg(test)]
        if let Some(probe) = &scope.cleanup_probe {
            probe.scope_quiesced.notify_one();
        }
        let _cleanup_admission = match process_cleanup_admission_permits(&scope)
            .acquire_owned()
            .await
        {
            Ok(permit) => permit,
            Err(_) => {
                cleanup_errors.push("프로세스 정리 허가가 닫혔습니다".into());
                signal_captured_processes(&captured, PROCESS_KILL_SIGNAL, &mut cleanup_errors);
                if let Some(pid) = active_root(&mut child, &mut cleanup_errors) {
                    if let Err(error) = signal_process_group(pid, PROCESS_KILL_SIGNAL) {
                        cleanup_errors.push(error);
                    }
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(Duration::from_millis(250), child.wait()).await;
                }
                cleanup_errors.sort();
                cleanup_errors.dedup();
                return Err(cleanup_errors.join("; "));
            }
        };
        #[cfg(test)]
        if let Some(probe) = &scope.cleanup_probe {
            probe
                .cleanup_admission_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            probe.cleanup_admission_acquired.notify_one();
        }
        match tokio::time::timeout(
            PROCESS_CLEANUP_TIMEOUT,
            terminate_unix_inner(
                &mut child,
                &scope,
                root,
                first_visible,
                &mut captured,
                &mut cleanup_errors,
            ),
        )
        .await
        {
            Ok(()) => {}
            Err(_) => {
                cleanup_errors.push("프로세스 scope 정리 전체 시간 초과".into());
                signal_captured_processes(&captured, PROCESS_KILL_SIGNAL, &mut cleanup_errors);
                if let Some(pid) = active_root(&mut child, &mut cleanup_errors) {
                    let _ = signal_process_group(pid, PROCESS_KILL_SIGNAL);
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(Duration::from_millis(250), child.wait()).await;
                }
                match tokio::time::timeout(
                    PROCESS_FALLBACK_TIMEOUT,
                    capture_scoped_identities(&scope, None, &mut captured, &mut cleanup_errors),
                )
                .await
                {
                    Ok(_) => signal_captured_processes(
                        &captured,
                        PROCESS_KILL_SIGNAL,
                        &mut cleanup_errors,
                    ),
                    Err(_) => {
                        cleanup_errors.push("프로세스 scope fallback 조회 시간 초과".into());
                        signal_captured_processes(
                            &captured,
                            PROCESS_KILL_SIGNAL,
                            &mut cleanup_errors,
                        );
                    }
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

pub(crate) async fn terminate(child: Child, scope: ProcessScope) -> Result<(), String> {
    tokio::spawn(terminate_owned(child, scope))
        .await
        .map_err(|error| format!("프로세스 정리 작업 대기 실패: {error}"))?
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    fn macos_identity(pid: u32, unique_id: u64, pid_version: u32) -> ProcessIdentity {
        let mut audit_token = AuditToken { value: [0; 8] };
        audit_token.value[5] = pid;
        audit_token.value[7] = pid_version;
        ProcessIdentity {
            pid,
            audit_token,
            unique_id,
            pid_version,
            parent_unique_id: 0,
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_identity_requires_unique_id_and_pid_version_for_the_same_generation() {
        let captured = macos_identity(42, 7, 3);

        assert!(captured.matches_current_generation(Some(&macos_identity(42, 7, 3))));
        assert!(!captured.matches_current_generation(Some(&macos_identity(42, 8, 3))));
        assert!(!captured.matches_current_generation(Some(&macos_identity(42, 7, 4))));
        assert!(!captured.matches_current_generation(Some(&macos_identity(43, 7, 3))));
        assert!(!captured.matches_current_generation(None));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_generation_revalidation_detects_a_reaped_process() {
        let mut command = Command::new("sleep");
        command.arg("5").kill_on_drop(true);
        let mut child = command.spawn().expect("spawn generation probe");
        let pid = child.id().expect("generation probe pid");
        let captured = ProcessIdentity::capture(pid)
            .expect("capture live generation")
            .expect("live generation exists");

        assert!(
            !captured
                .captured_generation_is_gone()
                .expect("revalidate live generation")
        );
        child.start_kill().expect("kill generation probe");
        child.wait().await.expect("reap generation probe");
        assert!(
            captured
                .captured_generation_is_gone()
                .expect("revalidate reaped generation")
        );
    }

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

    #[tokio::test]
    async fn captured_candidate_survives_cancellation_before_later_discovery() {
        let mut command = Command::new("sleep");
        command.arg("5");
        let mut child = command.spawn().expect("spawn cancellation fixture");
        let pid = child.id().expect("fixture process id");
        let mut captured = Vec::new();
        let mut visible = Vec::new();
        let mut errors = Vec::new();

        let cancelled = tokio::time::timeout(Duration::from_millis(20), async {
            capture_and_quiesce_candidate_identities(
                vec![pid, pid],
                None,
                &mut captured,
                &mut visible,
                &mut errors,
            );
            std::future::pending::<()>().await;
        })
        .await;

        assert!(cancelled.is_err(), "later discovery must be cancelled");
        assert_eq!(visible, [pid]);
        assert!(errors.is_empty(), "{}", errors.join("; "));
        assert_eq!(
            captured.len(),
            1,
            "captured identity was lost on cancellation"
        );
        signal_captured_processes(&captured, PROCESS_KILL_SIGNAL, &mut errors);
        child.wait().await.expect("reap cancellation fixture");
        assert!(errors.is_empty(), "{}", errors.join("; "));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn marker_fd_scan_keeps_discovery_permit_after_owner_cancellation() {
        let discovery_permits = Arc::new(tokio::sync::Semaphore::new(2));
        let mut permits = Arc::clone(&discovery_permits)
            .acquire_many_owned(2)
            .await
            .expect("reserve both discovery permits");
        let scan_permit = permits.split(1).expect("split marker scan permit");
        let operation = Arc::new(tokio::sync::Mutex::new(()));
        let operation_guard = Arc::clone(&operation).lock_owned().await;
        let (scan_started, started) = tokio::sync::oneshot::channel();
        let (release_scan, release) = std::sync::mpsc::sync_channel(0);
        let (scan_sender, scan_receiver) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(async move {
            let ownership = (operation_guard, Arc::new(scan_permit));
            let scan = match spawn_marker_fd_scan(ownership, move || {
                scan_started.send(()).expect("report scan start");
                release.recv().expect("release marker scan");
            }) {
                Ok(scan) => scan,
                Err(_) => panic!("marker scan thread must start"),
            };
            scan_sender.send(scan).expect("return scan handle");
            std::future::pending::<()>().await;
        });
        let scan = scan_receiver.await.expect("receive scan handle");
        started.await.expect("marker scan started");

        owner.abort();
        assert!(
            owner
                .await
                .expect_err("owner must be cancelled")
                .is_cancelled(),
            "owner cancellation was not observed"
        );

        assert!(
            discovery_permits.try_acquire().is_err(),
            "reserved permits must exclude unrelated discovery"
        );
        assert!(
            operation.try_lock().is_err(),
            "blocking marker scan returned its operation guard"
        );
        drop(permits);
        let available = discovery_permits
            .try_acquire()
            .expect("the unrelated permit must become available");
        assert!(
            discovery_permits.try_acquire().is_err(),
            "blocking marker scan returned its permit before completing"
        );
        drop(available);

        release_scan.send(()).expect("release marker scan");
        scan.await.expect("marker scan completed");
        let all_permits = discovery_permits
            .try_acquire_many(2)
            .expect("marker scan released its permit after completion");
        drop(all_permits);
        assert!(operation.try_lock().is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_marker_scan_does_not_block_current_or_later_fallbacks() {
        let probe = CleanupProbe::with_discovery_capacity(1);
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & wait"]);
        let (child, scope) = spawn_scoped_with_probe(&mut command, probe)
            .await
            .expect("spawn fallback fixture");
        let root = child.id().expect("fallback fixture root");
        let descendant = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(pid) = descendant_pids(root)
                    .await
                    .expect("discover fallback fixture descendant")
                    .into_iter()
                    .next()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fallback fixture descendant appeared");

        let operation = try_discovery_operation(&scope.discovery_operation)
            .expect("reserve marker scan operation");
        let (scan_started, started) = tokio::sync::oneshot::channel();
        let (release_scan, release) = std::sync::mpsc::sync_channel(0);
        let scan = spawn_marker_fd_scan(
            (
                Arc::clone(&operation),
                Arc::clone(&scope.discovery_reservation),
            ),
            move || {
                scan_started.send(()).expect("report marker scan start");
                release.recv().expect("release timed-out marker scan");
            },
        )
        .unwrap_or_else(|_| panic!("marker scan thread must start"));
        started.await.expect("marker scan started");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), scan)
                .await
                .is_err(),
            "marker scan receiver must time out"
        );
        drop(operation);

        let mut captured = Vec::new();
        let mut cleanup_errors = Vec::new();
        let visible = tokio::time::timeout(
            Duration::from_secs(1),
            capture_scoped_identities(&scope, Some(root), &mut captured, &mut cleanup_errors),
        )
        .await
        .expect("current-round fallbacks stayed bounded");
        assert!(visible.contains(&descendant));
        assert!(captured.iter().any(|identity| identity.pid == descendant));
        assert!(cleanup_errors.is_empty(), "{}", cleanup_errors.join("; "));

        assert!(
            try_discovery_operation(&scope.discovery_operation).is_none(),
            "timed-out marker scan released the operation early"
        );
        let later = tokio::time::timeout(
            Duration::from_secs(1),
            discover_scope_pids(&scope, Some(root), &mut cleanup_errors),
        )
        .await
        .expect("later-round fallbacks did not await the marker scan");
        assert!(later.contains(&descendant));
        assert!(cleanup_errors.is_empty(), "{}", cleanup_errors.join("; "));

        signal_captured_processes(&captured, PROCESS_KILL_SIGNAL, &mut cleanup_errors);
        assert!(cleanup_errors.is_empty(), "{}", cleanup_errors.join("; "));
        let candidate = captured
            .iter()
            .find(|identity| identity.pid == descendant)
            .expect("captured fallback candidate");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if candidate
                    .captured_generation_is_gone()
                    .expect("revalidate killed fallback candidate")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fallback candidate was killed while marker scan remained blocked");
        assert!(
            try_discovery_operation(&scope.discovery_operation).is_none(),
            "candidate signaling released the marker scan operation"
        );

        release_scan.send(()).expect("release marker scan");
        let released = tokio::time::timeout(
            Duration::from_secs(1),
            Arc::clone(&scope.discovery_operation).lock_owned(),
        )
        .await
        .expect("marker scan released its operation after returning");
        drop(released);
        terminate(child, scope)
            .await
            .expect("cleanup fallback fixture");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn marker_fd_scan_start_failure_returns_ownership() {
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::clone(&permits)
            .acquire_owned()
            .await
            .expect("reserve discovery capacity");
        let operation = Arc::new(tokio::sync::Mutex::new(()));
        let operation_guard = Arc::clone(&operation).lock_owned().await;

        let error =
            match spawn_marker_fd_scan_inner((operation_guard, Arc::new(permit)), || (), true) {
                Ok(_) => panic!("forced marker scan start failure must fail"),
                Err(error) => error,
            };
        assert!(error.ownership.is_some(), "thread start lost ownership");
        assert_eq!(error.error.kind(), std::io::ErrorKind::Other);
        assert!(permits.try_acquire().is_err());
        assert!(operation.try_lock().is_err());

        drop(error);
        assert!(permits.try_acquire().is_ok());
        assert!(operation.try_lock().is_ok());
    }

    #[tokio::test]
    async fn spawn_waits_for_discovery_reservation_before_side_effects() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-process-pre-spawn-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let side_effect = root.join("SPAWNED");
        let probe = CleanupProbe::with_discovery_capacity(1);
        let held = probe
            .discovery_permits()
            .acquire_owned()
            .await
            .expect("hold spawn reservation");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf spawned > \"$1\"; sleep 30",
            "rafikx-pre-spawn",
            side_effect.to_str().expect("side effect path"),
        ]);
        let spawn = tokio::spawn({
            let probe = Arc::clone(&probe);
            async move { spawn_scoped_with_probe(&mut command, probe).await }
        });
        tokio::time::timeout(Duration::from_secs(1), probe.wait_for_spawn_reservation())
            .await
            .expect("spawn reached reservation");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!side_effect.exists());

        drop(held);
        let (child, scope) = tokio::time::timeout(Duration::from_secs(2), spawn)
            .await
            .expect("spawn acquired reservation")
            .expect("spawn task joined")
            .expect("spawn succeeded");
        tokio::time::timeout(Duration::from_secs(1), async {
            while !side_effect.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("spawn side effect observed");
        terminate(child, scope)
            .await
            .expect("cleanup spawned fixture");
        assert!(side_effect.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn closed_reservation_and_spawn_failure_release_capacity() {
        let closed_probe = CleanupProbe::with_discovery_capacity(1);
        closed_probe.discovery_permits().close();
        let mut closed_command = Command::new("sh");
        closed_command.args(["-c", "exit 0"]);
        let closed = spawn_scoped_with_probe(&mut closed_command, closed_probe)
            .await
            .expect_err("closed reservation must reject spawn");
        assert_eq!(closed.kind(), std::io::ErrorKind::Other);

        let failed_probe = CleanupProbe::with_discovery_capacity(1);
        let permits = failed_probe.discovery_permits();
        let mut missing = Command::new("/rafikx/does-not-exist");
        spawn_scoped_with_probe(&mut missing, failed_probe)
            .await
            .expect_err("missing executable must fail");
        assert_eq!(permits.available_permits(), 1);
    }

    #[tokio::test]
    async fn cleanup_admission_queues_sessions_after_discovery() {
        // Given: cleanup admission is saturated while physical discovery remains available.
        let cleanup_admission_permits = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
        let discovery_permits = std::sync::Arc::new(tokio::sync::Semaphore::new(3));
        let held_admission = cleanup_admission_permits
            .clone()
            .acquire_many_owned(2)
            .await
            .expect("hold both cleanup admission permits");
        let mut cleanups = Vec::new();
        let mut probes = Vec::new();
        for _ in 0..3 {
            let mut command = Command::new("sleep");
            command.arg("30");
            let probe = std::sync::Arc::new(CleanupProbe {
                cleanup_admission_permits: Some(cleanup_admission_permits.clone()),
                discovery_permits: Some(discovery_permits.clone()),
                ..CleanupProbe::default()
            });
            let (child, scope) = spawn_scoped_with_probe(&mut command, probe.clone())
                .await
                .expect("spawn saturated cleanup fixture");
            let cleanup = tokio::spawn(terminate(child, scope));
            tokio::time::timeout(Duration::from_secs(1), probe.worker_started.notified())
                .await
                .expect("cleanup worker started");
            tokio::time::timeout(
                Duration::from_secs(2),
                probe.scope_discovery_started.notified(),
            )
            .await
            .expect("scope discovery started before admission");
            cleanups.push(cleanup);
            probes.push(probe);
        }

        // Then: every session quiesces its scope before any can enter saturated admission.
        tokio::task::yield_now().await;
        for probe in &probes {
            assert_eq!(
                probe
                    .cleanup_admission_calls
                    .load(std::sync::atomic::Ordering::SeqCst),
                0,
                "cleanup entered saturated admission before scope discovery"
            );
        }

        drop(held_admission);
        for cleanup in cleanups {
            tokio::time::timeout(Duration::from_secs(20), cleanup)
                .await
                .expect("saturated cleanup completed")
                .expect("cleanup caller joined")
                .expect("saturated cleanup succeeded");
        }
        for probe in &probes {
            assert_eq!(
                probe
                    .cleanup_admission_calls
                    .load(std::sync::atomic::Ordering::SeqCst),
                1,
                "queued cleanup never acquired admission"
            );
        }
    }

    #[tokio::test]
    async fn queued_cleanup_quiesces_retained_marker_before_admission() {
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file());
        let Some(python) = python else { return };

        let root = std::env::temp_dir().join(format!(
            "rafikx-process-pre-admission-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let ready = root.join("ready");
        let survived = root.join("DESCENDANT_SURVIVED");
        let script = "import os,sys,time\nos.environ.clear()\nchild=os.fork()\nif child == 0:\n os.setsid()\n if os.fork() > 0: os._exit(0)\n for fd in (0,1,2):\n  try: os.close(fd)\n  except OSError: pass\n open(sys.argv[1], 'w').write(str(os.getpid()))\n time.sleep(1)\n open(sys.argv[2], 'w').write('survived')\n time.sleep(60)\nos._exit(0)";
        let mut command = Command::new("env");
        command.args([
            "-u",
            PROCESS_SCOPE_ENV,
            python,
            "-c",
            script,
            ready.to_str().expect("ready path"),
            survived.to_str().expect("survived path"),
        ]);
        let cleanup_admission_permits = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
        let held_admission = cleanup_admission_permits
            .clone()
            .acquire_many_owned(2)
            .await
            .expect("hold cleanup admission");
        let probe = std::sync::Arc::new(CleanupProbe {
            cleanup_admission_permits: Some(cleanup_admission_permits),
            discovery_permits: Some(std::sync::Arc::new(tokio::sync::Semaphore::new(1))),
            ..CleanupProbe::default()
        });
        let (child, scope) = spawn_scoped_with_probe(&mut command, probe.clone())
            .await
            .expect("spawn admission fixture");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("detached daemon ready");

        let cleanup = tokio::spawn(terminate(child, scope));
        tokio::time::timeout(Duration::from_secs(1), probe.worker_started.notified())
            .await
            .expect("cleanup worker started");
        tokio::time::timeout(Duration::from_secs(2), probe.scope_quiesced.notified())
            .await
            .expect("retained-marker descendants quiesced before admission");
        assert_eq!(
            probe
                .cleanup_admission_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "cleanup admission preceded retained-marker quiescence"
        );

        tokio::time::sleep(Duration::from_millis(1300)).await;
        let side_effect_happened = survived.exists();
        drop(held_admission);
        tokio::time::timeout(Duration::from_secs(12), cleanup)
            .await
            .expect("queued cleanup completed")
            .expect("cleanup caller joined")
            .expect("queued cleanup succeeded");
        let _ = std::fs::remove_dir_all(root);

        assert!(
            !side_effect_happened,
            "retained-marker descendant ran while cleanup waited for admission"
        );
        assert_eq!(
            probe
                .cleanup_admission_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn cleanup_uses_its_reserved_capacity_for_every_discovery_round() {
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file());
        let Some(python) = python else { return };

        // Given: this scope owns one permit while unrelated work occupies every other permit.
        let root = std::env::temp_dir().join(format!(
            "rafikx-process-admission-budget-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let ready = root.join("ready");
        let script = "import os,sys,time\nos.environ.clear()\nchild=os.fork()\nif child == 0:\n os.setsid()\n if os.fork() > 0: os._exit(0)\n for fd in (0,1,2):\n  try: os.close(fd)\n  except OSError: pass\n open(sys.argv[1], 'w').write(str(os.getpid()))\n time.sleep(60)\nos._exit(0)";
        let mut command = Command::new("env");
        command.args([
            "-u",
            PROCESS_SCOPE_ENV,
            python,
            "-c",
            script,
            ready.to_str().expect("ready path"),
        ]);
        let discovery_permits = std::sync::Arc::new(tokio::sync::Semaphore::new(3));
        let probe = std::sync::Arc::new(CleanupProbe {
            discovery_permits: Some(discovery_permits.clone()),
            ..CleanupProbe::default()
        });
        let (child, scope) = spawn_scoped_with_probe(&mut command, probe.clone())
            .await
            .expect("spawn admission fixture");
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
        let permits = discovery_permits
            .acquire_many(2)
            .await
            .expect("hold unrelated discovery capacity");
        let cleanup = tokio::spawn(terminate(child, scope));
        tokio::time::timeout(Duration::from_secs(1), probe.worker_started.notified())
            .await
            .expect("cleanup worker started");

        // When: cleanup performs initial, revalidation, and final discovery.
        let cleanup_result = tokio::time::timeout(Duration::from_secs(12), cleanup)
            .await
            .expect("admitted cleanup completed")
            .expect("cleanup caller joined");

        // Then: no round reacquires global capacity and the exact retained-marker daemon dies.
        let gone = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let output = std::process::Command::new("ps")
                    .args(["-p", &daemon_pid.to_string(), "-o", "stat="])
                    .output()
                    .expect("inspect admission-budget daemon");
                let state = String::from_utf8_lossy(&output.stdout);
                if !output.status.success()
                    || state.trim().is_empty()
                    || state.trim().starts_with('Z')
                {
                    break true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or(false);
        if !gone {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &daemon_pid.to_string()])
                .status();
        }
        let _ = std::fs::remove_dir_all(root);
        assert!(
            cleanup_result.is_ok(),
            "cleanup deadline started before admission: {cleanup_result:?}"
        );
        assert_eq!(
            probe
                .scope_discovery_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            3,
            "cleanup repeated a redundant full discovery round"
        );
        assert!(gone, "admitted cleanup stranded detached PID {daemon_pid}");
        drop(permits);
    }

    #[tokio::test]
    async fn caller_cancellation_while_queued_does_not_strand_descendants() {
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file());
        let Some(python) = python else { return };

        // Given: an environment-cleared descendant whose cleanup is queued for admission.
        let root = std::env::temp_dir().join(format!(
            "rafikx-process-queued-cancel-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let ready = root.join("ready");
        let script = "import os,sys,time\nos.environ.clear()\nchild=os.fork()\nif child == 0:\n os.setsid()\n if os.fork() > 0: os._exit(0)\n for fd in (0,1,2):\n  try: os.close(fd)\n  except OSError: pass\n open(sys.argv[1], 'w').write(str(os.getpid()))\n time.sleep(60)\nos._exit(0)";
        let mut command = Command::new("env");
        command.args([
            "-u",
            PROCESS_SCOPE_ENV,
            python,
            "-c",
            script,
            ready.to_str().expect("ready path"),
        ]);
        let cleanup_admission_permits = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let held_admission = cleanup_admission_permits
            .clone()
            .acquire_owned()
            .await
            .expect("hold cleanup admission");
        let probe = std::sync::Arc::new(CleanupProbe {
            cleanup_admission_permits: Some(cleanup_admission_permits),
            discovery_permits: Some(std::sync::Arc::new(tokio::sync::Semaphore::new(1))),
            ..CleanupProbe::default()
        });
        let (child, scope) = spawn_scoped_with_probe(&mut command, probe.clone())
            .await
            .expect("spawn cancellation fixture");
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
        let scope_marker = scope.marker_path.clone();
        let caller = tokio::spawn(terminate(child, scope));
        tokio::time::timeout(Duration::from_secs(1), probe.worker_started.notified())
            .await
            .expect("cleanup worker started");
        tokio::time::timeout(Duration::from_secs(2), probe.scope_quiesced.notified())
            .await
            .expect("cleanup quiesced before admission");

        // When: the awaiting caller is cancelled while the owned worker is queued.
        caller.abort();
        assert!(
            caller
                .await
                .expect_err("caller must be cancelled")
                .is_cancelled(),
            "cleanup caller did not observe cancellation"
        );

        // Then: the detached worker retains the scope and completes descendant cleanup.
        assert!(
            scope_marker.exists(),
            "queued worker dropped its process scope"
        );
        drop(held_admission);
        let gone = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let output = std::process::Command::new("ps")
                    .args(["-p", &daemon_pid.to_string(), "-o", "stat="])
                    .output()
                    .expect("inspect queued-cancellation daemon");
                let state = String::from_utf8_lossy(&output.stdout);
                if !output.status.success()
                    || state.trim().is_empty()
                    || state.trim().starts_with('Z')
                {
                    break true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or(false);
        if !gone {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &daemon_pid.to_string()])
                .status();
        }
        assert!(
            gone,
            "caller cancellation stranded detached PID {daemon_pid}"
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while scope_marker.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("cleanup worker released process scope");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn capacity_one_cleanup_finishes_before_the_next_spawn() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-process-capacity-one-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let spawned = root.join("B_SPAWNED");
        let discovery_permits = Arc::new(tokio::sync::Semaphore::new(1));
        let cleanup_admission_permits = Arc::new(tokio::sync::Semaphore::new(1));
        let held_admission = Arc::clone(&cleanup_admission_permits)
            .acquire_owned()
            .await
            .expect("hold cleanup admission");
        let probe_a = Arc::new(CleanupProbe {
            cleanup_admission_permits: Some(cleanup_admission_permits),
            discovery_permits: Some(Arc::clone(&discovery_permits)),
            ..CleanupProbe::default()
        });
        let mut command_a = Command::new("sleep");
        command_a.arg("30");
        let (child_a, scope_a) = spawn_scoped_with_probe(&mut command_a, Arc::clone(&probe_a))
            .await
            .expect("spawn scope A");
        let cleanup_a = tokio::spawn(terminate(child_a, scope_a));
        tokio::time::timeout(Duration::from_secs(2), probe_a.scope_quiesced.notified())
            .await
            .expect("scope A quiesced");

        let probe_b = Arc::new(CleanupProbe {
            discovery_permits: Some(discovery_permits),
            ..CleanupProbe::default()
        });
        let mut command_b = Command::new("sh");
        command_b.args([
            "-c",
            "printf spawned > \"$1\"; sleep 30",
            "rafikx-capacity-one",
            spawned.to_str().expect("spawn marker path"),
        ]);
        let spawn_b = tokio::spawn({
            let probe_b = Arc::clone(&probe_b);
            async move { spawn_scoped_with_probe(&mut command_b, probe_b).await }
        });
        tokio::time::timeout(Duration::from_secs(1), probe_b.wait_for_spawn_reservation())
            .await
            .expect("scope B queued for reservation");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!spawned.exists(), "scope B spawned before A cleanup");

        drop(held_admission);
        cleanup_a
            .await
            .expect("scope A cleanup joined")
            .expect("scope A cleanup succeeded");
        let (child_b, scope_b) = tokio::time::timeout(Duration::from_secs(2), spawn_b)
            .await
            .expect("scope B acquired released reservation")
            .expect("scope B task joined")
            .expect("scope B spawned");
        tokio::time::timeout(Duration::from_secs(1), async {
            while !spawned.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("scope B side effect observed");
        terminate(child_b, scope_b).await.expect("cleanup scope B");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn scoped_process_drop_cleans_detached_retained_marker_descendant() {
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file());
        let Some(python) = python else { return };
        let root = std::env::temp_dir().join(format!(
            "rafikx-process-owner-drop-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let ready = root.join("ready");
        let survived = root.join("DESCENDANT_SURVIVED");
        let script = "import os,sys,time\nos.environ.clear()\nchild=os.fork()\nif child == 0:\n os.setsid()\n if os.fork() > 0: os._exit(0)\n for fd in (0,1,2):\n  try: os.close(fd)\n  except OSError: pass\n open(sys.argv[1], 'w').write(str(os.getpid()))\n time.sleep(2)\n open(sys.argv[2], 'w').write('survived')\n time.sleep(60)\nos._exit(0)";
        let mut command = Command::new("env");
        command.args([
            "-u",
            PROCESS_SCOPE_ENV,
            python,
            "-c",
            script,
            ready.to_str().expect("ready path"),
            survived.to_str().expect("survived path"),
        ]);
        let (child, scope) = spawn_scoped(&mut command)
            .await
            .expect("spawn owner-drop fixture");
        let owner = tokio::spawn(async move {
            let _process = ScopedProcess::new(child, scope);
            std::future::pending::<()>().await;
        });
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

        owner.abort();
        assert!(owner.await.expect_err("owner cancelled").is_cancelled());
        tokio::time::sleep(Duration::from_millis(2300)).await;
        let side_effect_happened = survived.exists();
        if side_effect_happened {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &daemon_pid.to_string()])
                .status();
        }
        let _ = std::fs::remove_dir_all(root);
        assert!(
            !side_effect_happened,
            "dropping scoped ownership stranded detached PID {daemon_pid}"
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
        let (mut child, scope) = spawn_scoped(&mut command)
            .await
            .expect("spawn completed command");
        child.wait().await.expect("wait completed command");
        let started = std::time::Instant::now();

        terminate(child, scope)
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
        let (child, scope) = spawn_scoped(&mut command)
            .await
            .expect("spawn scoped child");

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
                terminate(child, scope).await.expect("terminate child");
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

        terminate(child, scope).await.expect("terminate child");
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
        let (child, scope) = spawn_scoped(&mut command)
            .await
            .expect("spawn process tree");
        tokio::time::sleep(Duration::from_millis(50)).await;
        terminate(child, scope).await.expect("terminate tree");
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
        let (child, scope) = spawn_scoped(&mut command)
            .await
            .expect("spawn detached process");
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
        terminate(child, scope)
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
    async fn final_discovery_keeps_environment_only_reparented_members() {
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file());
        let Some(python) = python else { return };
        let root = std::env::temp_dir().join(format!(
            "rafikx-process-env-only-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let ready = root.join("ready");
        let script = "import os,sys,time\nchild=os.fork()\nif child == 0:\n os.setsid()\n if os.fork() > 0: os._exit(0)\n os.closerange(3, 1024)\n open(sys.argv[1], 'w').write(str(os.getpid()))\n time.sleep(60)\nos._exit(0)";
        let mut command = Command::new(python);
        command.args(["-c", script, ready.to_str().expect("ready path")]);
        let (mut child, scope) = spawn_scoped(&mut command)
            .await
            .expect("spawn env-only process");
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
        .expect("environment-only daemon ready");
        let _ = child.wait().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut cleanup_errors = Vec::new();
        let discovered = discover_scope_pids(&scope, None, &mut cleanup_errors).await;

        let _ = std::process::Command::new("kill")
            .args(["-KILL", &daemon_pid.to_string()])
            .status();
        let _ = std::fs::remove_dir_all(root);
        assert!(cleanup_errors.is_empty(), "{}", cleanup_errors.join("; "));
        assert!(
            discovered.contains(&daemon_pid),
            "environment-only PID {daemon_pid} was invisible to final discovery: {discovered:?}"
        );
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
        let ready = root.join("ready");
        let script = "import os,sys,time\nos.environ.clear()\nchild=os.fork()\nif child == 0:\n os.setsid()\n if os.fork() > 0: os._exit(0)\n for fd in (0,1,2):\n  try: os.close(fd)\n  except OSError: pass\n open(sys.argv[1], 'w').write(str(os.getpid()))\n time.sleep(60)\nos._exit(0)";
        let mut command = Command::new("env");
        command.args([
            "-u",
            PROCESS_SCOPE_ENV,
            python,
            "-c",
            script,
            ready.to_str().expect("ready path"),
        ]);
        let (child, scope) = spawn_scoped(&mut command)
            .await
            .expect("spawn clearenv process");
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
        .expect("environment-cleared daemon ready");
        // Linux 의 marker-fd 조회는 discovery operation·예약을 함께 소비한다 — 블록을
        // 벗어나며 둘 다 반환돼야 뒤따르는 terminate 의 조회가 다시 잠글 수 있다.
        #[cfg(target_os = "linux")]
        let inherited = {
            let operation = try_discovery_operation(&scope.discovery_operation)
                .expect("reserve inherited-scope discovery operation");
            inherited_scope_pids(&scope, operation, Arc::clone(&scope.discovery_reservation)).await
        };
        #[cfg(not(target_os = "linux"))]
        let inherited = inherited_scope_pids(&scope).await;
        let inherited = inherited.expect("discover retained scope descriptor");
        assert!(
            inherited.contains(&daemon_pid),
            "daemon PID {daemon_pid} did not retain the scope descriptor: {inherited:?}"
        );
        terminate(child, scope)
            .await
            .expect("terminate clearenv tree");
        let gone = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let output = std::process::Command::new("ps")
                    .args(["-p", &daemon_pid.to_string(), "-o", "stat="])
                    .output()
                    .expect("inspect environment-cleared daemon");
                let state = String::from_utf8_lossy(&output.stdout);
                if !output.status.success()
                    || state.trim().is_empty()
                    || state.trim().starts_with('Z')
                {
                    break true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or(false);
        if !gone {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &daemon_pid.to_string()])
                .status();
        }
        assert!(gone, "environment-cleared daemon survived cleanup");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cleanup_timeout_kills_already_captured_detached_descendant() {
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file());
        let Some(python) = python else { return };
        let root = std::env::temp_dir().join(format!(
            "rafikx-process-timeout-fallback-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("test directory");
        let ready = root.join("ready");
        let script = "import os,sys,time\nos.environ.clear()\nchild=os.fork()\nif child == 0:\n os.setsid()\n if os.fork() > 0: os._exit(0)\n for fd in (0,1,2):\n  try: os.close(fd)\n  except OSError: pass\n open(sys.argv[1], 'w').write(str(os.getpid()))\n time.sleep(60)\nos._exit(0)";
        let mut command = Command::new("env");
        command.args([
            "-u",
            PROCESS_SCOPE_ENV,
            python,
            "-c",
            script,
            ready.to_str().expect("ready path"),
        ]);
        let (child, mut scope) = spawn_scoped(&mut command)
            .await
            .expect("spawn timeout fixture");
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
        scope.force_cleanup_timeout = true;
        let error = terminate(child, scope)
            .await
            .expect_err("forced cleanup timeout must fail closed");
        assert!(error.contains("전체 시간 초과"), "{error}");

        let gone = tokio::time::timeout(Duration::from_secs(2), async {
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
                    break true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or(false);
        if !gone {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &daemon_pid.to_string()])
                .status();
        }
        assert!(gone, "captured detached daemon survived cleanup timeout");
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
        let (child, mut scope) = spawn_scoped(&mut command)
            .await
            .expect("spawn process tree");
        scope.force_discovery_failure = true;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let error = terminate(child, scope)
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
        let (child, scope) = spawn_scoped(&mut command)
            .await
            .expect("spawn Windows process tree");
        tokio::time::sleep(Duration::from_millis(200)).await;
        terminate(child, scope).await.expect("terminate job");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
