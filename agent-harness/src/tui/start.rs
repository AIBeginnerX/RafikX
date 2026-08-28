use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::App;
use super::view::Pal;
use crate::lifecycle::LifecycleState;

const STAGES: [&str; 4] = ["CONTEXT", "PLAN", "EXECUTE", "VERIFY"];

/// 분할 시작 화면이 필요한 최소 크기 — 그 밖에선 기존 중앙 배치 폴백.
/// 높이 26: 7행 배너 + 리본·마키(11행)와 하단 정보(9행)가 함께 들어가는 선.
pub const SPLIT_MIN_WIDTH: u16 = 84;
pub const SPLIT_MIN_HEIGHT: u16 = 26;

/// 배너 최대 줄 수 — 글자 7행 + 여백 + 리본 + 마키.
const BANNER_LINES: u16 = 12;

pub fn draw(f: &mut Frame, app: &App, area: Rect, palette: &Pal) {
    if area.width >= SPLIT_MIN_WIDTH && area.height >= SPLIT_MIN_HEIGHT {
        draw_split(f, app, area, palette);
    } else {
        draw_centered(f, app, area, palette);
    }
}

// ---------------------------------------------------------------------------
// 분할 시작 화면 — 왼쪽 디지털 배너 + 오른쪽 세션·기능 요약, 하단 기존 정보.
// ---------------------------------------------------------------------------

fn draw_split(f: &mut Frame, app: &App, area: Rect, palette: &Pal) {
    // 하단 기존 내용(RECENT 제외)에 필요한 줄 수를 확보하고 나머지를 배너에 준다.
    let bottom_rows = bottom_lines(app, palette, false).len() as u16 + 2;
    let banner_h = area.height.saturating_sub(bottom_rows).min(BANNER_LINES);
    let (banner, bottom) = if banner_h >= 10 {
        (
            Rect::new(area.x, area.y, area.width, banner_h),
            Rect::new(area.x, area.y + banner_h, area.width, area.height - banner_h),
        )
    } else {
        (area, Rect::new(area.x, area.y, area.width, 0))
    };

    // 배너 맨 아래 행은 좌우 짧은 흐린 선 — 상단 분할과 하단 정보를 가볍게 구분.
    let (content_h, rules_y) = if banner_h >= 2 {
        (banner_h - 1, Some(banner.y + banner_h - 1))
    } else {
        (banner_h, None)
    };
    let content = Rect::new(banner.x, banner.y, banner.width, content_h);
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(content);
    draw_banner(f, app, halves[0], palette);
    draw_digest(f, app, halves[1], palette);

    if let Some(y) = rules_y {
        const RULE: &str = "────────────";
        let rule_style = Style::default().fg(palette.mute);
        let left_rule = Rect::new(banner.x + 2, y, RULE.chars().count() as u16, 1);
        let right_x = banner.x + banner.width.saturating_sub(2 + RULE.chars().count() as u16);
        let right_rule = Rect::new(right_x, y, RULE.chars().count() as u16, 1);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(RULE, rule_style))),
            left_rule,
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(RULE, rule_style))),
            right_rule,
        );
    }

    if bottom.height > 0 {
        let lines = bottom_lines(app, palette, false);
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), bottom);
    }
}

/// RafikX 로고 — { / } 마크와 RIX 워드마크를 7행 블록 아트로. 공식 로고(검은 바탕의
/// 중괄호+사선+RAFIKX 워드마크)의 터미널 재현: 중괄호 갈고리는 사선 쪽(안쪽)으로,
/// 중앙 돌기는 바깥쪽으로, 사선은 아래-왼쪽에서 위-오른쪽으로 뻗는다.
const GLYPHS: &[(char, [&str; 7])] = &[
    ('{', [" ██", " █ ", " █ ", "██ ", " █ ", " █ ", " ██"]),
    ('/', ["   █", "  █ ", "  █ ", " █  ", " █  ", "█   ", "█   "]),
    ('}', ["██ ", " █ ", " █ ", " ██", " █ ", " █ ", "██ "]),
    (
        'R',
        [
            "█████", "█   █", "█   █", "█████", "█ █  ", "█  █ ", "█   █",
        ],
    ),
    (
        'I',
        [
            "█████", "  █  ", "  █  ", "  █  ", "  █  ", "  █  ", "█████",
        ],
    ),
    (
        'X',
        [
            "█   █", "█   █", " █ █ ", "  █  ", " █ █ ", "█   █", "█   █",
        ],
    ),
];

