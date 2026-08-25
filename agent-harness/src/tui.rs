mod md;
mod view;

use std::io::{stdout, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    MouseButton, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::execute;
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use tokio::sync::{mpsc, oneshot};

use crate::agent::{ApprovalChoice, LocalAsk};
use crate::chat::{self, Session, Slash};
use crate::config::Config;
use crate::db::Db;
use crate::provider::{ContentBlock, Message, Role};
use crate::ui::{self, Live};

pub(super) const STATUS_MARK: &str = " N·A·I ";
pub(super) const STATUS_NAME: &str = "RAFIKX";
pub(super) const STATUS_DIVIDER: &str = " │";
pub(super) const APPROVAL_YES: &str = " [Yes]";
pub(super) const APPROVAL_NO: &str = " [No]";
pub(super) const APPROVAL_ALWAYS: &str = " [Always]";

pub struct App {
    session: Session,
    pub transcript: Vec<Entry>,
    pub input: String,
    pub cursor: usize,
    pub scroll: u16,
    pub follow: bool,
    pub busy: bool,
    pub help: bool,
    pub status: String,
    pub tokens: String,
    /// 풋터 표시용 컨텍스트·캐시 요약 문자열
    pub ctx: String,
    pub binding: String,
    pub cwd: String,
    pub approval: Option<ApprovalPrompt>,
    pub picker: Option<Picker>,
    pub secret: Option<SecretPrompt>,
    pub text: Option<TextPrompt>,
    pub confirm: Option<ConfirmPrompt>,
    history: Vec<String>,
    history_idx: Option<usize>,
    streaming: bool,
    quit: bool,
    quit_after: bool,
    /// 슬래시 명령 Enter 2회 확인용
    pub slash_armed: bool,
    /// 실행 중 접수한 대기 프롬프트 큐 — 턴이 끝나면 순차 실행
    pub queue: Vec<String>,
    /// 현재 실행의 Todo 목록 — 하단 진행 패널에 즉시 반영된다.
    pub todos: Vec<crate::tools_more::TodoItem>,
    /// 실행 중이거나 끝난 서브에이전트 역할·모델·상태.
    pub agents: Vec<crate::ui::AgentProgress>,
    /// 완료 요약 화면이 활성화되면 직전 실행의 늦은 진행 이벤트를 버린다.
    final_summary: bool,
    /// 현재 실행 중인 턴의 핸들 — Esc 인터럽트용
    pub turn_handle: Option<tokio::task::JoinHandle<()>>,
    /// 새 릴리스 태그 — 있으면 U 키로 업그레이드 진행 가능
    pub upgrade: Option<String>,
    /// 업그레이드 진행 중 플래그
    pub upgrading: bool,
}

pub struct ApprovalPrompt {
    pub preview: String,
    tx: oneshot::Sender<ApprovalChoice>,
}

#[derive(Clone, Copy)]
pub(super) struct ApprovalButton {
    pub rect: Rect,
    pub choice: ApprovalChoice,
    pub label: &'static str,
}

pub struct Picker {
    pub title: String,
    pub items: Vec<String>,
    pub ids: Vec<String>,
    pub selected: usize,
    pub kind: PickerKind,
    pub target: Option<String>,
    /// 타이핑 검색어 (대소문자 무시 부분 일치)
    pub query: String,
}

/// 검색어로 걸러진 항목의 원본 인덱스 목록.
fn picker_visible(picker: &Picker) -> Vec<usize> {
    let q = picker.query.trim().to_lowercase();
    picker
        .items
        .iter()
        .enumerate()
        .filter(|(_, it)| q.is_empty() || it.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerKind {
    Model,
    Manage,
    Action,
}

pub struct SecretPrompt {
    pub provider: String,
    pub buf: String,
}

pub struct TextPrompt {
    pub title: String,
    pub hint: String,
    pub buf: String,
    pub kind: TextKind,
}

#[derive(Clone, Debug)]
pub enum TextKind {
    BaseUrl { provider: String },
    Model { provider: String },
    CustomName,
    CustomUrl { name: String },
    CustomModel { name: String, base_url: String },
}

pub struct ConfirmPrompt {
    pub title: String,
    pub body: String,
    pub provider: String,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub kind: EntryKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    User,
    /// 실행 대기 중인 프롬프트 — 턴 시작 시 User 로 승격된다.
    Queued,
    Assistant,
    System,
    Tool,
    Warn,
}

struct TermGuard;

static KITTY_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Drop for TermGuard {
    fn drop(&mut self) {
        ui::set_live(None);
        let _ = disable_raw_mode();
        let mut out = stdout();
        if KITTY_ENABLED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let _ = execute!(out, PopKeyboardEnhancementFlags);
        }
        {
            use std::io::Write as _;
            let _ = write!(out, "\x1b[?1007l");
            let _ = out.flush();
        }
        let _ = execute!(
            out,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = out.flush();
    }
}

pub async fn run(
    cfg: Config,
    yes: bool,
    provider: Option<String>,
    model: Option<String>,
    class: Option<String>,
    resume: Option<String>,
) -> Result<()> {
    if crate::ui::stdio_is_piped() {
        anyhow::bail!("대화 화면은 터미널에서만 엽니다");
    }
    enable_utf8();

    let session = chat::open_session(cfg, yes, provider, model, class, resume, false)?;
    let cwd = session.cfg.workspace.display().to_string();
    let mut app = App {
        transcript: hydrate(&session.messages),
        binding: binding_label(&session),
        cwd,
        session,
        input: String::new(),
        cursor: 0,
        scroll: 0,
        follow: true,
        busy: false,
        help: false,
        status: "준비".into(),
        tokens: String::new(),
        ctx: String::new(),
        approval: None,
        picker: None,
        secret: None,
        text: None,
        confirm: None,
        history: Vec::new(),
        history_idx: None,
        streaming: false,
        quit: false,
        quit_after: false,
        slash_armed: false,
        queue: Vec::new(),
        todos: crate::tools_more::current_todos(),
        agents: Vec::new(),
        final_summary: false,
        turn_handle: None,
        upgrade: None,
        upgrading: false,
    };
    if app.transcript.is_empty() {
        app.transcript.push(Entry {
            kind: EntryKind::System,
            text: "RafikX 대화. /connect 로 키를 붙입니다. Ctrl+V 붙여넣기.".into(),
        });
    }

    // 새 릴리스 확인 — 네트워크 조회가 시작을 막지 않도록 백그라운드로 돌린다.
    let (upd_tx, mut upd_rx) = mpsc::unbounded_channel::<String>();
    // 동기 조회(git/gh)라 별도 스레드에서 — 시작을 막지 않는다.
    std::thread::spawn(move || match crate::update::latest_release() {
        Ok(rel) => {
            let current = env!("CARGO_PKG_VERSION");
            if let Some(notice) = crate::update::upgrade_notice(&rel, current) {
                let _ = upd_tx.send(notice);
            } else {
                let _ = upd_tx.send(format!(
                    "버전 확인: 최신입니다 (v{current})"
                ));
            }
        }
        Err(e) => {
            let _ = upd_tx.send(format!("버전 확인 실패: {e:#}"));
        }
    });

    let (live_tx, mut live_rx) = mpsc::unbounded_channel::<Live>();
    ui::set_live(Some(Arc::new(move |ev| {
        let _ = live_tx.send(ev);
    })));

    let (ask_tx, mut ask_rx) = mpsc::unbounded_channel::<(String, oneshot::Sender<ApprovalChoice>)>();
    let local_ask: LocalAsk = {
        let ask_tx = ask_tx.clone();
        Arc::new(move |preview: String| {
            let ask_tx = ask_tx.clone();
            Box::pin(async move {
                let (tx, rx) = oneshot::channel();
                if ask_tx.send((preview, tx)).is_err() {
                    return ApprovalChoice::No;
                }
                rx.await.unwrap_or(ApprovalChoice::No)
            })
        })
    };

    enable_raw_mode().context("raw mode")?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableBracketedPaste)
    .context("alternate screen")?;
    // Kitty 키보드 프로토콜 — 지원 터미널(Ghostty·Kitty·WezTerm 등)에서
    // Shift+Enter·Ctrl+Enter 같은 변형키 조합을 정확한 이벤트로 받는다.
    // 미지원 터미널은 기존 xterm 이스케이프로 폴백한다.
    let kitty_keys = matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    );
    KITTY_ENABLED.store(kitty_keys, std::sync::atomic::Ordering::SeqCst);
    if kitty_keys {
        execute!(
            out,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        )
        .ok();
    }
    // Alternate Scroll (DECSET 1007) — 대체 화면에서 마우스 휠을 ↑↓ 키로 변환.
    // 마우스 캡처를 하지 않으므로 드래그 선택·복사는 그대로 유지된다.
    // crossterm 에 1007 모드가 없어 raw escape 로 직접 켠다.
    {
        use std::io::Write as _;
        let _ = write!(out, "\x1b[?1007h");
        let _ = out.flush();
    }
    let _guard = TermGuard;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(80));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<TurnDone>();
    if let Ok(path) = Db::db_path()
        && let Ok(db) = Db::open(&path)
        && let Ok(Some(goal)) = db.active_goal()
        && goal.continuations < 8
    {
        if let Ok(messages) = serde_json::from_str::<Vec<Message>>(&goal.messages_json) {
            app.session.messages = messages;
        }
        push(
            &mut app,
            EntryKind::System,
            format!(
                "중단된 목표를 자동 재개합니다: {} (Todo {}/{})",
                goal.objective, goal.completed, goal.total
            ),
        );
        start_turn(
            &mut app,
            goal.objective,
            Some("dev".into()),
            false,
            &local_ask,
            &done_tx,
        );
    }
    let mut dirty = true;

    loop {
        if app.quit {
            break;
        }
        if dirty {
            terminal.draw(|f| view::draw(f, &app))?;
            dirty = false;
        }

        tokio::select! {
            _ = tick.tick() => {
                dirty = true;
            }
            ev = events.next() => {
                match ev {
                    Some(Ok(Event::Key(k))) => {
                        if k.kind == KeyEventKind::Release {
                            continue;
                        }
                        dirty = true;
                        handle_key(&mut app, k, &local_ask, &done_tx);
                    }
                    Some(Ok(Event::Paste(text))) => {
                        handle_paste(&mut app, &text);
                        dirty = true;
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        let size = terminal.size().unwrap_or_default();
                        handle_mouse(&mut app, mouse, size.width, size.height);
                        dirty = true;
                    }
                    Some(Ok(Event::Resize(_, _))) => dirty = true,
                    Some(Err(e)) => anyhow::bail!("입력 이벤트: {e}"),
                    None => break,
                    _ => {}
                }
            }
            live = live_rx.recv() => {
                if let Some(ev) = live {
                    apply_live(&mut app, ev);
                    dirty = true;
                }
            }
            upd = upd_rx.recv() => {
                if let Some(notice) = upd {
                    app.upgrade = crate::update::last_seen_tag();
                    push(&mut app, EntryKind::Warn, notice);
                    dirty = true;
                }
            }
            ask = ask_rx.recv() => {
                if let Some((preview, tx)) = ask {
                    set_mouse_capture(true);
                    app.approval = Some(ApprovalPrompt { preview, tx });
                    app.help = false;
                    dirty = true;
                }
            }
            done = done_rx.recv() => {
                if let Some(done) = done {
                    finish_turn(&mut app, done, &local_ask, &done_tx);
                    dirty = true;
                    if app.quit_after {
                        app.quit = true;
                    }
                }
            }
        }
    }

    drop(terminal);
    drop(_guard);
    if let Some(id) = chat::save_if_dirty(&mut app.session)? {
        eprintln!("세션을 저장했습니다: {id}");
    }
    Ok(())
}

