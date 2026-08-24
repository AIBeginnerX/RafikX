use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex, OnceLock};

static COLOR: OnceLock<bool> = OnceLock::new();

pub fn init() {
    enable_windows_vt();
    let on = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let _ = COLOR.set(on);
}

fn color() -> bool {
    *COLOR.get_or_init(|| io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

fn c(code: &str, s: &str) -> String {
    if color() {
        format!("{code}{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn gold(s: &str) -> String {
    c("\x1b[1;38;5;180m", s)
}
pub fn dim(s: &str) -> String {
    c("\x1b[2m", s)
}
pub fn cyan(s: &str) -> String {
    c("\x1b[36m", s)
}
pub fn green(s: &str) -> String {
    c("\x1b[32m", s)
}
pub fn yellow(s: &str) -> String {
    c("\x1b[33m", s)
}
pub fn red(s: &str) -> String {
    c("\x1b[31m", s)
}
pub fn bold(s: &str) -> String {
    c("\x1b[1m", s)
}

const WIDTH: usize = 64;

pub fn rule() {
    println!("{}", dim(&"─".repeat(WIDTH)));
}

pub fn banner(subtitle: &str) {
    init();
    println!();
    println!("{}", gold("╭──────────────────────────────────────────────────────────────╮"));
    println!(
        "{} {} {}",
        gold("│"),
        pad_visible(
            &format!("{}  {}", gold("RAFIKX"), dim(&format!("v{}", env!("CARGO_PKG_VERSION")))),
            WIDTH - 2,
        ),
        gold("│")
    );
    if !subtitle.is_empty() {
        println!(
            "{} {} {}",
            gold("│"),
            pad_visible(&dim(subtitle), WIDTH - 2),
            gold("│")
        );
    }
    println!("{}", gold("╰──────────────────────────────────────────────────────────────╯"));
}

fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        n += 1;
    }
    n
}

fn pad_visible(s: &str, width: usize) -> String {
    let n = visible_len(s);
    if n >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - n))
}

pub fn section(title: &str) {
    println!();
    println!("{} {}", cyan("▸"), bold(title));
}

pub fn ok(msg: &str) {
    if live_active() {
        live_line(msg);
        return;
    }
    println!("  {} {}", green("●"), msg);
}

pub fn warn(msg: &str) {
    if emit(Live::Warn(msg.to_string())) {
        return;
    }
    println!("  {} {}", yellow("●"), msg);
}

pub fn fail(msg: &str) {
    if emit(Live::Warn(msg.to_string())) {
        return;
    }
    println!("  {} {}", red("●"), msg);
}

pub fn note(msg: &str) {
    if live_active() {
        live_line(msg);
        return;
    }
    println!("  {} {}", dim("·"), msg);
}

pub fn print_footer() {
    if live_active() {
        return;
    }
    let lines = crate::usage::footer_lines();
    if lines.is_empty() {
        return;
    }
    println!();
    rule();
    for line in lines {
        println!("  {line}");
    }
    rule();
    let _ = io::stdout().flush();
}

/// Agent/TUI live output. When a sink is installed, prints go there instead of stdout.
#[derive(Clone, Debug)]
pub enum Live {
    Chunk(String),
    Assistant(String),
    System(String),
    Warn(String),
    Status(String),
}

type LiveFn = Arc<dyn Fn(Live) + Send + Sync>;

fn live_slot() -> &'static Mutex<Option<LiveFn>> {
    static SLOT: OnceLock<Mutex<Option<LiveFn>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

pub fn set_live(sink: Option<LiveFn>) {
    if let Ok(mut g) = live_slot().lock() {
        *g = sink;
    }
}

pub fn live_active() -> bool {
    live_slot()
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .is_some()
}

fn emit(ev: Live) -> bool {
    let f = live_slot().lock().ok().and_then(|g| g.clone());
    if let Some(f) = f {
        f(ev);
        true
    } else {
        false
    }
}

pub fn live_chunk(s: &str) {
    if s.is_empty() {
        return;
    }
    crate::spinner::mark_answer_started();
    if !emit(Live::Chunk(s.to_string())) {
        print!("{s}");
        let _ = io::stdout().flush();
    }
}

pub fn live_assistant(s: &str) {
    if s.is_empty() {
        return;
    }
    crate::spinner::mark_answer_started();
    if !emit(Live::Assistant(s.to_string())) {
        println!("{s}");
    }
}

pub fn live_line(s: &str) {
    // 스피너 구동 중에는 줄을 버퍼링해 진행바를 깨뜨리지 않고, 종료 때 출력한다.
    if crate::spinner::is_running() {
        crate::spinner::push_note(s);
        crate::spinner::set_label(s);
        return;
    }
    if !emit(Live::System(s.to_string())) {
        println!("{s}");
    }
}

pub fn live_status(s: &str) {
    if crate::spinner::is_running() {
        crate::spinner::set_label(s);
        return;
    }
    if !emit(Live::Status(s.to_string())) {
        note(s);
    }
}

pub fn live_warn(s: &str) {
    if crate::spinner::is_running() {
        let note = format!("⚠ {s}");
        crate::spinner::push_note(&note);
        crate::spinner::set_label(s);
        return;
    }
    if !emit(Live::Warn(s.to_string())) {
        warn(s);
    }
}

/// True when stdin or stdout is a pipe or redirected file (`echo hi | rafikx`).
/// A normal Windows cmd / PowerShell / Windows Terminal console is not piped,
/// even if `IsTerminal` returns false.
pub fn stdio_is_piped() -> bool {
    stdio_redirected(StdIo::In) || stdio_redirected(StdIo::Out)
}

/// Launch the session UI unless stdio is clearly piped/redirected.
pub fn want_interactive_ui() -> bool {
    !stdio_is_piped()
}

enum StdIo {
    In,
    Out,
}

#[cfg(windows)]
fn stdio_redirected(which: StdIo) -> bool {
    const STD_INPUT_HANDLE: i32 = -10;
    const STD_OUTPUT_HANDLE: i32 = -11;
    const FILE_TYPE_DISK: u32 = 0x0001;
    const FILE_TYPE_PIPE: u32 = 0x0003;
    let n = match which {
        StdIo::In => STD_INPUT_HANDLE,
        StdIo::Out => STD_OUTPUT_HANDLE,
    };
    type Handle = *mut std::ffi::c_void;
    unsafe extern "system" {
        fn GetStdHandle(n: i32) -> Handle;
        fn GetFileType(h: Handle) -> u32;
    }
    unsafe {
        let h = GetStdHandle(n);
        if h.is_null() || h == (-1isize as Handle) {
            return false;
        }
        let t = GetFileType(h);
        t == FILE_TYPE_PIPE || t == FILE_TYPE_DISK
    }
}

#[cfg(not(windows))]
fn stdio_redirected(which: StdIo) -> bool {
    match which {
        StdIo::In => !io::stdin().is_terminal(),
        StdIo::Out => !io::stdout().is_terminal(),
    }
}

#[cfg(windows)]
fn enable_windows_vt() {
    type Handle = *mut std::ffi::c_void;
    unsafe extern "system" {
        fn GetStdHandle(n: i32) -> Handle;
        fn GetConsoleMode(h: Handle, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: Handle, mode: u32) -> i32;
    }
    const STD_OUTPUT_HANDLE: i32 = -11;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if h.is_null() || h == (-1isize as Handle) {
            return;
        }
        let mut mode = 0u32;
        if GetConsoleMode(h, &mut mode) == 0 {
            return;
        }
        let _ = SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
    }
}

#[cfg(not(windows))]
fn enable_windows_vt() {}