fn glyph(ch: char) -> Option<&'static [&'static str; 7]> {
    GLYPHS.iter().find(|(c, _)| *c == ch).map(|(_, rows)| rows)
}

/// 배너 락업 — 콤팩트한 로고 마크 + 오른쪽 RIX 워드마크 (공식 로고 구성과 동일).
const MARK: &str = "{/}";
const WORD: &str = "RIX";
/// 마크 내부 간격 — 로고처럼 사선이 중괄호에 바짝 붙는다.
const MARK_GAP: usize = 2;
/// 마크와 워드마크 사이 — 로고의 마크↔RAFIKX 간격에 해당.
const GROUP_GAP: usize = 4;
/// 워드마크 글자 간격.
const WORD_GAP: usize = 1;
const MARK_W: usize = 3 + MARK_GAP + 4 + MARK_GAP + 3;
const WORD_W: usize = 3 * 5 + WORD_GAP * 2;
/// 락업 전체 폭 — 마크 + 그룹 간격 + 워드마크.
const LOCKUP_W: usize = MARK_W + GROUP_GAP + WORD_W;

/// 부팅 연출 — 글자가 아래에서부터 한 행씩 켜진다 (reduced_motion 이면 즉시 전체).
fn visible_rows(app: &App) -> usize {
    visible_rows_for(app.motion_tick, reduced_motion(app) || !booting(app))
}

fn visible_rows_for(tick: u16, settled: bool) -> usize {
    if settled {
        return 7;
    }
    ((tick as usize + 2) / 2).clamp(1, 7)
}

/// 반짝임 — (틱, 행, 열) 해시의 일부 칸만 밝게. 규칙적이지 않아 스치는 느낌이 난다.
fn spark(tick: u16, row: usize, col: usize) -> bool {
    let t = (tick as usize).wrapping_mul(11);
    let h = (t ^ row.wrapping_mul(31) ^ col.wrapping_mul(137))
        .wrapping_mul(2_654_435_761);
    (h >> 13) % 19 == 0
}

/// 테마별 배너 색 — 글자 전체에 accent→code 그라디언트를 깐다.
/// 테마가 바뀌면 accent·code 편성이 달라져 배너 색조가 함께 바뀐다.
fn banner_fill(palette: &Pal, t: f32) -> Color {
    lerp_rgb(palette.accent, palette.code, t)
}

fn lerp_rgb(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
            Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
        }
        _ => {
            if t < 0.5 {
                a
            } else {
                b
            }
        }
    }
}

const MARQUEE_CYCLE: &str = "RAFIKX HARNESS · THE TERMINAL IS NOW A RUNTIME · ";

fn marquee_line(tick: u16, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    // 바이트가 아니라 문자 단위로 순환한다 — '·' 같은 다중 바이트 문자가 깨진다.
    let cycle: Vec<char> = MARQUEE_CYCLE.chars().collect();
    let offset = tick as usize % cycle.len();
    (0..width)
        .map(|i| cycle[(offset + i) % cycle.len()])
        .collect()
}