struct TurnDone {
    result: Result<chat::TurnInfo, anyhow::Error>,
    session: Session,
}

fn handle_key(
    app: &mut App,
    k: KeyEvent,
    local_ask: &LocalAsk,
    done_tx: &mpsc::UnboundedSender<TurnDone>,
) {
    if let Some(prompt) = app.approval.take() {
        match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                set_mouse_capture(false);
                let _ = prompt.tx.send(ApprovalChoice::Yes);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                set_mouse_capture(false);
                let _ = prompt.tx.send(ApprovalChoice::No);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                set_mouse_capture(false);
                let _ = prompt.tx.send(ApprovalChoice::Always);
            }
            _ => {
                app.approval = Some(prompt);
            }
        }
        return;
    }

    let ctrl_early = k.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl_early && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('C')) {
        if app.busy {
            app.quit_after = true;
            app.status = "이번 턴이 끝나면 종료합니다".into();
        } else {
            app.quit = true;
        }
        return;
    }

    if let Some(c) = app.confirm.take() {
        match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                confirm_yes(app, c);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.status = "삭제를 취소했습니다".into();
            }
            _ => {
                app.confirm = Some(c);
            }
        }
        return;
    }

    if let Some(mut secret) = app.secret.take() {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => {
                set_mouse_capture(true);
                app.status = "연결을 취소했습니다".into();
            }
            KeyCode::Enter => {
                finish_secret(app, secret);
            }
            KeyCode::Backspace => {
                secret.buf.pop();
                app.secret = Some(secret);
            }
            KeyCode::Char('u') | KeyCode::Char('U') if ctrl => {
                secret.buf.clear();
                app.secret = Some(secret);
            }
            KeyCode::Char('v') | KeyCode::Char('V') if ctrl => {
                if let Some(clip) = read_clipboard() {
                    secret
                        .buf
                        .push_str(&crate::accounts_ui::sanitize_pasted_key(&clip));
                }
                app.secret = Some(secret);
            }
            KeyCode::Char(c) if !ctrl && c != '\n' && c != '\r' => {
                secret.buf.push(c);
                app.secret = Some(secret);
            }
            _ => {
                app.secret = Some(secret);
            }
        }
        return;
    }

    if let Some(mut text) = app.text.take() {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => {
                set_mouse_capture(true);
                app.status = "입력을 취소했습니다".into();
            }
            KeyCode::Enter => finish_text(app, text),
            KeyCode::Backspace => {
                text.buf.pop();
                app.text = Some(text);
            }
            KeyCode::Char('u') | KeyCode::Char('U') if ctrl => {
                text.buf.clear();
                app.text = Some(text);
            }
            KeyCode::Char('v') | KeyCode::Char('V') if ctrl => {
                if let Some(clip) = read_clipboard() {
                    text.buf
                        .push_str(&crate::accounts_ui::sanitize_pasted_key(&clip));
                }
                app.text = Some(text);
            }
            KeyCode::Char(c) if !ctrl && c != '\n' && c != '\r' => {
                text.buf.push(c);
                app.text = Some(text);
            }
            _ => {
                app.text = Some(text);
            }
        }
        return;
    }

    if let Some(mut picker) = app.picker.take() {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let vis = picker_visible(&picker);
        let vis_len = vis.len();
        // 선택 커서를 필터된 목록 기준으로 보정
        if picker.selected >= vis_len {
            picker.selected = vis_len.saturating_sub(1);
        }
        match k.code {
            KeyCode::Esc => {
                app.status = "선택을 취소했습니다".into();
            }
            KeyCode::Up => {
                if picker.selected > 0 {
                    picker.selected -= 1;
                }
                app.picker = Some(picker);
            }
            KeyCode::Down => {
                if picker.selected + 1 < vis_len {
                    picker.selected += 1;
                }
                app.picker = Some(picker);
            }
            KeyCode::Enter => {
                if vis.is_empty() {
                    app.status = "일치하는 항목이 없습니다".into();
                    app.picker = Some(picker);
                } else {
                    picker.selected = vis[picker.selected];
                    apply_picker(app, picker);
                }
            }
            KeyCode::Backspace => {
                picker.query.pop();
                picker.selected = 0;
                app.picker = Some(picker);
            }
            KeyCode::Char('e') | KeyCode::Char('E')
                if ctrl && picker.kind == PickerKind::Manage =>
            {
                if vis_len > 0 {
                    picker.selected = vis[picker.selected];
                }
                if let Some(id) = picker.ids.get(picker.selected).cloned() {
                    if !id.is_empty() && id != CUSTOM_ID {
                        open_secret(app, &id);
                    } else {
                        app.picker = Some(picker);
                    }
                } else {
                    app.picker = Some(picker);
                }
            }
            KeyCode::Char('d') | KeyCode::Char('D')
                if ctrl && picker.kind == PickerKind::Manage =>
            {
                if vis_len > 0 {
                    picker.selected = vis[picker.selected];
                }
                if let Some(id) = picker.ids.get(picker.selected).cloned() {
                    if !id.is_empty() && id != CUSTOM_ID {
                        ask_disconnect(app, &id);
                    } else {
                        app.picker = Some(picker);
                    }
                } else {
                    app.picker = Some(picker);
                }
            }
            KeyCode::Char(c) if !ctrl => {
                // 타이핑으로 목록을 검색한다
                picker.query.push(c);
                picker.selected = 0;
                app.picker = Some(picker);
            }
            _ => {
                app.picker = Some(picker);
            }
        }
        return;
    }

    if app.help {
        if matches!(k.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter) {
            app.help = false;
        }
        return;
    }

    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);

    match k.code {
        KeyCode::PageUp => {
            app.follow = false;
            app.scroll = app.scroll.saturating_add(8);
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_sub(8);
            if app.scroll == 0 {
                app.follow = true;
            }
        }
        KeyCode::Char('u') if ctrl => {
            app.follow = false;
            app.scroll = app.scroll.saturating_add(4);
        }
        KeyCode::Char('d') if ctrl => {
            app.scroll = app.scroll.saturating_sub(4);
            if app.scroll == 0 {
                app.follow = true;
            }
        }
        KeyCode::Esc => {
            if app.help {
                app.help = false;
            } else if app.busy {
                // opencode 스타일 interrupt — 실행 중 Esc 한 번으로 생성을 중단한다.
                if let Some(h) = app.turn_handle.take() {
                    h.abort();
                }
                app.busy = false;
                app.streaming = false;
                app.status = "중단됨".into();
                push(app, EntryKind::Warn, "응답을 중단했습니다. (Esc)");
            }
        }
        KeyCode::Tab | KeyCode::BackTab => {
            // opencode 스타일 — Tab 으로 plan/build 하네스 모드를 전환한다.
            let next = if app.session.is_plan_mode() { "build" } else { "plan" };
            app.session.mode = next.to_string();
            app.binding = binding_label(&app.session);
            app.status = format!("모드: {next}");
        }
        KeyCode::Char('u') | KeyCode::Char('U')
            if app.upgrade.is_some() && !app.upgrading && app.input.is_empty() =>
        {
            start_upgrade(app);
        }
        KeyCode::Char('?') if app.input.is_empty() => {
            app.help = true;
        }
        KeyCode::Enter if shift || ctrl => insert_char(app, '\n'),
        KeyCode::Char('j') if ctrl => insert_char(app, '\n'),
        KeyCode::Enter => {
            // 슬래시 명령은 Enter 두 번으로 실행 (첫 번째는 확인)
            if app.input.trim().starts_with('/') && !app.slash_armed {
                app.slash_armed = true;
                app.status = "슬래시 명령 — 다시 Enter 로 실행".into();
                return;
            }
            app.slash_armed = false;
            submit(app, local_ask, done_tx);
        }
        KeyCode::Backspace => backspace(app),
        KeyCode::Delete => delete(app),
        KeyCode::Left => app.cursor = prev_idx(&app.input, app.cursor),
        KeyCode::Right => app.cursor = next_idx(&app.input, app.cursor),
        KeyCode::Home => app.cursor = 0,
        KeyCode::End => app.cursor = app.input.len(),
        KeyCode::Up if k.modifiers.contains(KeyModifiers::SHIFT) => {
            app.follow = false;
            app.scroll = app.scroll.saturating_add(4);
        }
        KeyCode::Down if k.modifiers.contains(KeyModifiers::SHIFT) => {
            app.scroll = app.scroll.saturating_sub(4);
            if app.scroll == 0 {
                app.follow = true;
            }
        }
        KeyCode::Up if app.input.is_empty() => {
            // 휠(Alternate Scroll)·빈 입력 상태의 ↑ — 지나간 화면 보기
            app.follow = false;
            app.scroll = app.scroll.saturating_add(4);
        }
        KeyCode::Down if app.input.is_empty() => {
            app.scroll = app.scroll.saturating_sub(4);
            if app.scroll == 0 {
                app.follow = true;
            }
        }
        KeyCode::Up => history_prev(app),
        KeyCode::Down => history_next(app),
        KeyCode::Char('/') if app.input.is_empty() => {
            // 명령 목록은 하단 팔레트가 담당 — 트랜스크립트에는 출력하지 않는다.
            insert_char(app, '/');
        }
        KeyCode::Char(c) if !ctrl => insert_char(app, c),
        _ => {}
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent, width: u16, height: u16) {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }
    let model = view::selected_model_label(app);
    let Some(choice) = approval_popup_click(mouse.column, mouse.row, width, height)
        .or_else(|| approval_click(mouse.column, mouse.row, height, &model))
    else {
        return;
    };
    if let Some(prompt) = app.approval.take() {
        set_mouse_capture(false);
        let _ = prompt.tx.send(choice);
    }
}

