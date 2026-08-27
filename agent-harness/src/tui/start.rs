use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::App;
use super::view::Pal;
use crate::lifecycle::LifecycleState;

const STAGES: [&str; 4] = ["CONTEXT", "PLAN", "EXECUTE", "VERIFY"];

pub fn draw(f: &mut Frame, app: &App, area: Rect, palette: &Pal) {
    let compact = area.width < 72;
    let short = area.height < 18;
    let mut lines = Vec::new();
    let content_rows = if compact { 10 } else { 9 };
    let top = area.height.saturating_sub(content_rows) / 2;
    lines.extend((0..top).map(|_| Line::default()));
    lines.push(Line::from(Span::styled(
        "R A F I K X  /  R U N S P A C E",
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        if booting(app) {
            "INITIALIZING THE AGENT RUNTIME"
        } else if configured(app) {
            "THE TERMINAL IS NOW A RUNTIME"
        } else {
            "CONNECT A MODEL TO OPEN THE RUNTIME"
        },
        Style::default().fg(palette.mute),
    )));
    lines.push(Line::default());
    lines.extend(signal_lines(app, compact, palette));
    if !short {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "맥락을 모으고, 계획하고, 실행하고, 검증합니다.",
            Style::default().fg(palette.body),
        )));
    }
    lines.push(Line::default());
    lines.push(metadata_line("MODEL", &app.binding, palette));
    lines.push(metadata_line("WORKSPACE", &app.cwd, palette));
    if area.height >= 16 && !app.recent_sessions.is_empty() {
        // 데스크탑 splash의 세션 히스토리에 대응하는 CLI 시작 화면 요소.
        lines.push(metadata_line(
            "RECENT",
            &app.recent_sessions.join("  ›  "),
            palette,
        ));
    }
    if area.height >= 14 {
        lines.push(Line::from(Span::styled(
            if configured(app) {
                "Enter 실행  ·  Tab plan/build  ·  /sessions 최근 세션  ·  ? 도움말"
            } else {
                "/connect 로 모델 연결"
            },
            Style::default().fg(palette.mute),
        )));
    }
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

pub fn compact_signal(app: &App, palette: &Pal) -> Vec<Span<'static>> {
    let current = active_stage(app);
    STAGES
        .iter()
        .enumerate()
        .flat_map(|(index, label)| {
            let marker = stage_marker(app, index, current);
            let color = stage_color(app, index, current, palette);
            let mut spans = vec![Span::styled(
                format!("{marker} {label}"),
                Style::default().fg(color),
            )];
            if index + 1 < STAGES.len() {
                spans.push(Span::styled(" ─ ", Style::default().fg(palette.border)));
            }
            spans
        })
        .collect()
}

pub fn short_signal(app: &App, palette: &Pal) -> Vec<Span<'static>> {
    let current = active_stage(app);
    ["C", "P", "E", "V"]
        .iter()
        .enumerate()
        .flat_map(|(index, label)| {
            let marker = stage_marker(app, index, current);
            let color = stage_color(app, index, current, palette);
            let mut spans = vec![Span::styled(
                format!("{marker}{label}"),
                Style::default().fg(color),
            )];
            if index + 1 < STAGES.len() {
                spans.push(Span::styled("›", Style::default().fg(palette.border)));
            }
            spans
        })
        .collect()
}

fn signal_lines(app: &App, compact: bool, palette: &Pal) -> Vec<Line<'static>> {
    let spans = compact_signal(app, palette);
    if !compact {
        return vec![Line::from(spans)];
    }
    vec![
        Line::from(spans[..3].to_vec()),
        Line::from(spans[4..].to_vec()),
    ]
}

fn metadata_line(label: &str, value: &str, palette: &Pal) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<10}"),
            Style::default().fg(palette.secondary),
        ),
        Span::styled(value.to_string(), Style::default().fg(palette.text)),
    ])
}

fn booting(app: &App) -> bool {
    app.show_start && app.motion_tick < 16 && !reduced_motion(app)
}

fn configured(app: &App) -> bool {
    !crate::auth::usable_names(&app.session.cfg).is_empty()
}

fn active_stage(app: &App) -> Option<usize> {
    if booting(app) {
        return Some((app.motion_tick / 4).min(3) as usize);
    }
    match lifecycle_state(app) {
        Some(LifecycleState::Queued) => Some(0),
        Some(LifecycleState::Planning) => Some(1),
        Some(
            LifecycleState::Running
            | LifecycleState::WaitingApproval
            | LifecycleState::Delegating
            | LifecycleState::CancelRequested,
        ) => Some(2),
        Some(
            LifecycleState::Answering
            | LifecycleState::Succeeded
            | LifecycleState::Limited
            | LifecycleState::Failed
            | LifecycleState::Cancelled,
        ) => Some(3),
        None => None,
    }
}

fn stage_marker(app: &App, index: usize, current: Option<usize>) -> &'static str {
    if current == Some(index) {
        if booting(app) && app.motion_tick % 4 < 2 {
            "◆"
        } else {
            "●"
        }
    } else if current.is_some_and(|stage| index < stage) {
        "●"
    } else {
        "◇"
    }
}

fn stage_color(
    app: &App,
    index: usize,
    current: Option<usize>,
    palette: &Pal,
) -> ratatui::style::Color {
    let state = lifecycle_state(app);
    if current == Some(index) {
        return match state {
            Some(LifecycleState::Failed | LifecycleState::Cancelled) => palette.err,
            Some(LifecycleState::Limited | LifecycleState::WaitingApproval) => palette.warn,
            Some(LifecycleState::Succeeded) => palette.success,
            _ => palette.code,
        };
    }
    if current.is_some_and(|stage| index < stage) {
        palette.secondary
    } else {
        palette.mute
    }
}

fn lifecycle_state(app: &App) -> Option<LifecycleState> {
    app.lifecycle_state.lock().ok().and_then(|state| *state)
}

fn reduced_motion(app: &App) -> bool {
    terminal_reduced_motion(&app.session)
}

pub fn terminal_reduced_motion(session: &crate::chat::Session) -> bool {
    session.cfg.file.ui.reduced_motion
        || std::env::var("RAFIKX_REDUCE_MOTION")
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}