fn draw_banner(f: &mut Frame, app: &App, area: Rect, palette: &Pal) {
    let left_inner_w = area.width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let ribbon: Vec<Span<'static>> = compact_signal(app, palette);
    if left_inner_w >= LOCKUP_W {
        // 락업을 패널 중앙에 모은다 — 콤팩트한 로고 + 워드마크 한 덩어리.
        let pad = (left_inner_w - LOCKUP_W) / 2;
        let shown = visible_rows(app);
        for row_index in 0..shown {
            let mut spans = vec![Span::raw(" ".repeat(pad))];
            let mut abs_col = 0usize;
            let mut draw_glyph = |spans: &mut Vec<Span<'static>>, ch: char, abs_col: &mut usize| {
                let Some(rows) = glyph(ch) else { return };
                for (col, cell) in rows[row_index].chars().enumerate() {
                    if cell == '█' {
                        let t = *abs_col as f32 / (LOCKUP_W - 1).max(1) as f32;
                        // 사선은 로고의 선처럼 항상 선명하게 — 나머지는 그라디언트·반짝임.
                        let style = if ch == '/' {
                            Style::default()
                                .fg(palette.text)
                                .add_modifier(Modifier::BOLD)
                        } else if !reduced_motion(app) && spark(app.motion_tick, row_index, col)
                        {
                            // 반짝임은 마크를 파지 않고 색만 밝힌다 — 항상 완전.
                            Style::default()
                                .fg(palette.text)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(banner_fill(palette, t))
                        };
                        spans.push(Span::styled("█".to_string(), style));
                    } else {
                        // 내부 공백도 그대로 출력 — 건너뛰면 모양이 뭉개진다.
                        spans.push(Span::raw(" "));
                    }
                    *abs_col += 1;
                }
            };
            for (part_index, ch) in MARK.chars().enumerate() {
                if part_index > 0 {
                    spans.push(Span::raw(" ".repeat(MARK_GAP)));
                    abs_col += MARK_GAP;
                }
                draw_glyph(&mut spans, ch, &mut abs_col);
            }
            spans.push(Span::raw(" ".repeat(GROUP_GAP)));
            abs_col += GROUP_GAP;
            for (letter_index, ch) in WORD.chars().enumerate() {
                if letter_index > 0 {
                    spans.push(Span::raw(" ".repeat(WORD_GAP)));
                    abs_col += WORD_GAP;
                }
                draw_glyph(&mut spans, ch, &mut abs_col);
            }
            lines.push(Line::from(spans));
        }
        // 부팅 중 남는 행 — 글자가 다 켜질 때까지 자리를 비워둔다.
        while lines.len() < 7 {
            lines.push(Line::default());
        }
        lines.push(Line::default());
        lines.push(Line::from(ribbon));
        lines.push(Line::from(Span::styled(
            marquee_line(app.motion_tick, left_inner_w.min(LOCKUP_W + 6)),
            Style::default().fg(palette.secondary),
        )));
    } else {
        // 좁은 패널 — 블록 글꼴 대신 한 줄 표기.
        lines.push(Line::from(Span::styled(
            "{/}  RAFIKX",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            marquee_line(app.motion_tick, left_inner_w),
            Style::default().fg(palette.secondary),
        )));
    }
    let top = (area.height.saturating_sub(lines.len() as u16)) / 2;
    let mut padded = (0..top).map(|_| Line::default()).collect::<Vec<_>>();
    padded.append(&mut lines);
    f.render_widget(Paragraph::new(padded), area);
}