pub(super) fn approval_popup_layout(area: Rect) -> (Rect, [ApprovalButton; 3]) {
    let popup = view::overlay_rect(area, 72, 22);
    let inner_x = popup.x.saturating_add(1);
    let inner_width = popup.width.saturating_sub(2);
    if inner_width < 23 {
        let first_y = popup
            .y
            .saturating_add(popup.height.saturating_sub(5));
        return (
            popup,
            [
                ApprovalButton {
                    rect: Rect::new(inner_x, first_y, 5.min(inner_width), 1),
                    choice: ApprovalChoice::Yes,
                    label: "[Yes]",
                },
                ApprovalButton {
                    rect: Rect::new(
                        inner_x,
                        first_y.saturating_add(1),
                        4.min(inner_width),
                        1,
                    ),
                    choice: ApprovalChoice::No,
                    label: "[No]",
                },
                ApprovalButton {
                    rect: Rect::new(
                        inner_x,
                        first_y.saturating_add(2),
                        8.min(inner_width),
                        1,
                    ),
                    choice: ApprovalChoice::Always,
                    label: "[Always]",
                },
            ],
        );
    }
    let button_y = popup
        .y
        .saturating_add(popup.height.saturating_sub(3));
    let group_width = 23u16;
    let start = inner_x.saturating_add(inner_width.saturating_sub(group_width) / 2);
    (
        popup,
        [
            ApprovalButton {
                rect: Rect::new(start, button_y, 5, 1),
                choice: ApprovalChoice::Yes,
                label: "[Yes]",
            },
            ApprovalButton {
                rect: Rect::new(start.saturating_add(8), button_y, 4, 1),
                choice: ApprovalChoice::No,
                label: "[No]",
            },
            ApprovalButton {
                rect: Rect::new(start.saturating_add(15), button_y, 8, 1),
                choice: ApprovalChoice::Always,
                label: "[Always]",
            },
        ],
    )
}

fn approval_popup_click(
    column: u16,
    row: u16,
    width: u16,
    height: u16,
) -> Option<ApprovalChoice> {
    let area = Rect::new(0, 0, width, height);
    let (_, buttons) = approval_popup_layout(area);
    buttons
        .into_iter()
        .find(|button| {
            button.rect.contains(ratatui::layout::Position::new(column, row))
        })
        .map(|button| button.choice)
}

fn approval_click(
    column: u16,
    row: u16,
    height: u16,
    model: &str,
) -> Option<ApprovalChoice> {
    if height < 2 || row != height.saturating_sub(2) {
        return None;
    }
    let prefix = format!(
        "{STATUS_MARK}{STATUS_NAME}{STATUS_DIVIDER} MODEL {model} ·"
    );
    let yes_start = crate::tui::md::display_width(&prefix) as u16;
    let no_start = yes_start + crate::tui::md::display_width(APPROVAL_YES) as u16;
    let always_start = no_start + crate::tui::md::display_width(APPROVAL_NO) as u16;
    if (yes_start..no_start).contains(&column) {
        Some(ApprovalChoice::Yes)
    } else if (no_start..always_start).contains(&column) {
        Some(ApprovalChoice::No)
    } else if (always_start
        ..always_start + crate::tui::md::display_width(APPROVAL_ALWAYS) as u16)
        .contains(&column)
    {
        Some(ApprovalChoice::Always)
    } else {
        None
    }
}

