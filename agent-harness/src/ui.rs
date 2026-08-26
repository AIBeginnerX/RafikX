use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::run::{RunContext, RunEventKind};

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
    println!(
        "{}",
        gold("+--------------------------------------------------------------+")
    );
    println!(
        "{} {} {}",
        gold("|"),
        pad_visible(
            &format!(
                "{}  {}",
                gold("RAFIKX"),
                dim(&format!("v{}", env!("CARGO_PKG_VERSION")))
            ),
            WIDTH - 2,
        ),
        gold("|")
    );
    if !subtitle.is_empty() {
        println!(
            "{} {} {}",
            gold("|"),
            pad_visible(&dim(subtitle), WIDTH - 2),
            gold("|")
        );
    }
    println!(
        "{}",
        gold("+--------------------------------------------------------------+")
    );
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
    println!("{} {}", cyan(">"), bold(title));
}

pub fn ok(msg: &str) {
    if live_active() {
        live_line(msg);
        return;
    }
    println!("  {} {}", green("+"), msg);
}

pub fn warn(msg: &str) {
    if emit(Live::Warn(msg.to_string())) {
        return;
    }
    println!("  {} {}", yellow("!"), msg);
}

pub fn fail(msg: &str) {
    if emit(Live::Warn(msg.to_string())) {
        return;
    }
    println!("  {} {}", red("x"), msg);
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Live {
    Chunk(String),
    Assistant(String),
    System(String),
    Warn(String),
    Status(String),
    Todo(Vec<crate::tools_more::TodoItem>),
    Agent(AgentProgress),
    /// 이번 턴의 실행 축 한 줄 (engine·team·discipline·self·gate) — working 패널 마지막 줄.
    Mode(String),
}

/// working 패널의 한 줄 — 실행 중인 에이전트 하나.
/// `role`·`model` 이 빈 문자열이면 **갱신만** 하겠다는 뜻이라 수신 측이 기존 값을 유지한다
/// (agent.rs 는 프로파일 이름을 모르므로 activity 만 갱신한다).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentProgress {
    pub id: String,
    pub role: String,
    pub model: String,
    /// 지금 하는 일 한 조각 — "반복 3/25", "[도구] read_file" 등.
    #[serde(default)]
    pub activity: String,
    /// true 면 이 줄을 지운다.
    #[serde(default)]
    pub done: bool,
}

pub type LiveFn = Arc<dyn Fn(Live) + Send + Sync>;

fn live_slot() -> &'static Mutex<Option<LiveFn>> {
    static SLOT: OnceLock<Mutex<Option<LiveFn>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

pub fn set_live(sink: Option<LiveFn>) {
    if let Ok(mut g) = live_slot().lock() {
        *g = sink;
    }
}

pub fn current_live_sink() -> Option<LiveFn> {
    live_slot().lock().ok().and_then(|sink| sink.clone())
}

pub fn live_active() -> bool {
    live_slot().lock().ok().and_then(|g| g.clone()).is_some()
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

fn emit_in(run: &RunContext, event: Live) -> bool {
    run.emit(
        RunEventKind::Live,
        serde_json::to_value(&event).unwrap_or_else(|_| json!({"type":"unknown"})),
    );
    run.emit_live(event.clone()) || emit(event)
}

pub fn live_chunk_in(run: &RunContext, s: &str) {
    if s.is_empty() {
        return;
    }
    run.progress().mark_answer_started();
    if !emit_in(run, Live::Chunk(s.to_string())) {
        print!("{s}");
        let _ = io::stdout().flush();
    }
}

pub fn live_assistant_in(run: &RunContext, s: &str) {
    if s.is_empty() {
        return;
    }
    run.progress().mark_answer_started();
    if !emit_in(run, Live::Assistant(s.to_string())) {
        println!("{s}");
    }
}

pub fn live_line_in(run: &RunContext, s: &str) {
    if run.progress().is_running() {
        run.progress().push_note(s);
        run.progress().set_label(s);
        return;
    }
    if !emit_in(run, Live::System(s.to_string())) {
        println!("{s}");
    }
}

pub fn live_status_in(run: &RunContext, s: &str) {
    if run.progress().is_running() {
        run.progress().set_label(s);
        return;
    }
    if !emit_in(run, Live::Status(s.to_string())) {
        note(s);
    }
}

pub fn live_warn_in(run: &RunContext, s: &str) {
    if run.progress().is_running() {
        let note = format!("⚠ {s}");
        run.progress().push_note(&note);
        run.progress().set_label(s);
        return;
    }
    if !emit_in(run, Live::Warn(s.to_string())) {
        warn(s);
    }
}

pub fn live_todo_in(run: &RunContext, items: &[crate::tools_more::TodoItem]) {
    let _ = emit_in(run, Live::Todo(items.to_vec()));
}

pub fn live_agent_in(run: &RunContext, progress: AgentProgress) {
    let _ = emit_in(run, Live::Agent(progress));
}

/// working 패널의 워커 줄 id — 위임 자식은 agent_id, 루트 실행은 run_id.
/// task.rs 와 파이프라인이 같은 규칙을 쓰므로 같은 실행은 항상 한 줄로 합쳐진다.
pub fn worker_id(run: &RunContext) -> String {
    run.agent_id()
        .map(ToString::to_string)
        .unwrap_or_else(|| run.run_id().to_string())
}

/// working 패널 한 줄을 보내는 단일 진입점. `role`·`model` 이 비면 기존 값을 유지한다.
pub fn live_worker_in(
    run: &RunContext,
    id: &str,
    role: &str,
    model: &str,
    activity: &str,
    done: bool,
) {
    live_agent_in(
        run,
        AgentProgress {
            id: id.to_string(),
            role: role.to_string(),
            model: model.to_string(),
            activity: activity.to_string(),
            done,
        },
    );
}

/// 이번 턴의 실행 축 한 줄. TUI 가 없으면 조용히 버려진다 (CLI 는 print_binding 이 이미 알린다).
pub fn live_mode(s: &str) {
    let _ = emit(Live::Mode(s.to_string()));
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

pub fn live_todo(items: &[crate::tools_more::TodoItem]) {
    let _ = emit(Live::Todo(items.to_vec()));
}

pub fn live_agent(progress: AgentProgress) {
    let _ = emit(Live::Agent(progress));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_progress_reads_records_written_before_the_working_panel() {
        // 구 형식: activity·done 이 없고 지금은 안 쓰는 status 가 들어 있다.
        let old = r#"{"type":"agent","payload":{"id":"agent-1","role":"reviewer","model":"minimax-m3","status":"running"}}"#;
        let Live::Agent(progress) = serde_json::from_str::<Live>(old).expect("구 JSON 역직렬화")
        else {
            panic!("Live::Agent 여야 한다");
        };
        assert_eq!(progress.id, "agent-1");
        assert_eq!(progress.role, "reviewer");
        assert_eq!(progress.model, "minimax-m3");
        assert_eq!(progress.activity, "");
        assert!(!progress.done, "구 기록은 살아 있는 줄로 읽혀야 한다");
    }

    #[test]
    fn mode_line_survives_a_round_trip() {
        let json = serde_json::to_string(&Live::Mode("engine=minimax(고정)".into())).unwrap();
        let Live::Mode(s) = serde_json::from_str::<Live>(&json).unwrap() else {
            panic!("Live::Mode 여야 한다");
        };
        assert_eq!(s, "engine=minimax(고정)");
    }
}