/// 오른쪽 패널 — 선택 가능한 최근 세션 + 핵심 기능 요약.
fn draw_digest(f: &mut Frame, app: &App, area: Rect, palette: &Pal) {
    let mut lines: Vec<Line> = Vec::new();
    let width = area.width.saturating_sub(2) as usize;

    lines.push(Line::from(Span::styled(
        "최근 세션",
        Style::default()
            .fg(palette.secondary)
            .add_modifier(Modifier::BOLD),
    )));
    if app.recent_sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "아직 세션이 없습니다 — 첫 대화를 시작하세요",
            Style::default().fg(palette.mute),
        )));
    } else {
        for (index, session) in app.recent_sessions.iter().take(3).enumerate() {
            let selected = index == app.start_session_sel;
            let (marker, style) = if selected {
                (
                    " ▸ ",
                    Style::default()
                        .fg(palette.accent)
                        .bg(palette.panel)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (" › ", Style::default().fg(palette.text))
            };
            lines.push(truncated_line(marker, &session.title, width, style));
        }
        lines.push(Line::from(Span::styled(
            "↑↓ 선택 · Enter 이어하기 · /sessions 전체",
            Style::default().fg(palette.mute),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "핵심 기능",
        Style::default()
            .fg(palette.secondary)
            .add_modifier(Modifier::BOLD),
    )));
    for feature in digest_features() {
        lines.push(truncated_line(
            " · ",
            feature,
            width,
            Style::default().fg(palette.body),
        ));
    }

    let top = (area.height.saturating_sub(lines.len() as u16)) / 2;
    let mut padded = (0..top).map(|_| Line::default()).collect::<Vec<_>>();
    padded.append(&mut lines);
    f.render_widget(Paragraph::new(padded), area);
}

fn digest_features() -> [&'static str; 4] {
    [
        "맥락→계획→실행→검증 4단계 Harness",
        "파일 편집·검색·LSP 진단·웹·MCP 도구",
        "교훈·사실 기억으로 같은 실수 반복 방지",
        "모델 카탈로그 하루 1회 자동 갱신",
    ]
}

fn truncated_line(marker: &str, text: &str, width: usize, style: Style) -> Line<'static> {
    let budget = width.saturating_sub(marker.chars().count());
    let shown: String = if display_width(text) <= budget {
        text.to_string()
    } else {
        let mut out = String::new();
        let mut used = 0usize;
        for ch in text.chars() {
            let w = if ch.is_ascii() { 1 } else { 2 };
            if used + w + 1 > budget {
                break;
            }
            out.push(ch);
            used += w;
        }
        out.push('…');
        out
    };
    Line::from(vec![
        Span::styled(marker.to_string(), style),
        Span::styled(shown, style),
    ])
}