fn submit(app: &mut App, local_ask: &LocalAsk, done_tx: &mpsc::UnboundedSender<TurnDone>) {
    let line = app.input.trim().to_string();
    if line.is_empty() {
        return;
    }
    if line == "/" {
        // 명령 없이 "/" 만 — 하단 팔레트가 이미 보고 있으므로 아무것도 실행하지 않는다.
        app.input.clear();
        app.cursor = 0;
        app.slash_armed = false;
        app.status = "명령을 입력하세요".into();
        return;
    }
    app.slash_armed = false;
    if app.busy && !line.starts_with('/') {
        // 실행 중 입력은 큐에 적립 — 현재 턴이 끝나면 순차 실행된다.
        app.queue.push(line.clone());
        push(app, EntryKind::Queued, line);
        app.input.clear();
        app.cursor = 0;
        app.status = format!("실행 중 — 대기 {}건 (완료 후 자동 실행)", app.queue.len());
        return;
    }
    if app.busy && line.starts_with('/') {
        app.status = "실행 중입니다 — 슬래시 명령은 완료 후 사용하세요".into();
        return;
    }
    app.history.push(line.clone());
    app.history_idx = None;
    app.input.clear();
    app.cursor = 0;
    app.follow = true;
    app.scroll = 0;

    if line.starts_with('/') {
        let mut parts = line.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();
        match cmd {
            "/model" | "/models" if rest.is_empty() => {
                open_model_picker(app);
                return;
            }
            "/provider" | "/accounts" => {
                if rest.is_empty() {
                    open_manage_picker(app);
                } else {
                    start_connect(app, rest);
                }
                return;
            }
            "/connect" | "/login" => {
                if rest.is_empty() {
                    open_manage_picker(app);
                } else {
                    start_connect(app, rest);
                }
                return;
            }
            _ => {}
        }
        match chat::handle_slash(&mut app.session, &line, false) {
            Ok(Slash::Continue(notes)) => {
                for n in notes {
                    push(app, EntryKind::System, n);
                }
                app.binding = binding_label(&app.session);
            }
            Ok(Slash::Quit) => {
                if app.busy {
                    app.quit_after = true;
                    app.status = "이번 턴이 끝나면 종료합니다".into();
                } else {
                    app.quit = true;
                }
            }
            Ok(Slash::Agent(task)) => {
                start_turn(app, task, Some("dev".into()), false, local_ask, done_tx);
            }
            Ok(Slash::Compact) => {
                start_compact(app, done_tx);
            }
            Ok(Slash::AssignRoles) => {
                start_assign(app, done_tx);
            }
            Err(e) => push(app, EntryKind::Warn, format!("{e:#}")),
        }
        return;
    }

    let class = app.session.class.clone();
    let obsidian = app.session.obsidian_on;
    start_turn(app, line, class, obsidian, local_ask, done_tx);
}

fn start_turn(
    app: &mut App,
    prompt: String,
    class: Option<String>,
    obsidian: bool,
    local_ask: &LocalAsk,
    done_tx: &mpsc::UnboundedSender<TurnDone>,
) {
    app.final_summary = false;
    promote_queued(app, &prompt);
    app.busy = true;
    app.streaming = false;
    app.status = "실행 중…".into();
    app.binding = binding_label(&app.session);

    let mut session = app.session.clone();
    let local_ask = local_ask.clone();
    let done_tx = done_tx.clone();
    let handle = tokio::spawn(async move {
        let result = chat::run_turn(
            &mut session,
            &prompt,
            class.as_deref(),
            obsidian,
            Some(local_ask),
        )
        .await;
        let _ = done_tx.send(TurnDone { result, session });
    });
    app.turn_handle = Some(handle);
}

/// 큐잉된 프롬프트를 정식 User 엔트리로 바꾼다. 없으면 새로 push.
fn promote_queued(app: &mut App, prompt: &str) {
    if let Some(e) = app
        .transcript
        .iter_mut()
        .find(|e| e.kind == EntryKind::Queued && e.text == prompt)
    {
        e.kind = EntryKind::User;
        return;
    }
    push(app, EntryKind::User, prompt.to_string());
}

fn finish_turn(
    app: &mut App,
    done: TurnDone,
    local_ask: &LocalAsk,
    done_tx: &mpsc::UnboundedSender<TurnDone>,
) {
    app.busy = false;
    app.streaming = false;
    app.turn_handle = None;
    app.session = done.session;
    app.binding = binding_label(&app.session);
    match done.result {
        Ok(info) => {
            let secs = info.elapsed_ms as f64 / 1000.0;
            app.status = format!(
                "{}  {}  ·  {:.1}s",
                info.status, info.label, secs
            );
            let fmt_k = |n: u32| -> String {
                if n >= 1_000_000 {
                    format!("{:.0}M", n as f64 / 1e6)
                } else if n >= 1_000 {
                    format!("{:.0}k", n as f64 / 1e3)
                } else {
                    n.to_string()
                }
            };
            app.tokens = format!("{}/{}", fmt_k(info.tokens_in), fmt_k(info.tokens_out));
            if info.ctx_window > 0 {
                let pct = info
                    .ctx_used
                    .min(info.ctx_window)
                    .saturating_mul(100)
                    .checked_div(info.ctx_window)
                    .unwrap_or(0);
                let cache =
                    cache_usage_label(info.cached_in, info.ctx_used, info.cache_reported);
                app.ctx = format!(
                    "ctx {}/{} ({}%) · {} · compact auto · mem {}",
                    fmt_k(info.ctx_used),
                    fmt_k(info.ctx_window),
                    pct,
                    cache,
                    if app.session.obsidian_on { "on" } else { "off" }
                );
            } else {
                app.ctx.clear();
            }
            if !info.run_id.is_empty() {
                let summary = completion_report(&info);
                replace_with_final_response(&mut app.transcript, info.answer, summary);
                app.todos.clear();
                app.agents.clear();
                app.scroll = u16::MAX;
                app.follow = false;
                app.final_summary = true;
            }
        }
        Err(e) => {
            push(app, EntryKind::Warn, format!("오류: {e:#}"));
            app.status = "실패".into();
        }
    }

    if !app.queue.is_empty() && !app.quit_after {
        let next = app.queue.remove(0);
        let class = app.session.class.clone();
        let obsidian = app.session.obsidian_on;
        start_turn(app, next, class, obsidian, local_ask, done_tx);
    }
}

fn replace_with_final_response(
    transcript: &mut Vec<Entry>,
    answer: String,
    summary: String,
) {
    transcript.clear();
    if !answer.trim().is_empty() {
        transcript.push(Entry {
            kind: EntryKind::Assistant,
            text: answer,
        });
    }
    transcript.push(Entry {
        kind: EntryKind::System,
        text: summary,
    });
}

fn start_compact(
    app: &mut App,
    done_tx: &mpsc::UnboundedSender<TurnDone>,
) {
    if app.busy {
        return;
    }
    push(app, EntryKind::System, "대화를 요약해 압축하는 중…");
    app.busy = true;
    app.status = "압축 중…".into();
    let mut session = app.session.clone();
    let done_tx = done_tx.clone();
    tokio::spawn(async move {
        let result = match chat::compact_session(&mut session).await {
            Ok(len) => {
                push_live_system(format!("압축 완료 ({len}자 요약)."));
                Ok(chat::TurnInfo {
                    run_id: String::new(),
                    label: "compact 완료".into(),
                    status: "ok".into(),
                    tokens_in: 0,
                    tokens_out: 0,
                    ctx_used: 0,
                    ctx_window: 0,
                    cached_in: 0,
                    cache_reported: false,
                    elapsed_ms: 0,
                    answer: String::new(),
                    summary: chat::CompletionSummary::default(),
                })
            }
            Err(e) => Err(e),
        };
        let _ = done_tx.send(TurnDone { result, session });
    });
}

/// /engine multi — 등록 연결의 모델을 원격 조회해 역할별로 자동 배정한다.
/// 네트워크 조회가 껴 있어 start_compact 와 같은 백그라운드 턴으로 돈다.
fn start_assign(
    app: &mut App,
    done_tx: &mpsc::UnboundedSender<TurnDone>,
) {
    if app.busy {
        return;
    }
    push(app, EntryKind::System, "Provider 모델을 조회해 역할별로 배정하는 중…");
    app.busy = true;
    app.status = "역할 배정 중…".into();
    let mut session = app.session.clone();
    let done_tx = done_tx.clone();
    tokio::spawn(async move {
        let result = match crate::harness::auto_assign_roles(&session.cfg).await {
            Ok(notes) => {
                for n in notes {
                    push_live_system(n);
                }
                if let Ok(cfg) = session.cfg.reload() {
                    session.cfg = cfg;
                }
                session.sticky = None;
                Ok(chat::TurnInfo {
                    run_id: String::new(),
                    label: "역할 배정 완료".into(),
                    status: "ok".into(),
                    tokens_in: 0,
                    tokens_out: 0,
                    ctx_used: 0,
                    ctx_window: 0,
                    cached_in: 0,
                    cache_reported: false,
                    elapsed_ms: 0,
                    answer: String::new(),
                    summary: chat::CompletionSummary::default(),
                })
            }
            Err(e) => Err(e),
        };
        let _ = done_tx.send(TurnDone { result, session });
    });
}

