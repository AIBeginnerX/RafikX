//! 실행 중임을 눈에 보이게 하는 디지털 진행 표시.
//! braille 스피너 + 슬라이딩 진행바 + 경과 초 + 라벨.
//! stderr 전용이라 응답 스트림(stdout)과 섞이지 않으며,
//! TUI/데스크탑(live sink 활성)에서는 완전히 무작동한다.

use std::io::IsTerminal;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::run::RunContext;

const FRAMES: &[char] = &['|', '/', '-', '\\'];
const BAR_WIDTH: usize = 16;
const SEGMENT: usize = 5;

/// 답변 출력(Chunk/Assistant)이 시작되면 스피너는 임무를 마친다.
static ANSWER_STARTED: AtomicBool = AtomicBool::new(false);

pub fn mark_answer_started() {
    ANSWER_STARTED.store(true, Ordering::Relaxed);
}

pub fn mark_answer_started_in(run: &RunContext) {
    run.progress().mark_answer_started();
}

fn answer_started() -> bool {
    ANSWER_STARTED.load(Ordering::Relaxed)
}

/// 스피너 구동 중 라벨 갱신 (agent 루프의 반복·도구 정보 등).
/// 라벨과 그 라벨이 걸린 시각 — 단계별 경과를 세기 위해 함께 보관한다.
fn label_slot() -> &'static Mutex<Option<(String, Instant)>> {
    static SLOT: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

pub fn set_label(msg: &str) {
    if let Ok(mut g) = label_slot().lock() {
        let trimmed: String = msg.chars().take(56).collect();
        *g = Some((trimmed, Instant::now()));
    }
}

pub fn set_label_in(run: &RunContext, msg: &str) {
    run.progress().set_label(msg);
}

/// 현재 라벨 — TUI 진행바가 단계 상태를 표시할 때 읽는다.
pub fn current_label() -> Option<String> {
    label_slot()
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(label, _)| label.clone()))
}

/// 단계 경과 표기 — 60초 미만은 "42s", 그 이상은 "1m24s".
pub(crate) fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

/// 스피너 한 줄에 실리는 라벨 — 지금 단계가 얼마나 이어졌는지를 함께 보인다.
/// 같은 라벨이 오래 머무르면 그 자체로 "무엇을 하는 중인지"의 답이 된다.
pub(crate) fn label_with_elapsed(label: &str, secs: u64) -> String {
    if label.is_empty() {
        return String::new();
    }
    format!("{label} · {}", format_elapsed(secs))
}

/// 전역 스피너가 그리는 라벨 (경과 포함).
fn label_display() -> String {
    label_slot()
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .map(|(label, at)| label_with_elapsed(label, at.elapsed().as_secs()))
        })
        .unwrap_or_default()
}

pub fn current_label_in(run: &RunContext) -> Option<String> {
    run.progress().label()
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

pub fn push_note_in(run: &RunContext, line: &str) {
    run.progress().push_note(line);
}

pub fn drain_notes() -> Vec<String> {
    notes_slot()
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

pub fn drain_notes_in(run: &RunContext) -> Vec<String> {
    run.progress().drain_notes()
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

pub fn is_running_in(run: &RunContext) -> bool {
    run.progress().is_running()
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
    let pos = if pos < span { pos } else { span * 2 - 1 - pos };
    let mut bar = String::with_capacity(BAR_WIDTH * 3);
    bar.push('[');
    for i in 0..BAR_WIDTH {
        let lit = i >= pos && i < pos + SEGMENT;
        bar.push_str(if lit { "#" } else { "-" });
    }
    bar.push(']');
    bar
}

pub struct Spinner {
    stop: Option<Arc<AtomicBool>>,
    handle: Option<tokio::task::JoinHandle<()>>,
    active: bool,
    run: Option<RunContext>,
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
                run: None,
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
                let label = label_display();
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
            run: None,
        }
    }

    pub fn start_in(run: RunContext, msg: &str) -> Self {
        let active =
            stderr_is_terminal() && !run.has_live_sink() && std::env::var_os("NO_COLOR").is_none();
        if !active {
            return Self {
                stop: None,
                handle: None,
                active: false,
                run: Some(run),
            };
        }
        run.progress().reset();
        run.progress().set_running(true);
        run.progress().set_label(msg);
        enable_stderr_vt();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let run_t = run.clone();
        let handle = tokio::spawn(async move {
            let mut err = std::io::stderr();
            let started = Instant::now();
            let mut i = 0usize;
            loop {
                if stop_t.load(Ordering::Relaxed) || run_t.progress().answer_started() {
                    break;
                }
                let elapsed = started.elapsed().as_secs();
                let label = run_t.progress().label_display();
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
            run: Some(run),
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
            h.await_ok();
        }
        let notes = self
            .run
            .as_ref()
            .map(drain_notes_in)
            .unwrap_or_else(drain_notes);
        for line in notes {
            println!("{line}");
        }
        if let Some(run) = &self.run {
            run.progress().set_running(false);
        } else {
            RUNNING.store(false, Ordering::Relaxed);
        }
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
    fn frames_are_font_safe_ascii() {
        assert!(FRAMES.len() >= 4);
        assert!(FRAMES.iter().all(|c| c.is_ascii()));
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
        assert_eq!(current_label().as_deref(), Some("반복 2/25"));
        // 라벨을 걸자마자는 0초 — 경과가 라벨 뒤에 붙는다.
        assert!(label_display().starts_with("반복 2/25 · "));
        *label_slot().lock().unwrap() = None;
        assert!(label_display().is_empty());
    }

    #[test]
    fn elapsed_switches_to_minutes_after_a_minute() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(42), "42s");
        assert_eq!(format_elapsed(59), "59s");
        assert_eq!(format_elapsed(60), "1m00s");
        assert_eq!(format_elapsed(84), "1m24s");
        assert_eq!(format_elapsed(3_600), "60m00s");
    }

    #[test]
    fn label_gets_elapsed_suffix_unless_empty() {
        assert_eq!(
            label_with_elapsed("반복 3/25 · 모델 호출", 84),
            "반복 3/25 · 모델 호출 · 1m24s"
        );
        assert_eq!(label_with_elapsed("검증 중…", 42), "검증 중… · 42s");
        // 라벨이 없으면 경과만 떠 있는 줄을 만들지 않는다.
        assert!(label_with_elapsed("", 42).is_empty());
    }
}