/// 대략적 표시 폭 — 한글 등 비 ASCII 는 2칸으로 계산 (잘림 방지용 추정).
fn display_width(text: &str) -> usize {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

// ---------------------------------------------------------------------------
// 하단 — 기존에 표시되던 정보 (분할 모드에선 RECENT 를 오른쪽 패널이 대신한다).
// ---------------------------------------------------------------------------

fn bottom_lines(app: &App, palette: &Pal, include_recent: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
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
    lines.push(Line::from(Span::styled(
        "맥락을 모으고, 계획하고, 실행하고, 검증합니다.",
        Style::default().fg(palette.body),
    )));
    lines.push(Line::default());
    lines.push(metadata_line("MODEL", &app.binding, palette));
    lines.push(metadata_line("WORKSPACE", &app.cwd, palette));
    if include_recent && !app.recent_sessions.is_empty() {
        let joined = app
            .recent_sessions
            .iter()
            .map(|s| s.title.as_str())
            .collect::<Vec<_>>()
            .join("  ›  ");
        lines.push(metadata_line("RECENT", &joined, palette));
    }
    if let Some(tip) = &app.start_tip {
        // 세션당 1줄 — App 생성 시 고정된 팁 (F9). /tips off 로 영구 해제.
        lines.push(metadata_line("TIP", &format!("{tip}  ·  /tips"), palette));
    }
    lines.push(Line::from(Span::styled(
        if configured(app) {
            "Enter 실행  ·  Tab plan/build  ·  Ctrl+T 파일 탐색기  ·  /sessions 세션  ·  ? 도움말"
        } else {
            "/connect 로 모델 연결"
        },
        Style::default().fg(palette.mute),
    )));
    lines
}

// ---------------------------------------------------------------------------
// 좁은 터미널 폴백 — 기존 중앙 배치 그대로.
// ---------------------------------------------------------------------------

fn draw_centered(f: &mut Frame, app: &App, area: Rect, palette: &Pal) {
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
        let joined = app
            .recent_sessions
            .iter()
            .map(|s| s.title.as_str())
            .collect::<Vec<_>>()
            .join("  ›  ");
        lines.push(metadata_line("RECENT", &joined, palette));
    }
    if let Some(tip) = &app.start_tip {
        lines.push(metadata_line("TIP", &format!("{tip}  ·  /tips"), palette));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette;
    use crate::tui::clamp_selection;
    use crate::tui::view::pal_of;

    #[test]
    fn glyphs_are_uniform_seven_rows() {
        for (ch, rows) in GLYPHS {
            assert_eq!(rows.len(), 7, "{ch} 행 수");
            let width = rows[0].chars().count();
            for row in rows {
                assert_eq!(row.chars().count(), width, "{ch} 행 폭 불일치: {row}");
                assert!(
                    row.chars().all(|c| c == '█' || c == ' '),
                    "{ch} 예상 밖 문자: {row}"
                );
            }
        }
        // 로고 구성 — 마크 3요소 + 워드마크 3글자.
        let keys: Vec<char> = GLYPHS.iter().map(|(c, _)| *c).collect();
        assert_eq!(keys, vec!['{', '/', '}', 'R', 'I', 'X']);
        let brace_left = GLYPHS.iter().find(|(c, _)| *c == '{').unwrap().1;
        assert_eq!(brace_left[3], "██ ", "{{ 중앙 돌기는 왼쪽(바깥)");
        let brace_right = GLYPHS.iter().find(|(c, _)| *c == '}').unwrap().1;
        assert_eq!(brace_right[3], " ██", "}} 중앙 돌기는 오른쪽(바깥)");
    }

    #[test]
    fn marquee_moves_and_fills_width() {
        let a = marquee_line(0, 20);
        let b = marquee_line(1, 20);
        assert_eq!(a.chars().count(), 20);
        assert_ne!(a, b, "틱이 다르면 흘러간다");
        assert!(
            marquee_line(0, MARQUEE_CYCLE.chars().count()).contains("RAFIKX HARNESS"),
            "순환 문구에 HARNESS 포함"
        );
        assert_eq!(marquee_line(0, 0), "");
        // 다중 바이트 문자가 깨지지 않는다 — 순환 문구의 '·' 가 그대로 나온다.
        assert!(marquee_line(0, 60).contains('·'), "UTF-8 보존");
    }

    #[test]
    fn boot_reveals_rows_progressively() {
        assert_eq!(visible_rows_for(0, false), 1, "부팅 첫 틱 — 1행만");
        assert_eq!(visible_rows_for(12, false), 7, "부팅 끝 — 전체 7행");
        assert_eq!(visible_rows_for(40, false), 7, "부팅 종료 후 — 전체");
        assert_eq!(visible_rows_for(0, true), 7, "reduced_motion — 즉시 전체");
    }

    #[test]
    fn banner_fill_is_theme_gradient_between_accent_and_code() {
        let th = pal_of(&palette::RAFIKX);
        assert_eq!(banner_fill(&th, 0.0), th.accent, "t=0 — accent");
        assert_eq!(banner_fill(&th, 1.0), th.code, "t=1 — code");
        assert_ne!(banner_fill(&th, 0.5), th.accent, "중간은 혼합색");
    }

    #[test]
    fn lockup_is_mark_plus_word() {
        // 락업 구성 — 콤팩트 마크 { / } + 간격 + RIX.
        assert_eq!(MARK, "{/}");
        assert_eq!(WORD, "RIX");
        assert_eq!(MARK_W, 3 + MARK_GAP + 4 + MARK_GAP + 3);
        assert_eq!(WORD_W, 17);
        assert_eq!(LOCKUP_W, MARK_W + GROUP_GAP + WORD_W);
        // 로고 키 집합 — 마크 3요소와 워드마크 3글자.
        let keys: Vec<char> = GLYPHS.iter().map(|(c, _)| *c).collect();
        assert_eq!(keys, vec!['{', '/', '}', 'R', 'I', 'X']);
    }

    #[test]
    fn session_selection_stays_in_bounds() {
        assert_eq!(clamp_selection(0, -1, 4), 0, "위 끝에서 더 올라가지 않는다");
        assert_eq!(clamp_selection(0, 1, 4), 1);
        assert_eq!(clamp_selection(3, 1, 4), 3, "아래 끝 고정");
        assert_eq!(clamp_selection(2, 0, 4), 2, "이동 없음");
        assert_eq!(clamp_selection(0, 1, 0), 0, "빈 목록");
    }
}