fn push_live_system(text: String) {
    crate::ui::live_line(&text);
}

fn apply_live(app: &mut App, ev: Live) {
    if app.final_summary && ignore_after_final_summary(&ev) {
        return;
    }
    app.follow = true;
    match ev {
        Live::Chunk(s) => {
            // 큐잉 엔트리 등이 사이에 끼어도 유실되지 않게, 붙일 곳이 없으면 새 답변을 연다.
            if !app.streaming
                || app.transcript.last().map(|e| e.kind) != Some(EntryKind::Assistant)
            {
                app.transcript.push(Entry {
                    kind: EntryKind::Assistant,
                    text: String::new(),
                });
                app.streaming = true;
            }
            if let Some(last) = app.transcript.last_mut() {
                if last.kind == EntryKind::Assistant {
                    last.text.push_str(&s);
                }
            }
        }
        Live::Assistant(s) => {
            if app.streaming {
                if let Some(last) = app.transcript.last_mut() {
                    if last.kind == EntryKind::Assistant {
                        if !last.text.ends_with('\n') && !last.text.is_empty() {
                            last.text.push('\n');
                        }
                        last.text.push_str(&s);
                        return;
                    }
                }
            }
            push(app, EntryKind::Assistant, s);
        }
        Live::System(s) => {
            if let Some(tag) = s.strip_prefix("[upgrade-ok]") {
                app.upgrade = None;
                push(app, EntryKind::System, format!("{tag} 설치 완료 — rafikx 를 다시 실행하면 새 버전으로 시작됩니다."));
                return;
            }
            if s == "[upgrade-done]" {
                app.upgrading = false;
                app.status = "준비".into();
                return;
            }
            let kind = if s.contains("[도구]") {
                EntryKind::Tool
            } else {
                EntryKind::System
            };
            push(app, kind, s);
        }
        Live::Warn(s) => push(app, EntryKind::Warn, s),
        Live::Status(s) => {
            // 모델 호출이 끝날 때마다 누적 사용량과 현재 컨텍스트·캐시를 반영한다.
            if s.starts_with("[tokens]") {
                if let Some(metrics) = parse_token_metrics(&s) {
                    let fmt_k = |n: u32| -> String {
                        if n >= 1_000_000 {
                            format!("{:.0}M", n as f64 / 1e6)
                        } else if n >= 1_000 {
                            format!("{:.0}k", n as f64 / 1e3)
                        } else {
                            n.to_string()
                        }
                    };
                    app.tokens = format!(
                        "{}/{}",
                        fmt_k(metrics.total_in),
                        fmt_k(metrics.total_out)
                    );
                    let win = crate::harness::current_ctx_window();
                    if win > 0 {
                        let pct = metrics
                            .context
                            .min(win)
                            .saturating_mul(100)
                            .checked_div(win)
                            .unwrap_or(0);
                        let cache = metrics
                            .cache
                            .map(|n| cache_usage_label(n, metrics.context, true))
                            .unwrap_or_else(|| "cache --".into());
                        app.ctx = format!(
                            "ctx {}/{} ({}%) · {} · compact auto · mem {}",
                            fmt_k(metrics.context),
                            fmt_k(win),
                            pct,
                            cache,
                            if app.session.obsidian_on { "on" } else { "off" }
                        );
                    }
                }
                return;
            }
            app.status = s;
        }
        Live::Todo(items) => {
            app.todos = items;
            app.follow = true;
        }
        Live::Agent(progress) => {
            update_agent_progress(&mut app.agents, progress);
            app.follow = true;
        }
    }
}

fn ignore_after_final_summary(event: &Live) -> bool {
    matches!(
        event,
        Live::Chunk(_)
            | Live::Assistant(_)
            | Live::System(_)
            | Live::Warn(_)
            | Live::Status(_)
            | Live::Todo(_)
            | Live::Agent(_)
    )
}

fn update_agent_progress(
    agents: &mut Vec<crate::ui::AgentProgress>,
    progress: crate::ui::AgentProgress,
) {
    if let Some(existing) = agents.iter_mut().find(|item| item.id == progress.id) {
        *existing = progress;
    } else {
        agents.push(progress);
    }
}

fn cache_usage_label(cached: u32, context: u32, reported: bool) -> String {
    if !reported {
        return "cache --".into();
    }
    let percent = cached
        .min(context)
        .saturating_mul(100)
        .checked_div(context)
        .unwrap_or(0);
    format!("cache {percent}%")
}

/// 턴 종료 통계 한 블록만 출력한다 — oh-my-pi 스타일: 후속 행동 메뉴 없음,
/// 개선 제안 서사 없음. 다음 행동은 사용자가 입력한다.
fn completion_report(info: &chat::TurnInfo) -> String {
    let summary = &info.summary;
    let files = if summary.changed_files.is_empty() {
        "없음".into()
    } else {
        summary.changed_files.join(", ")
    };
    let context = if info.ctx_window == 0 {
        "unknown".into()
    } else {
        let pct = info
            .ctx_used
            .min(info.ctx_window)
            .saturating_mul(100)
            .checked_div(info.ctx_window)
            .unwrap_or(0);
        format!(
            "{}/{} ({}%)",
            format_run_tokens(info.ctx_used),
            format_run_tokens(info.ctx_window),
            pct
        )
    };
    let cache = cache_usage_label(info.cached_in, info.ctx_used, info.cache_reported);
    let compaction = if summary.auto_compacted {
        "ran"
    } else {
        "standby"
    };
    let memory = if summary.memory_enabled {
        format!("{} source(s)", summary.memory_sources)
    } else {
        "off".into()
    };
    let mut text = format!(
        "Run summary · {} {} · {:.1}s · {}/{}\n  Context {} · {} · compact auto ({}) · memory {}\n  Work    Todo {}/{} · {} iteration(s) · {} change(s) · {} tool error(s)",
        completion_mark(&info.status),
        info.status,
        info.elapsed_ms as f64 / 1000.0,
        summary.provider,
        summary.model,
        context,
        cache,
        compaction,
        memory,
        summary.completed_todos,
        summary.total_todos,
        summary.iterations,
        summary.changed_files.len(),
        summary.tool_errors
    );
    if !summary.changed_files.is_empty() {
        text.push_str(&format!("\n  Files   {files}"));
    }
    text
}

fn format_run_tokens(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn completion_mark(status: &str) -> &'static str {
    if status.eq_ignore_ascii_case("ok") {
        "✓"
    } else {
        "!"
    }
}

struct TokenMetrics {
    total_in: u32,
    total_out: u32,
    context: u32,
    cache: Option<u32>,
}

fn parse_token_metrics(status: &str) -> Option<TokenMetrics> {
    let value = |key: &str| {
        status
            .split_whitespace()
            .find_map(|part| part.strip_prefix(key))
            .and_then(|raw| raw.parse::<u32>().ok())
    };
    Some(TokenMetrics {
        total_in: value("total_in=")?,
        total_out: value("total_out=")?,
        context: value("context=")?,
        cache: value("cache="),
    })
}

fn push(app: &mut App, kind: EntryKind, text: impl Into<String>) {
    app.transcript.push(Entry {
        kind,
        text: text.into(),
    });
}

fn binding_label(session: &Session) -> String {
    let model = session.model.as_deref().unwrap_or("auto");
    let provider = session.provider.as_deref().unwrap_or("auto");
    let class = session.class.as_deref().unwrap_or("auto");
    let mode = if session.is_plan_mode() { "plan" } else { "build" };
    format!("{provider} · {model} · {class} · {mode}")
}

fn collapsed_input(input: &str) -> String {
    let lines = input.lines().count().max(1);
    let chars = input.chars().count();
    if lines >= 4 || chars > 240 {
        format!("[붙여넣기 {lines}줄 · {chars}자]")
    } else {
        input.to_string()
    }
}

