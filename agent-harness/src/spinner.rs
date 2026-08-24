//! 실행 중임을 눈에 보이게 하는 디지털 진행 표시.
//! braille 스피너 + 슬라이딩 진행바 + 경과 초 + 라벨.
//! stderr 전용이라 응답 스트림(stdout)과 섞이지 않으며,
//! TUI/데스크탑(live sink 활성)에서는 완전히 무작동한다.

use std::io::IsTerminal;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const BAR_WIDTH: usize = 16;
const SEGMENT: usize = 5;

/// 답변 출력(Chunk/Assistant)이 시작되면 스피너는 임무를 마친다.
static ANSWER_STARTED: AtomicBool = AtomicBool::new(false);

pub fn mark_answer_started() {
    ANSWER_STARTED.store(true, Ordering::Relaxed);
}

fn answer_started() -> bool {
    ANSWER_STARTED.load(Ordering::Relaxed)
}

/// 스피너 구동 중 라벨 갱신 (agent 루프의 반복·도구 정보 등).
fn label_slot() -> &'static Mutex<Option<String>> {
    static SLOT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

pub fn set_label(msg: &str) {
    if let Ok(mut g) = label_slot().lock() {
        let trimmed: String = msg.chars().take(56).collect();
        *g = Some(trimmed);
    }
}

/// 현재 라벨 — TUI 진행바가 단계 상태를 표시할 때 읽는다.
pub fn current_label() -> Option<String> {
    label_slot().lock().ok().and_then(|g| g.clone())
}

/// 스피너 구동 중 들어온 시스템 메시지 버퍼 — 종료 때 한 번에 출력해 유실 없다.
fn notes_slot() -> &'static Mutex<Vec<String>> {
    static SLOT: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn push_note(line: &str) {
    if let Ok(mut g) = notes_slot().lock() {
        g.push(line.to_string());
    }
}

pub fn drain_notes() -> Vec<String> {
    notes_slot()
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

static RUNNING: AtomicBool = AtomicBool::new(false);

fn stderr_is_terminal() -> bool {
    std::io::stderr().is_terminal()
}

#[cfg(windows)]
fn enable_stderr_vt() {
    type Handle = *mut std::ffi::c_void;
    #[allow(non_snake_case)]
    unsafe extern "system" {
        fn GetStdHandle(n: i32) -> Handle;
        fn GetConsoleMode(h: Handle, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: Handle, mode: u32) -> i32;
    }
    const STD_ERROR_HANDLE: i32 = -12;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    unsafe {
        let h = GetStdHandle(STD_ERROR_HANDLE);
        if h.is_null() || h == (-1isize as Handle) {
            return;
        }
        let mut mode = 0u32;
        if GetConsoleMode(h, &mut mode) != 0 {
            let _ = SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn enable_stderr_vt() {}

fn render_bar(tick: usize) -> String {
    // 슬라이딩 세그먼트 — 총량을 모르는 작업용 인디케이터
    let span = BAR_WIDTH - SEGMENT + 1;
    let pos = tick % (span * 2);
    let pos = if pos < span {
        pos
    } else {
        span * 2 - 1 - pos
    };
    let mut bar = String::with_capacity(BAR_WIDTH * 3);
    bar.push('[');
    for i in 0..BAR_WIDTH {
        let lit = i >= pos && i < pos + SEGMENT;
        bar.push_str(if lit { "█" } else { "░" });
    }
    bar.push(']');
    bar
}

pub struct Spinner {
    stop: Option<Arc<AtomicBool>>,
    handle: Option<tokio::task::JoinHandle<()>>,
    active: bool,
}

impl Spinner {
    pub fn start(msg: &str) -> Self {
        let active = stderr_is_terminal()
            && !crate::ui::live_active()
            && std::env::var_os("NO_COLOR").is_none();
        if !active {
            return Self {
                stop: None,
                handle: None,
                active: false,
            };
        }
        ANSWER_STARTED.store(false, Ordering::Relaxed);
        RUNNING.store(true, Ordering::Relaxed);
        enable_stderr_vt();
        set_label(msg);
        if let Ok(mut g) = notes_slot().lock() {
            g.clear();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let handle = tokio::spawn(async move {
            let mut err = std::io::stderr();
            let started = Instant::now();
            let mut i = 0usize;
            loop {
                if stop_t.load(Ordering::Relaxed) || answer_started() {
                    break;
                }
                let elapsed = started.elapsed().as_secs();
                let label = label_slot()
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .unwrap_or_default();
                let _ = write!(
                    err,
                    "\r\x1b[36m{}\x1b[0m {} {:>3}초 {} ",
                    FRAMES[i % FRAMES.len()],
                    render_bar(i),
                    elapsed,
                    label
                );
                let _ = err.flush();
                i += 1;
                tokio::time::sleep(Duration::from_millis(90)).await;
            }
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        });
        Self {
            stop: Some(stop),
            handle: Some(handle),
            active: true,
        }
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn finish(mut self) {
        self.stop_internal();
    }

    fn stop_internal(&mut self) {
        if !self.active {
            return;
        }
        if let Some(f) = self.stop.take() {
            f.store(true, Ordering::Relaxed);
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await_ok();
        }
        // 버퍼된 시스템 메시지를 유실 없이 출력
        for line in drain_notes() {
            println!("{line}");
        }
        RUNNING.store(false, Ordering::Relaxed);
        self.active = false;
    }
}

trait AwaitOk {
    fn await_ok(self);
}
impl AwaitOk for tokio::task::JoinHandle<()> {
    fn await_ok(self) {
        // drop 만으로 중단 신호가 전달되므로 대기하지 않는다 (abort 위임).
        self.abort();
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop_internal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_braille() {
        assert!(FRAMES.len() >= 8);
    }

    #[test]
    fn bar_has_fixed_width_and_moving_segment() {
        let a = render_bar(0);
        let b = render_bar(1);
        assert_eq!(a.chars().count(), BAR_WIDTH + 2);
        assert_ne!(a, b);
        assert!(a.starts_with('[') && a.ends_with(']'));
    }

    #[test]
    fn inactive_without_terminal() {
        let s = Spinner::start("테스트");
        assert!(!s.active());
        assert!(!is_running());
    }

    #[test]
    fn label_roundtrip() {
        set_label("반복 2/25");
        assert_eq!(
            label_slot().lock().unwrap().as_deref(),
            Some("반복 2/25")
        );
        *label_slot().lock().unwrap() = None;
    }
}