fn hydrate(messages: &[Message]) -> Vec<Entry> {
    let mut out = Vec::new();
    for m in messages {
        let mut texts = Vec::new();
        let mut tools = Vec::new();
        for b in &m.content {
            match b {
                ContentBlock::Text { text } if !text.trim().is_empty() => texts.push(text.clone()),
                ContentBlock::ToolUse { name, .. } => tools.push(format!("[도구] {name}")),
                ContentBlock::ToolResult { content, is_error, .. } => {
                    let tag = if *is_error { "도구 오류" } else { "도구 결과" };
                    tools.push(format!("{tag}: {}", truncate(content, 400)));
                }
                _ => {}
            }
        }
        match m.role {
            Role::User => {
                if !texts.is_empty() {
                    out.push(Entry {
                        kind: EntryKind::User,
                        text: texts.join("\n"),
                    });
                }
                for t in tools {
                    out.push(Entry {
                        kind: EntryKind::Tool,
                        text: t,
                    });
                }
            }
            Role::Assistant => {
                if !texts.is_empty() {
                    out.push(Entry {
                        kind: EntryKind::Assistant,
                        text: texts.join("\n"),
                    });
                }
                for t in tools {
                    out.push(Entry {
                        kind: EntryKind::Tool,
                        text: t,
                    });
                }
            }
            Role::System => {
                if !texts.is_empty() {
                    out.push(Entry {
                        kind: EntryKind::System,
                        text: texts.join("\n"),
                    });
                }
            }
        }
    }
    out
}

fn truncate(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

fn insert_char(app: &mut App, c: char) {
    app.input.insert(app.cursor, c);
    app.cursor += c.len_utf8();
    app.history_idx = None;
    app.slash_armed = false;
}

fn backspace(app: &mut App) {
    if app.cursor == 0 {
        return;
    }
    let prev = prev_idx(&app.input, app.cursor);
    app.input.replace_range(prev..app.cursor, "");
    app.cursor = prev;
    app.slash_armed = false;
}

fn delete(app: &mut App) {
    if app.cursor >= app.input.len() {
        return;
    }
    let next = next_idx(&app.input, app.cursor);
    app.input.replace_range(app.cursor..next, "");
}

fn prev_idx(s: &str, i: usize) -> usize {
    s[..i].chars().next_back().map(|c| i - c.len_utf8()).unwrap_or(0)
}

fn next_idx(s: &str, i: usize) -> usize {
    s[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(s.len())
}

fn history_prev(app: &mut App) {
    if app.input.contains('\n') {
        return;
    }
    if app.history.is_empty() {
        return;
    }
    let i = match app.history_idx {
        None => app.history.len() - 1,
        Some(0) => 0,
        Some(n) => n - 1,
    };
    app.history_idx = Some(i);
    app.input = app.history[i].clone();
    app.cursor = app.input.len();
}

fn history_next(app: &mut App) {
    if app.input.contains('\n') {
        return;
    }
    let Some(i) = app.history_idx else {
        return;
    };
    if i + 1 >= app.history.len() {
        app.history_idx = None;
        app.input.clear();
        app.cursor = 0;
        return;
    }
    app.history_idx = Some(i + 1);
    app.input = app.history[i + 1].clone();
    app.cursor = app.input.len();
}

/// U 키 — 업그레이드 요청을 남기고 에이전트를 종료하면 main 이 `rafikx update` 흐름을 실행한다.
fn start_upgrade(app: &mut App) {
    if app.upgrading {
        return;
    }
    app.upgrading = true;
    crate::update::request_update();
    app.quit = true;
    app.status = "종료 후 업데이트를 실행합니다…".into();
    push(app, EntryKind::System, "에이전트를 종료하고 업데이트를 실행합니다. 곧 화면이 전환됩니다.");
}

fn open_model_picker(app: &mut App) {
    let regs = crate::auth::registered_models(&app.session.cfg);
    if regs.is_empty() {
        push(
            app,
            EntryKind::Warn,
            "등록된 모델이 없습니다. /connect 로 서비스를 연결하세요.",
        );
        return;
    }
    let mut items = Vec::new();
    let mut ids = Vec::new();
    items.push("하네스 자동".into());
    ids.push(String::new());
    for r in &regs {
        items.push(format!("{} / {}", r.provider, r.id));
        ids.push(format!("{}\t{}", r.provider, r.id));
    }
    app.picker = Some(Picker {
        title: "모델".into(),
        items,
        ids,
        selected: 0,
        kind: PickerKind::Model,
        target: None,
        query: String::new(),
    });
}

const CUSTOM_ID: &str = "__custom__";

fn open_manage_picker(app: &mut App) {
    let names = crate::auth::menu_provider_names(&app.session.cfg);
    let mut items = Vec::new();
    let mut ids = Vec::new();
    for n in &names {
        items.push(crate::accounts_ui::manage_row(&app.session.cfg, n));
        ids.push(n.clone());
    }
    items.push("사용자 지정 OpenAI 호환 추가".into());
    ids.push(CUSTOM_ID.into());
    app.picker = Some(Picker {
        title: "서비스 · Enter 등록/관리".into(),
        items,
        ids,
        selected: 0,
        kind: PickerKind::Manage,
        target: None,
        query: String::new(),
    });
}

fn open_action_picker(app: &mut App, name: &str) {
    let mut items = vec![
        "키 다시 붙이기".into(),
        "이 세션에서 사용".into(),
        "기본값으로 설정".into(),
        "기본 모델 바꾸기".into(),
    ];
    let mut ids = vec![
        "key".into(),
        "use".into(),
        "default".into(),
        "model".into(),
    ];
    if app
        .session
        .cfg
        .provider(name)
        .map(|p| p.kind == "openai_compat")
        .unwrap_or(false)
    {
        items.push("Base URL 바꾸기".into());
        ids.push("url".into());
    }
    items.push("연결 해제 (키 삭제)".into());
    ids.push("delete".into());
    app.picker = Some(Picker {
        title: crate::auth::provider_label(name),
        items,
        ids,
        selected: 0,
        kind: PickerKind::Action,
        target: Some(name.to_string()),
        query: String::new(),
    });
}

fn start_connect(app: &mut App, raw: &str) {
    let alias = crate::auth::resolve_provider_alias(raw).unwrap_or_else(|| raw.trim().to_string());
    let names = crate::auth::menu_provider_names(&app.session.cfg);
    let Some(name) = names.iter().find(|n| *n == &alias).cloned().or_else(|| {
        names
            .into_iter()
            .find(|n| crate::auth::provider_label(n).to_lowercase().contains(&raw.to_lowercase()))
    }) else {
        push(
            app,
            EntryKind::Warn,
            format!("'{raw}' 서비스를 찾지 못했습니다. /connect"),
        );
        return;
    };
    begin_connect(app, &name);
}

fn apply_picker(app: &mut App, picker: Picker) {
    let Some(id) = picker.ids.get(picker.selected).cloned() else {
        return;
    };
    match picker.kind {
        PickerKind::Model => {
            if id.is_empty() {
                app.session.provider = None;
                app.session.model = None;
                // 자동 전환 시 영속 선택도 초기화
                crate::chat::persist_last_choice(&app.session.cfg.clone(), "", "");
                push(app, EntryKind::System, "모델을 하네스 자동으로 돌렸습니다.");
            } else {
                let (selected_provider, selected_model) =
                    id.split_once('\t').unwrap_or(("", id.as_str()));
                app.session.model = Some(selected_model.to_string());
                // 영속화 — 재시작 후에도 동일 모델 사용
                if let Some(r) = crate::auth::registered_models(&app.session.cfg)
                    .into_iter()
                    .find(|r| r.provider == selected_provider && r.id == selected_model)
                {
                    app.session.provider = Some(r.provider.clone());
                    let _ = crate::accounts_ui::write_provider_model(
                        &app.session.cfg,
                        &r.provider,
                        &r.id,
                    );
                    crate::chat::persist_last_choice(
                        &app.session.cfg.clone(),
                        &r.provider,
                        &r.id,
                    );
                    push(
                        app,
                        EntryKind::System,
                        format!("모델: {} (저장 — 재시작 후에도 유지)", r.id),
                    );
                } else {
                    push(
                        app,
                        EntryKind::System,
                        format!("모델: {selected_model} (세션 한정)"),
                    );
                }
            }
            app.binding = binding_label(&app.session);
        }
        PickerKind::Manage => {
            if id == CUSTOM_ID {
                start_custom(app);
            } else {
                pick_managed(app, &id);
            }
        }
        PickerKind::Action => {
            let Some(name) = picker.target else { return };
            apply_action(app, &name, &id);
        }
    }
}

fn pick_managed(app: &mut App, name: &str) {
    let Ok(p) = app.session.cfg.provider(name) else {
        push(app, EntryKind::Warn, format!("'{name}' 이(가) config에 없습니다."));
        return;
    };
    if crate::auth::auth_mode(name, p) == "none" {
        use_provider(app, name);
        push(app, EntryKind::System, "로컬(Ollama)은 키가 없습니다.");
        return;
    }
    if crate::auth::is_connected(&app.session.cfg, name) {
        open_action_picker(app, name);
    } else {
        open_secret(app, name);
    }
}

fn apply_action(app: &mut App, name: &str, action: &str) {
    match action {
        "key" => open_secret(app, name),
        "use" => use_provider(app, name),
        "default" => match crate::accounts_ui::set_default_provider(&app.session.cfg, name) {
            Ok(()) => {
                reload_cfg(app);
                push(
                    app,
                    EntryKind::System,
                    format!("기본 서비스: {}", crate::auth::provider_label(name)),
                );
            }
            Err(e) => push(app, EntryKind::Warn, format!("{e:#}")),
        },
        "model" => open_text(
            app,
            "기본 모델",
            "모델 ID 를 입력하세요. Enter 저장",
            app.session
                .cfg
                .provider(name)
                .map(|p| p.model.clone())
                .unwrap_or_default(),
            TextKind::Model {
                provider: name.to_string(),
            },
        ),
        "url" => open_text(
            app,
            "Base URL",
            "OpenAI 호환 base URL. Enter 저장",
            app.session
                .cfg
                .provider(name)
                .ok()
                .and_then(|p| p.base_url.clone())
                .unwrap_or_default(),
            TextKind::BaseUrl {
                provider: name.to_string(),
            },
        ),
        "delete" => ask_disconnect(app, name),
        _ => {}
    }
}

fn begin_connect(app: &mut App, name: &str) {
    pick_managed(app, name);
}

fn use_provider(app: &mut App, name: &str) {
    app.session.provider = Some(name.to_string());
    // 영속화 — 기본 연결로 저장
    let _ = crate::accounts_ui::set_default_provider(&app.session.cfg, name);
    app.binding = binding_label(&app.session);
    push(
        app,
        EntryKind::System,
        format!(
            "프로바이더: {} (기본 연결로 저장)",
            crate::auth::provider_label(name)
        ),
    );
}

fn open_secret(app: &mut App, name: &str) {
    set_mouse_capture(false);
    app.secret = Some(SecretPrompt {
        provider: name.to_string(),
        buf: String::new(),
    });
    app.status = format!(
        "{} · 키를 붙여넣으세요 (Ctrl+V)",
        crate::auth::provider_label(name)
    );
}

fn open_text(app: &mut App, title: &str, hint: &str, buf: String, kind: TextKind) {
    set_mouse_capture(false);
    app.text = Some(TextPrompt {
        title: title.into(),
        hint: hint.into(),
        buf,
        kind,
    });
}

fn start_custom(app: &mut App) {
    open_text(
        app,
        "사용자 지정 이름",
        "영문 소문자·숫자·밑줄. 예: myproxy",
        String::new(),
        TextKind::CustomName,
    );
}

fn ask_disconnect(app: &mut App, name: &str) {
    app.confirm = Some(ConfirmPrompt {
        title: "연결 해제".into(),
        body: format!(
            "{} 키를 지우고 연결을 해제할까요?\n\n[y] 삭제   [n] 취소",
            crate::auth::provider_label(name)
        ),
        provider: name.to_string(),
    });
}

fn confirm_yes(app: &mut App, c: ConfirmPrompt) {
    match crate::auth::disconnect_provider(&c.provider) {
        Ok(()) => {
            reload_cfg(app);
            if app.session.provider.as_deref() == Some(c.provider.as_str()) {
                app.session.provider = None;
            }
            app.binding = binding_label(&app.session);
            push(
                app,
                EntryKind::System,
                format!("{} 연결을 해제했습니다.", crate::auth::provider_label(&c.provider)),
            );
        }
        Err(e) => push(app, EntryKind::Warn, format!("{e:#}")),
    }
}

fn finish_secret(app: &mut App, secret: SecretPrompt) {
    set_mouse_capture(true);
    match crate::auth::replace_or_save_key(&secret.provider, &secret.buf) {
        Ok(_) => {
            reload_cfg(app);
            let _ = crate::accounts_ui::set_default_provider(&app.session.cfg, &secret.provider);
            reload_cfg(app);
            app.session.provider = Some(secret.provider.clone());
            push(
                app,
                EntryKind::System,
                format!(
                    "{} 연결됨. 키는 대화에 남기지 않았습니다.",
                    crate::auth::provider_label(&secret.provider)
                ),
            );
            app.binding = binding_label(&app.session);
            app.status = "모델 목록 조회 중…".into();

            // 키가 정상이면 원격 모델 목록을 자동으로 불러와 저장하고,
            // 순위 기준 기본 모델까지 골라 저장한다.
            let cfg2 = app.session.cfg.clone();
            let pname = secret.provider.clone();
            tokio::spawn(async move {
                let list = crate::auth::list_remote_models(&cfg2, &pname).await;
                match list {
                    Ok(models) if !models.is_empty() => {
                        if let Err(e) = crate::auth::save_catalog(&cfg2, &pname, &models) {
                            crate::applog::info(&format!("catalog save: {e}"));
                        }
                        let (main_m, _small) =
                            crate::auth::pick_preferred(&models, &pname);
                        let mut note = format!(
                            "[models] {} 사용 가능 {}개 — /model 로 선택하세요",
                            crate::auth::provider_label(&pname),
                            models.len()
                        );
                        if let Some(m) = main_m {
                            if crate::api::set_provider_model(&pname, &m).is_ok() {
                                note.push_str(&format!(" · 기본 모델 {m} 저장"));
                            }
                        }
                        crate::ui::live_line(&note);
                    }
                    Ok(_) => {
                        crate::ui::live_line(&format!(
                            "[models] {} 의 모델 목록을 가져오지 못했습니다. /model 에서 기본 모델을 쓰세요.",
                            crate::auth::provider_label(&pname)
                        ));
                    }
                    Err(e) => {
                        crate::ui::live_warn(&format!(
                            "[models] {} 모델 조회 실패: {e}",
                            crate::auth::provider_label(&pname)
                        ));
                    }
                }
            });
        }
        Err(e) => {
            push(app, EntryKind::Warn, format!("연결 실패: {e:#}"));
            open_secret(app, &secret.provider);
        }
    }
}

fn finish_text(app: &mut App, text: TextPrompt) {
    set_mouse_capture(true);
    let val = crate::accounts_ui::sanitize_pasted_key(&text.buf);
    match text.kind {
        TextKind::BaseUrl { provider } => {
            match crate::accounts_ui::write_provider_base_url(&app.session.cfg, &provider, &val) {
                Ok(()) => {
                    reload_cfg(app);
                    push(app, EntryKind::System, format!("base URL: {val}"));
                }
                Err(e) => push(app, EntryKind::Warn, format!("{e:#}")),
            }
        }
        TextKind::Model { provider } => {
            match crate::accounts_ui::write_provider_model(&app.session.cfg, &provider, &val) {
                Ok(()) => {
                    reload_cfg(app);
                    app.session.model = Some(val.clone());
                    app.binding = binding_label(&app.session);
                    push(app, EntryKind::System, format!("모델: {val}"));
                }
                Err(e) => push(app, EntryKind::Warn, format!("{e:#}")),
            }
        }
        TextKind::CustomName => {
            if !crate::accounts_ui::valid_custom_id(&val) {
                push(
                    app,
                    EntryKind::Warn,
                    "이름은 영문 소문자로 시작하고 영문·숫자·밑줄만 됩니다.",
                );
                return;
            }
            open_text(
                app,
                "Base URL",
                "예: https://api.example.com/v1",
                String::new(),
                TextKind::CustomUrl { name: val },
            );
        }
        TextKind::CustomUrl { name } => {
            open_text(
                app,
                "기본 모델",
                "모델 ID",
                String::new(),
                TextKind::CustomModel {
                    name,
                    base_url: val,
                },
            );
        }
        TextKind::CustomModel { name, base_url } => {
            match crate::accounts_ui::append_custom_openai(&app.session.cfg, &name, &base_url, &val)
            {
                Ok(()) => {
                    reload_cfg(app);
                    push(app, EntryKind::System, format!("'{name}' 를 추가했습니다. 키를 붙이세요."));
                    open_secret(app, &name);
                }
                Err(e) => push(app, EntryKind::Warn, format!("{e:#}")),
            }
        }
    }
}

fn reload_cfg(app: &mut App) {
    match app.session.cfg.reload() {
        Ok(cfg) => app.session.cfg = cfg,
        Err(e) => push(app, EntryKind::Warn, format!("설정 다시 읽기 실패: {e:#}")),
    }
}

fn handle_paste(app: &mut App, raw: &str) {
    if let Some(s) = app.secret.as_mut() {
        s.buf
            .push_str(&crate::accounts_ui::sanitize_pasted_key(raw));
        return;
    }
    if let Some(t) = app.text.as_mut() {
        t.buf
            .push_str(&crate::accounts_ui::sanitize_pasted_key(raw));
        return;
    }
    if app.busy || app.picker.is_some() || app.help || app.confirm.is_some() {
        return;
    }
    let cleaned = raw.replace('\r', "");
    for ch in cleaned.chars() {
        if ch == '\n' {
            insert_char(app, '\n');
        } else {
            insert_char(app, ch);
        }
    }
}

fn set_mouse_capture(on: bool) {
    let mut out = stdout();
    if on {
        let _ = execute!(out, EnableMouseCapture);
    } else {
        let _ = execute!(out, DisableMouseCapture);
    }
}

fn read_clipboard() -> Option<String> {
    #[cfg(windows)]
    {
        return read_clipboard_windows();
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn read_clipboard_windows() -> Option<String> {
    type Handle = *mut std::ffi::c_void;
    unsafe extern "system" {
        fn OpenClipboard(h: Handle) -> i32;
        fn CloseClipboard() -> i32;
        fn GetClipboardData(fmt: u32) -> Handle;
        fn GlobalLock(h: Handle) -> Handle;
        fn GlobalUnlock(h: Handle) -> i32;
    }
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let h = GetClipboardData(CF_UNICODETEXT);
        if h.is_null() {
            let _ = CloseClipboard();
            return None;
        }
        let ptr = GlobalLock(h) as *const u16;
        if ptr.is_null() {
            let _ = CloseClipboard();
            return None;
        }
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
            if len > 16_384 {
                break;
            }
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(h);
        let _ = CloseClipboard();
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

fn enable_utf8() {
    #[cfg(windows)]
    unsafe {
        type Handle = *mut std::ffi::c_void;
        unsafe extern "system" {
            fn SetConsoleOutputCP(cp: u32) -> i32;
            fn SetConsoleCP(cp: u32) -> i32;
            fn GetStdHandle(n: i32) -> Handle;
            fn GetConsoleMode(h: Handle, mode: *mut u32) -> i32;
            fn SetConsoleMode(h: Handle, mode: u32) -> i32;
        }
        let _ = SetConsoleOutputCP(65001);
        let _ = SetConsoleCP(65001);
        let h = GetStdHandle(-11);
        if !h.is_null() && h != (-1isize as Handle) {
            let mut mode = 0u32;
            if GetConsoleMode(h, &mut mode) != 0 {
                let _ = SetConsoleMode(h, mode | 0x0004);
            }
        }
    }
}

#[cfg(test)]
mod upgrade_tests {
    use super::*;

    #[test]
    fn long_paste_is_collapsed_without_losing_payload() {
        let pasted = "첫 줄\n둘째 줄\n셋째 줄\n넷째 줄\n다섯째 줄\n여섯째 줄\n일곱째 줄";
        assert_eq!(collapsed_input(pasted), "[붙여넣기 7줄 · 36자]");
        assert_eq!(pasted.lines().count(), 7);
    }

    #[test]
    fn cache_label_is_actual_reuse_percentage_only() {
        assert_eq!(cache_usage_label(900, 1_200, true), "cache 75%");
        assert_eq!(cache_usage_label(0, 0, false), "cache --");
    }

    #[test]
    fn approval_status_clicks_map_to_english_options() {
        let model = "minimax-m3";
        let prefix = format!(
            "{STATUS_MARK}{STATUS_NAME}{STATUS_DIVIDER} MODEL {model} ·"
        );
        let yes = crate::tui::md::display_width(&prefix) as u16;
        let no = yes + crate::tui::md::display_width(APPROVAL_YES) as u16;
        let always = no + crate::tui::md::display_width(APPROVAL_NO) as u16;
        assert_eq!(
            approval_click(yes + 1, 8, 10, model),
            Some(ApprovalChoice::Yes)
        );
        assert_eq!(
            approval_click(no + 1, 8, 10, model),
            Some(ApprovalChoice::No)
        );
        assert_eq!(
            approval_click(always + 1, 8, 10, model),
            Some(ApprovalChoice::Always)
        );
        assert_eq!(approval_click(2, 8, 10, model), None);
    }

    #[test]
    fn approval_popup_buttons_are_directly_mouse_selectable() {
        assert_eq!(
            approval_popup_click(40, 23, 100, 30),
            Some(ApprovalChoice::Yes)
        );
        assert_eq!(
            approval_popup_click(48, 23, 100, 30),
            Some(ApprovalChoice::No)
        );
        assert_eq!(
            approval_popup_click(56, 23, 100, 30),
            Some(ApprovalChoice::Always)
        );
        assert_eq!(approval_popup_click(40, 22, 100, 30), None);
    }

    #[test]
    fn approval_popup_stacks_buttons_on_narrow_terminals() {
        assert_eq!(
            approval_popup_click(2, 5, 18, 10),
            Some(ApprovalChoice::Yes)
        );
        assert_eq!(
            approval_popup_click(2, 6, 18, 10),
            Some(ApprovalChoice::No)
        );
        assert_eq!(
            approval_popup_click(2, 7, 18, 10),
            Some(ApprovalChoice::Always)
        );
    }

    #[test]
    fn agent_progress_updates_role_model_and_status_in_place() {
        let mut agents = Vec::new();
        update_agent_progress(
            &mut agents,
            crate::ui::AgentProgress {
                id: "agent-1".into(),
                role: "reviewer".into(),
                model: "minimax-m3".into(),
                status: "running".into(),
            },
        );
        update_agent_progress(
            &mut agents,
            crate::ui::AgentProgress {
                id: "agent-1".into(),
                role: "reviewer".into(),
                model: "minimax-m3".into(),
                status: "ok".into(),
            },
        );
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, "ok");
    }

    #[test]
    fn completion_report_has_no_next_action_menu() {
        let info = chat::TurnInfo {
            run_id: "run-1".into(),
            label: "dev".into(),
            status: "ok".into(),
            tokens_in: 0,
            tokens_out: 0,
            ctx_used: 0,
            ctx_window: 0,
            cached_in: 0,
            cache_reported: false,
            elapsed_ms: 1,
            answer: "완료".into(),
            summary: chat::CompletionSummary {
                changed_files: vec!["src/main.rs".into()],
                iterations: 1,
                completed_todos: 1,
                total_todos: 1,
                tool_errors: 0,
                provider: "minimax".into(),
                model: "minimax-m3".into(),
                auto_compacted: false,
                memory_enabled: false,
                memory_sources: 0,
            },
        };
        let text = completion_report(&info);
        assert!(text.contains("Run summary"));
        assert!(text.contains("Files   src/main.rs"));
        // oh-my-pi 스타일: 턴 종료 후 행동 선택 메뉴·개선 서사를 출력하지 않는다.
        assert!(!text.contains("Choose next action"));
        assert!(!text.contains("Improve"));
        assert!(!text.contains("[1]"));
    }

    #[test]
    fn final_response_keeps_answer_before_run_summary() {
        let mut transcript = vec![
            Entry {
                kind: EntryKind::Assistant,
                text: "[모델 작업]\n조사 중".into(),
            },
            Entry {
                kind: EntryKind::Tool,
                text: "[도구] read_file".into(),
            },
        ];
        let answer = "| model |\n|---|\n| minimax-m3 |";
        replace_with_final_response(&mut transcript, answer.into(), "Run summary".into());
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0].kind, EntryKind::Assistant);
        assert_eq!(transcript[0].text, answer);
        assert_eq!(transcript[1].kind, EntryKind::System);
        assert_eq!(transcript[1].text, "Run summary");
    }

    #[test]
    fn final_summary_ignores_late_work_events() {
        assert!(ignore_after_final_summary(&Live::Chunk("late".into())));
        assert!(ignore_after_final_summary(&Live::Assistant("late".into())));
        assert!(ignore_after_final_summary(&Live::System("[도구] late".into())));
        assert!(ignore_after_final_summary(&Live::Status(
            "[tokens] total_in=1 total_out=1 context=1".into()
        )));
    }

    #[test]
    fn completion_mark_never_labels_non_success_as_success() {
        assert_eq!(completion_mark("ok"), "✓");
        assert_eq!(completion_mark("denied"), "!");
        assert_eq!(completion_mark("limit"), "!");
    }
}
