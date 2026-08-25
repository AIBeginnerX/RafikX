use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::md::{markdown_segs, MdKind};
use super::{App, EntryKind};
use crate::palette::{self, Theme};

/// ratatui 색으로 변환한 팔레트.
pub struct Pal {
    pub bg: Color,
    pub accent: Color,
    pub secondary: Color,
    pub code: Color,
    pub text: Color,
    pub body: Color,
    pub mute: Color,
    pub warn: Color,
    pub err: Color,
    pub kw: Color,
    pub panel: Color,
}

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

pub fn theme_of(app: &App) -> Pal {
    let t: &Theme = {
        let name = app.session.cfg.file.ui.theme.as_str();
        if name.is_empty() {
            &palette::RAFIKX
        } else {
            palette::by_name(name)
        }
    };
    Pal {
        bg: rgb(t.bg),
        accent: rgb(t.accent),
        secondary: rgb(t.secondary),
        code: rgb(t.code),
        text: rgb(t.text),
        body: rgb(t.body),
        mute: rgb(t.mute),
        warn: rgb(t.warn),
        err: rgb(t.err),
        kw: rgb(t.kw),
        panel: rgb(t.panel),
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let th = theme_of(app);
    let area = f.area();
    f.render_widget(
        Block::default().style(Style::default().bg(th.bg).fg(th.accent)),
        area,
    );

    // 입력이 "/" 로 시작하면 하단에 명령 팔레트를 깐다 (최대 5개 + 총 개수).
    let slash_hits = slash_matches(app);
    let pal_h = if slash_hits.is_empty() {
        0u16
    } else {
        (slash_hits.len().min(9) + 1) as u16
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(input_height(app, area.width)),
            Constraint::Length(pal_h),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, app, chunks[0], &th);
    draw_transcript_frame(f, app, chunks[1], &th);
    draw_input(f, app, chunks[2], &th);
    if pal_h > 0 {
        draw_slash_palette(f, chunks[3], &slash_hits, &th);
    }
    draw_status_strip(f, app, chunks[4], &th);
    draw_footer(f, app, chunks[5], &th);

    if app.help {
        draw_overlay(f, area, "키", super::md::KEY_HELP, &th);
    }
    if let Some(p) = &app.picker {
        draw_picker(f, area, p, &th);
    }
    if let Some(t) = &app.text {
        let field = draw_field_overlay(
            f,
            area,
            &t.title,
            "",
            &t.hint,
            if t.buf.is_empty() { "" } else { &t.buf },
            t.buf.is_empty(),
            false,
            &th,
        );
        if let Some(inner) = field {
            place_cursor(f, inner, &t.buf, t.buf.len());
        }
    }
    if let Some(s) = &app.secret {
        let url = crate::accounts_ui::auth_console_url(&s.provider).unwrap_or("");
        let env = crate::auth::env_hint(&app.session.cfg, &s.provider);
        let header = format!(
            "{}\n{}\n환경변수  {env}",
            crate::auth::provider_label(&s.provider),
            if url.is_empty() {
                String::new()
            } else {
                format!("키 발급  {url}")
            }
        );
        let shown = if s.buf.is_empty() {
            String::new()
        } else {
            crate::accounts_ui::mask_secret(&s.buf)
        };
        let field = draw_field_overlay(
            f,
            area,
            "API 키",
            &header,
            "키를 붙여넣으세요 (Ctrl+V) · Enter 저장 · Esc 취소",
            &shown,
            s.buf.is_empty(),
            true,
            &th,
        );
        if let Some(inner) = field {
            place_cursor(f, inner, &shown, shown.len());
        }
    }
    if let Some(c) = &app.confirm {
        draw_overlay(f, area, &c.title, &c.body, &th);
    }
    if let Some(p) = &app.approval {
        let body = format!(
            "{}\n\n[y] 이번만   [n] 거부   [a] 이번 실행 모두",
            p.preview
        );
        draw_overlay(f, area, "도구 승인", &body, &th);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    let version = env!("CARGO_PKG_VERSION");
    let spans = vec![
        Span::styled(
            " RAFIKX ",
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" v{version} "), Style::default().fg(th.mute)),
        Span::raw(" "),
        // 하네스 정보 (모델명 제외 — 자동/수동 + 엔진)
        Span::styled(
            format!(
                "하네스 {} · {}",
                if app.session.cfg.file.harness.selection.eq_ignore_ascii_case("manual") {
                    "수동"
                } else {
                    "자동"
                },
                if app.session.cfg.file.general.engine.eq_ignore_ascii_case("deepseek") {
                    "deepseek"
                } else {
                    "rafikx"
                }
            ),
            Style::default().fg(th.code),
        ),
        Span::raw("  "),
        // 프로젝트 경로
        Span::styled(&app.cwd, Style::default().fg(th.secondary)),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(th.bg)), area);
}

/// 대화 영역을 테두리로 감싼 뒤 내부에 트랜스크립트를 그린다.
fn draw_transcript_frame(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.mute))
        .style(Style::default().bg(th.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);
    draw_transcript(f, app, inner, &th);
}

fn draw_transcript(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    let mut lines: Vec<Line> = Vec::new();
    for e in &app.transcript {
        let text = if e.kind == EntryKind::Assistant {
            compact_blank(&hide_thinking(&e.text))
        } else {
            e.text.clone()
        };
        if text.trim().is_empty() && e.kind == EntryKind::Assistant {
            continue;
        }
        let (tag, style) = match e.kind {
            EntryKind::User => (
                "you",
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
            ),
            EntryKind::Queued => ("wait", Style::default().fg(th.mute)),
            EntryKind::Assistant => (
                "rafikx",
                Style::default().fg(th.code).add_modifier(Modifier::BOLD),
            ),
            EntryKind::System => ("sys", Style::default().fg(th.mute)),
            EntryKind::Tool => ("tool", Style::default().fg(th.secondary)),
            EntryKind::Warn => (
                "!",
                Style::default()
                    .fg(th.err)
                    .add_modifier(Modifier::BOLD),
            ),
        };
        // 컴팩트 레이아웃: 태그를 첫 줄에 인라인으로 붙이고 이후 줄은 세로선만.
        let tag_pad = format!("{tag:<6}");
        let mut first = true;
        let push_row = |lines: &mut Vec<Line>, text: &str, st: Style, first: &mut bool| {
            if *first {
                lines.push(Line::from(vec![
                    Span::styled(format!(" {tag_pad}|"), style),
                    Span::styled(format!(" {text}"), st),
                ]));
                *first = false;
            } else {
                lines.push(Line::from(vec![
                    Span::styled("        |", Style::default().fg(th.mute)),
                    Span::styled(format!(" {text}"), st),
                ]));
            }
        };

        if e.kind == EntryKind::Assistant {
            // 반응형 표 — 채팅창 폭에 맞춰 셀 자동 줄바꿈
            let segs = crate::tui::md::markdown_segs_with_width(&text, Some(area.width.max(24) as usize));
            for seg in &segs {
                // 코드블록은 oh-my-pi 스타일 syntax highlighting 으로 그린다.
                if seg.kind == MdKind::CodeBlock {
                    push_code_rows(
                        &mut lines,
                        &seg.text,
                        seg.lang.as_deref(),
                        th,
                        &tag_pad,
                        &mut first,
                    );
                    continue;
                }
                let st = match seg.kind {
                    MdKind::Heading => Style::default()
                        .fg(th.accent)
                        .add_modifier(Modifier::BOLD),
                    MdKind::Emphasis => Style::default()
                        .fg(th.accent)
                        .add_modifier(Modifier::BOLD),
                    MdKind::Code | MdKind::CodeBlock | MdKind::Table | MdKind::Chart => {
                        // oh-my-pi 스타일 — 코드/표/차트를 어두운 패널 배경으로 본문과 분리한다.
                        Style::default().fg(th.code).bg(th.panel)
                    }
                    MdKind::Command => Style::default()
                        .fg(th.secondary)
                        .add_modifier(Modifier::BOLD),
                    MdKind::Text => Style::default().fg(th.text),
                };
                for piece in seg.text.split('\n') {
                    push_row(&mut lines, piece, st, &mut first);
                }
            }
        } else if e.kind == EntryKind::System {
            // 시스템 안내는 태그 없이 들여쓰기만 (sys 접두어가 거슬리므로)
            for row in text.split('\n') {
                lines.push(Line::from(Span::styled(
                    format!("   · {row}"),
                    Style::default().fg(th.mute),
                )));
            }
        } else {
            let failed_tool = e.kind == EntryKind::Tool && text.contains("도구 오류");
            if matches!(e.kind, EntryKind::Warn) || failed_tool {
                // 에러는 붉은 계열 본문 + 중요 단어만 다크엘로우 굵게.
                for row in text.split('\n') {
                    let mut spans: Vec<Span> = Vec::new();
                    if first {
                        spans.push(Span::styled(format!(" {tag_pad}|"), style));
                        first = false;
                    } else {
                        spans.push(Span::styled(
                            "        |",
                            Style::default().fg(th.mute),
                        ));
                    }
                    append_alert_spans(&mut spans, row, th);
                    lines.push(Line::from(spans));
                }
            } else {
                let st = Style::default().fg(th.body);
                for row in text.split('\n') {
                    push_row(&mut lines, row, st, &mut first);
                }
            }
        }
        // 빈 줄은 도구·경고 뒤에만 — 대화 본문은 태그 색으로 구분해 밀도를 높인다.
        if matches!(e.kind, EntryKind::Tool | EntryKind::Warn) {
            lines.push(Line::from(""));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  할 일을 말하면 실행합니다.  ?  키 도움말",
            Style::default().fg(th.mute),
        )));
    }

    // ratatui 의 Wrap+scroll 은 논리 줄 기준이라 자동 줄바꿈된 긴 답변에서
    // 내용이 건너뛰어져(검은 영역·윗줄만 표시) 보이는 문제가 있다.
    // 모든 논리 줄을 그리기 전에 비주얼 행으로 직접 자르고 Wrap 없이 윈도잉한다.
    let width = area.width.max(1) as usize;
    let mut visual: Vec<Line> = Vec::with_capacity(lines.len());
    for l in &lines {
        if wrapped_rows(l, area.width) <= 1 {
            visual.push(l.clone());
        } else {
            for row in wrap_spans(&l.spans, width) {
                visual.push(Line::from(row));
            }
        }
    }
    let total = visual.len() as u16;
    let vis = area.height;
    let max_scroll = total.saturating_sub(vis);
    // app.scroll 은 「바닥에서 몇 줄 위」 — ↑(휠 업)으로 늘려 과거를 보고,
    // 0 이 되면 자동 follow 로 복귀한다.
    let off = if app.follow {
        max_scroll
    } else {
        max_scroll.saturating_sub(app.scroll.min(max_scroll))
    };
    let start = off as usize;
    let end = (start + vis as usize).min(visual.len());
    let visible: Vec<Line> = visual[start..end].to_vec();

    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().bg(th.bg));
    f.render_widget(
        Paragraph::new(visible).block(block),
        area,
    );
}

/// 스팬 열을 지정 폭의 비주얼 행들로 자른다 (스타일 유지).
fn wrap_spans(spans: &[Span], width: usize) -> Vec<Vec<Span<'static>>> {
    #[derive(Clone)]
    struct Piece {
        text: String,
        fg: Color,
        bg: Option<Color>,
        bold: bool,
    }
    let mut rows: Vec<Vec<Piece>> = Vec::new();
    let mut cur: Vec<Piece> = Vec::new();
    let mut cur_w = 0usize;

    let flush = |cur: &mut Vec<Piece>, rows: &mut Vec<Vec<Piece>>| {
        let taken = std::mem::take(cur);
        if taken.is_empty() {
            rows.push(vec![Piece {
                text: " ".into(),
                fg: Color::White,
                bg: None,
                bold: false,
            }]);
        } else {
            rows.push(taken);
        }
    };

    for sp in spans {
        let fg = match sp.style.fg {
            Some(c) => c,
            None => Color::Reset,
        };
        let bg = sp.style.bg;
        let bold = sp
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD);
        for ch in sp.content.chars() {
            let cw = super::md::ch_width(ch).max(1);
            if cur_w + cw > width && cur_w > 0 {
                flush(&mut cur, &mut rows);
                cur_w = 0;
            }
            match cur.last_mut() {
                Some(p) if p.fg == fg && p.bg == bg && p.bold == bold => p.text.push(ch),
                _ => cur.push(Piece {
                    text: ch.to_string(),
                    fg,
                    bg,
                    bold,
                }),
            }
            cur_w += cw;
        }
    }
    flush(&mut cur, &mut rows);

    rows.into_iter()
        .map(|pieces| {
            pieces
                .into_iter()
                .map(|p| {
                    let mut st = Style::default().fg(p.fg);
                    if let Some(bg) = p.bg {
                        st = st.bg(bg);
                    }
                    if p.bold {
                        st = st.add_modifier(Modifier::BOLD);
                    }
                    Span::styled(p.text, st)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn input_height(app: &App, width: u16) -> u16 {
    let w = width.saturating_sub(2).max(8) as usize;
    let rows = super::md::wrap_text(&app.input, w).len().max(1);
    (rows as u16).clamp(1, 8)
}

/// Paragraph.wrap 이 접은 행까지 포함한 실제 렌더 행 수 — 스크롤 계산용.
fn wrapped_rows(line: &Line<'_>, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let w: usize = line
        .spans
        .iter()
        .map(|s| s.content.chars().map(super::md::ch_width).sum::<usize>())
        .sum();
    (w.div_ceil(width as usize)).max(1) as u16
}

const THINK_OPEN: &[&str] = &["<think>", "<thinking>"];
const THINK_CLOSE: &[&str] = &["</think>", "</thinking>"];

/// 모델이 단락 구분용으로 남기는 빈 줄을 코드블록 안에서만 보존하고 밖에서는 제거해
/// 화면 밀도를 높인다.
fn compact_blank(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_fence = false;
    for line in s.split('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_fence {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// 오류·경고 본문의 중요 단어 사전 — 붉은 본문 위에 다크엘로우 굵게 칠한다.
const ALERT_KEYWORDS: &[&str] = &[
    "오류", "실패", "거부", "중단", "경고", "위험",
    "error", "Error", "ERROR",
    "failed", "Failed", "FAILED", "fail",
    "warning", "Warning",
    "denied", "refused", "timeout",
];

fn append_alert_spans(spans: &mut Vec<Span<'static>>, text: &str, th: &Pal) {
    let err_style = Style::default().fg(th.err);
    let kw_style = Style::default()
        .fg(th.kw)
        .add_modifier(Modifier::BOLD);
    let lower = text.to_lowercase();
    let mut rest_start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let mut hit: Option<(usize, usize)> = None;
        for kw in ALERT_KEYWORDS {
            if lower[i..].starts_with(kw) {
                hit = Some((i, kw.len()));
                break;
            }
        }
        match hit {
            Some((p, l)) => {
                if p > rest_start {
                    spans.push(Span::styled(
                        text[rest_start..p].to_string(),
                        err_style,
                    ));
                }
                spans.push(Span::styled(text[p..p + l].to_string(), kw_style));
                i = p + l;
                rest_start = i;
            }
            None => {
                // UTF-8 경계를 넘지 않게 다음 문자 폭만큼 진행한다.
                i += text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
        }
    }
    if rest_start < text.len() {
        spans.push(Span::styled(text[rest_start..].to_string(), err_style));
    }
}

/// 코드블록을 syntax highlighting 한 뒤 트랜스크립트 행으로 붙인다.
fn push_code_rows(
    lines: &mut Vec<Line<'static>>,
    code: &str,
    lang: Option<&str>,
    th: &Pal,
    tag_pad: &str,
    first: &mut bool,
) {
    for row_spans in highlight_code(code, lang, th) {
        let mut all: Vec<Span> = Vec::with_capacity(row_spans.len() + 1);
        if *first {
            all.push(Span::styled(
                format!(" {tag_pad}|"),
                Style::default().fg(th.code).add_modifier(Modifier::BOLD),
            ));
            *first = false;
        } else {
            all.push(Span::styled("        |", Style::default().fg(th.mute)));
        }
        all.extend(row_spans);
        lines.push(Line::from(all));
    }
}

/// syntect 로 토큰별 착색한 코드 행 — 패널 배경 위에 oh-my-pi 처럼 색이 입혀진다.
fn highlight_code(code: &str, lang: Option<&str>, th: &Pal) -> Vec<Vec<Span<'static>>> {
    use std::sync::OnceLock;
    use syntect::easy::HighlightLines;
    use syntect::highlighting::ThemeSet;
    use syntect::parsing::SyntaxSet;

    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    let ss = SYNTAX_SET.get_or_init(syntect::parsing::SyntaxSet::load_defaults_nonewlines);
    let ts = THEMES.get_or_init(syntect::highlighting::ThemeSet::load_defaults);
    let theme = ts
        .themes
        .get("base16-ocean.dark")
        .or_else(|| ts.themes.values().next());
    let Some(theme) = theme else {
        return vec![vec![Span::styled(
            code.to_string(),
            Style::default().fg(th.code).bg(th.panel),
        )]];
    };
    let syntax = lang
        .and_then(|l| ss.find_syntax_by_token(l))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let mut hl = HighlightLines::new(syntax, theme);

    let mut out: Vec<Vec<Span<'static>>> = Vec::new();
    for row in code.split('\n') {
        let ranges = match hl.highlight_line(row, ss) {
            Ok(r) => r,
            Err(_) => {
                return vec![vec![Span::styled(
                    code.to_string(),
                    Style::default().fg(th.code).bg(th.panel),
                )]];
            }
        };
        let mut spans: Vec<Span> = Vec::with_capacity(ranges.len() + 1);
        for (style, chunk) in ranges {
            if chunk.is_empty() {
                continue;
            }
            spans.push(Span::styled(
                chunk.to_string(),
                Style::default()
                    .fg(Color::Rgb(
                        style.foreground.r,
                        style.foreground.g,
                        style.foreground.b,
                    ))
                    .bg(th.panel),
            ));
        }
        if spans.is_empty() {
            spans.push(Span::styled(" ".to_string(), Style::default().bg(th.panel)));
        }
        out.push(spans);
    }
    if out.is_empty() {
        out.push(vec![Span::styled(" ".to_string(), Style::default().bg(th.panel))]);
    }
    out
}

/// 모델의 사고 블록(<think>…</think>)을 화면에서 숨긴다. 미완결 블록도 숨김.
pub fn hide_thinking(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: loop {
        for open in THINK_OPEN {
            if let Some(i) = rest.find(open) {
                out.push_str(&rest[..i]);
                let after = &rest[i + open.len()..];
                for close in THINK_CLOSE {
                    if let Some(j) = after.find(close) {
                        rest = &after[j + close.len()..];
                        continue 'outer;
                    }
                }
                return out;
            }
        }
        out.push_str(rest);
        return out;
    }
}

fn draw_input(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    // opencode 스타일 — 테두리 없는 "> " 프롬프트. busy 중엔 "…" 로 상태를 표시한다.
    let (mark, mark_style) = if app.busy {
        ("…", Style::default().fg(th.mute))
    } else {
        (">", Style::default().fg(th.accent))
    };
    let body_style = Style::default()
        .fg(th.accent)
        .add_modifier(if app.busy {
            Modifier::empty()
        } else {
            Modifier::BOLD
        });
    let mut lines: Vec<Line> = Vec::new();
    let mut rows = app.input.split('\n');
    let first = rows.next().unwrap_or("");
    lines.push(Line::from(vec![
        Span::styled(format!("{mark} "), mark_style),
        Span::styled(first.to_string(), body_style),
    ]));
    for row in rows {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(row.to_string(), body_style),
        ]));
    }
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(th.bg)), area);

    if !app.busy
        && app.approval.is_none()
        && !app.help
        && app.picker.is_none()
        && app.secret.is_none()
        && app.text.is_none()
        && app.confirm.is_none()
    {
        let w = area.width.saturating_sub(4).max(1);
        let (x, y) = cursor_xy(&app.input, app.cursor, w);
        let cx = area.x.saturating_add(2u16.saturating_add(x));
        let cy = area.y.saturating_add(y);
        if cy < area.y.saturating_add(area.height) {
            f.set_cursor_position((cx, cy));
        }
    }
}

fn cursor_xy(text: &str, cursor: usize, width: u16) -> (u16, u16) {
    let cursor = cursor.min(text.len());
    let before = &text[..cursor];
    let mut x = 0u16;
    let mut y = 0u16;
    for ch in before.chars() {
        if ch == '\n' {
            y = y.saturating_add(1);
            x = 0;
            continue;
        }
        let cw = super::md::ch_width(ch) as u16;
        if width > 0 && x.saturating_add(cw) > width {
            y = y.saturating_add(1);
            x = cw;
        } else {
            x = x.saturating_add(cw);
        }
    }
    if width > 0 {
        x = x.min(width.saturating_sub(1));
    }
    (x, y)
}

/// 채팅창 아래 스트립 — [로고] RAFIKX │ [디지털바+Working] 실행 중 │ 작업 폴더
fn draw_status_strip(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let mut spans: Vec<Span> = Vec::new();
    // 선으로 그린 로고 마크 + 이름
    spans.push(Span::styled(
        " ◈ ",
        Style::default().fg(th.secondary),
    ));
    spans.push(Span::styled(
        "RAFIKX",
        Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(" │", Style::default().fg(th.mute)));

    if app.busy {
        // 작업 디지털 표시(진행바) → Working 모양(스피너)
        spans.push(Span::raw(" "));
        spans.extend(digital_bar(14));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as usize)
            .unwrap_or(0);
        let frame = SPINNER[(now / 120) % SPINNER.len()];
        spans.push(Span::styled(
            format!(" {frame} Working"),
            Style::default().fg(th.code).add_modifier(Modifier::BOLD),
        ));
        if let Some(phase) = crate::spinner::current_label() {
            spans.push(Span::styled(format!(" · {phase}"), Style::default().fg(th.mute)));
        }
    } else {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            "✓ Ready",
            Style::default().fg(th.mute),
        ));
    }

    // 선택된 모델의 컨텍스트 창 — 실행 전에도 즉시 확인 가능 (세션 재진입 포함)
    {
        let cfg = &app.session.cfg;
        let (pname, mname) = match (&app.session.provider, &app.session.model) {
            (Some(p), Some(m)) => (p.clone(), m.clone()),
            _ => {
                let dp = cfg.file.general.default_provider.clone();
                let dm = cfg
                    .provider(&dp)
                    .map(|x| x.model.clone())
                    .unwrap_or_default();
                (dp, dm)
            }
        };
        let win = if crate::harness::current_ctx_window() > 0 {
            crate::harness::current_ctx_window()
        } else {
            crate::packer::context_window_for(&pname, &mname, cfg.provider(&pname).ok())
        };
        if win > 0 {
            let fmt_k = |n: u32| -> String {
                if n >= 1_000_000 {
                    format!("{:.0}M", n as f64 / 1e6)
                } else if n >= 1_000 {
                    format!("{:.0}k", n as f64 / 1e3)
                } else {
                    n.to_string()
                }
            };
            spans.push(Span::styled(" │", Style::default().fg(th.mute)));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("ctx {}", fmt_k(win)),
                Style::default().fg(th.code),
            ));
        }
    }

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(th.bg)),
        area,
    );
}

/// 파란 계열 그라데이션의 디지털 바(사각형) — 어두운 베이스 위로 밝은 구간이 왕복.
fn digital_bar(width: usize) -> Vec<Span<'static>> {
    const BASE: (u8, u8, u8) = (24, 38, 96);
    const SHADES: [(u8, u8, u8); 6] = [
        (46, 82, 200),
        (66, 118, 240),
        (96, 156, 255),
        (140, 196, 255),
        (170, 212, 255),
        (210, 236, 255),
    ];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as usize)
        .unwrap_or(0);
    let width = width.max(6);
    let span_w = (width / 2).max(1);
    let period = (width + span_w) * 2;
    let t = (now / 90) % period;
    let head = if t < width + span_w { t } else { period - t };
    let mut out = vec![Span::styled(
        "[",
        Style::default().fg(Color::Rgb(70, 100, 200)),
    )];
    for i in 0..width {
        // head 위치에서 뒤로 span_w 길이만큼 밝게
        let bright = head >= span_w && i < head && i + span_w > head;
        if bright {
            let rel = (i - (head - span_w)) * SHADES.len() / span_w.max(1);
            let shade = SHADES[rel.min(SHADES.len() - 1)];
            out.push(Span::styled(
                "█".to_string(),
                Style::default().fg(Color::Rgb(shade.0, shade.1, shade.2)),
            ));
        } else {
            out.push(Span::styled(
                "█".to_string(),
                Style::default().fg(Color::Rgb(BASE.0, BASE.1, BASE.2)),
            ));
        }
    }
    out.push(Span::styled(
        "]",
        Style::default().fg(Color::Rgb(70, 100, 200)),
    ));
    out
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    let mut line_spans: Vec<Span> = Vec::new();
    // opencode footer 규격 — [모드 배지][spinner+상태][사용량][큐 힌트]
    let badge = if app.session.is_plan_mode() {
        Span::styled(
            " PLAN ",
            Style::default()
                .fg(th.bg)
                .bg(th.secondary)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " BUILD ",
            Style::default()
                .fg(th.bg)
                .bg(th.code)
                .add_modifier(Modifier::BOLD),
        )
    };
    line_spans.push(badge);
    line_spans.push(Span::raw(" "));

    // 사용량(in/out · ctx·캐시)을 우선 배치 — 상태 문자가 길어도 잘리지 않게
    if !app.tokens.is_empty() {
        line_spans.push(Span::styled(app.tokens.clone(), Style::default().fg(th.mute)));
        line_spans.push(Span::raw(" "));
    }
    if !app.ctx.is_empty() {
        line_spans.push(Span::styled(app.ctx.clone(), Style::default().fg(th.mute)));
        line_spans.push(Span::raw(" "));
    }

    if app.busy {
        // opencode 동일 — braille 스피너 1문자 + 상태. 사고 스트리밍 중엔 Working.
        let thinking = app
            .transcript
            .last()
            .filter(|e| e.kind == EntryKind::Assistant)
            .map(|e| hide_thinking(&e.text).trim().is_empty())
            .unwrap_or(false);
        if thinking {
            line_spans.push(Span::styled(
                box_spin().to_string(),
                Style::default().fg(th.kw).add_modifier(Modifier::BOLD),
            ));
            line_spans.push(Span::styled(
                " Working",
                Style::default()
                    .fg(th.mute)
                    .add_modifier(Modifier::ITALIC),
            ));
        } else {
            line_spans.push(Span::styled(
                braille_spin().to_string(),
                Style::default().fg(th.code),
            ));
            let phase = crate::spinner::current_label().unwrap_or_else(|| "실행 중".into());
            line_spans.push(Span::styled(format!(" {phase}"), Style::default().fg(th.code)));
        }
    } else {
        // 남은 폭을 계산해 상태 문자를 잘라 넣는다 — 뒤 항목이 화면 밖으로 밀리지 않게
        let used: usize = line_spans.iter().map(|sp| super::md::display_width(&sp.content)).sum();
        let budget = (area.width as usize).saturating_sub(used + 8).max(10);
        let short: String = app
            .status
            .chars()
            .take(budget)
            .collect::<String>();
        let ell = if app.status.chars().count() > budget { "…" } else { "" };
        line_spans.push(Span::styled(
            format!("{short}{ell}"),
            Style::default().fg(th.code),
        ));
    }

    if !app.queue.is_empty() {
        line_spans.push(Span::styled("  ·  ", Style::default().fg(th.mute)));
        line_spans.push(Span::styled(
            format!("대기 {}건", app.queue.len()),
            Style::default().fg(th.secondary),
        ));
    }

    f.render_widget(
        Paragraph::new(Line::from(line_spans)).style(Style::default().bg(th.bg)),
        area,
    );
}

/// 부팅 후 경과 밀리초 기반 프레임 인덱스 — ratatui 리드로우 주기와 무관하게 균등 회전.
fn spin_frame<'a>(frames: &[&'a str], interval_ms: u64) -> &'a str {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    frames[(ms / interval_ms) as usize % frames.len()]
}

/// opencode 동일 braille 10-frame (80ms).
fn braille_spin() -> &'static str {
    spin_frame(
        &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
        80,
    )
}

/// 사각형 도트가 도는 박스형 회전 — Working 표시용.
fn box_spin() -> &'static str {
    spin_frame(&["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"], 80)
}

/// 입력이 "/명령 접두어" 형태일 때 일치하는 명령 목록 (공백이 들어가면 숨긴다).
fn slash_matches(app: &App) -> Vec<&'static (&'static str, &'static str)> {
    let t = app.input.trim_start();
    if !t.starts_with('/') {
        return Vec::new();
    }
    // 인자를 입력하는 중에도 목록 유지 — 명령 토큰(첫 단어)만 검색
    let tok = t[1..]
        .split(char::is_whitespace)
        .next()
        .unwrap_or("")
        .to_lowercase();
    let tok = tok.as_str();
    let mut hits: Vec<&'static (&'static str, &'static str)> = crate::chat::SLASH_COMMANDS
        .iter()
        // 포함 검색 — "model" 을 쳐도 /model 이 잡힌다 (설명어도 검색 대상)
        .filter(|(name, desc)| {
            name[1..].to_lowercase().contains(tok) || desc.to_lowercase().contains(tok)
        })
        .collect();
    // 정확히 일치하는 명령은 맨 앞으로
    hits.sort_by_key(|(name, _)| name[1..].to_lowercase() != tok);
    if hits.is_empty() && !tok.is_empty() {
        // 검색 불일치 시에도 목록이 사라지지 않게 전체를 보여준다 (헤더에 불일치 표기)
        return crate::chat::SLASH_COMMANDS.iter().collect();
    }
    hits
}

fn draw_slash_palette(
    f: &mut Frame,
    area: Rect,
    hits: &[&'static (&'static str, &'static str)],
    th: &Pal,
) {
    let total = hits.len();
    let mut header = format!(" 명령 {total}개");
    if total > 9 {
        header.push_str(" · 접두어를 더 입력하면 좁혀집니다");
    }
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        header,
        Style::default().fg(th.mute),
    )));
    for (name, desc) in hits.iter().take(9) {
        lines.push(Line::from(vec![
            Span::styled(format!(" {name}"), Style::default().fg(th.accent)),
            Span::styled(format!("  {desc}"), Style::default().fg(th.mute)),
        ]));
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(th.secondary))
        .style(Style::default().bg(th.bg));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_picker(f: &mut Frame, area: Rect, picker: &super::Picker, th: &Pal) {
    // 검색어로 걸러진 항목만 보여준다 (번호 없음).
    let q = picker.query.trim().to_lowercase();
    let idxs: Vec<usize> = picker
        .items
        .iter()
        .enumerate()
        .filter(|(_, it)| q.is_empty() || it.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect();
    let total = idxs.len();
    let vis = 14usize;
    let sel = picker.selected.min(total.saturating_sub(1));
    let start = sel.saturating_sub(vis / 2);
    let end = (start + vis).min(total);
    let start = end.saturating_sub(vis).min(start);

    let mut lines = Vec::new();
    if q.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" {}개 · 입력하면 검색", total),
            Style::default().fg(th.mute),
        )));
    } else {
        lines.push(Line::from(vec![
            Span::styled("검색: ", Style::default().fg(th.mute)),
            Span::styled(picker.query.clone(), Style::default().fg(th.accent)),
            Span::raw("_"),
            Span::styled(format!("  ({total}개 일치)"), Style::default().fg(th.mute)),
        ]));
    }
    for i in start..end {
        let orig = idxs[i];
        let mark = if i == sel { "> " } else { "  " };
        lines.push(Line::from(Span::styled(
            format!("{mark}{}", picker.items[orig]),
            Style::default().fg(if i == sel { th.accent } else { th.body }),
        )));
    }
    if total == 0 {
        lines.push(Line::from(Span::styled(
            "  일치하는 항목이 없습니다",
            Style::default().fg(th.warn),
        )));
    }
    lines.push(Line::from(""));
    let extra = if picker.kind == super::PickerKind::Manage {
        "입력 검색  ↑↓ 이동  Enter 선택  Ctrl+E 키수정  Ctrl+D 삭제  Esc 취소"
    } else {
        "입력 검색  ↑↓ 이동  Enter 선택  Esc 취소"
    };
    lines.push(extra.into());
    let body = lines
        .into_iter()
        .map(|l| {
            let mut out = String::new();
            for span in l.spans {
                out.push_str(&span.content);
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n");
    let shown = (end - start + 2).min(18) as u16;
    draw_overlay_sized(f, area, &picker.title, &body, 80, shown.saturating_add(5), th);
}

#[allow(clippy::too_many_arguments)]
fn draw_field_overlay(
    f: &mut Frame,
    area: Rect,
    title: &str,
    header: &str,
    hint: &str,
    field: &str,
    empty: bool,
    _masked: bool,
    th: &Pal,
) -> Option<Rect> {
    let w = area.width.saturating_sub(8).min(76).max(36);
    let h = 14u16.min(area.height.saturating_sub(4)).max(10);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(th.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(th.code))
        .style(Style::default().bg(th.bg).fg(th.accent));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .split(inner);

    let mut head = Vec::new();
    for row in header.lines() {
        if !row.is_empty() {
            head.push(Line::from(Span::styled(row, Style::default().fg(th.code))));
        }
    }
    if head.is_empty() {
        head.push(Line::from(""));
    }
    f.render_widget(Paragraph::new(head).wrap(Wrap { trim: false }), chunks[0]);

    let placeholder = if _masked {
        "키를 붙여넣으세요 (Ctrl+V)"
    } else {
        "여기에 입력 · Ctrl+V"
    };
    let shown = if empty {
        Span::styled(placeholder, Style::default().fg(th.mute))
    } else {
        Span::styled(
            field,
            Style::default()
                .fg(th.accent)
                .add_modifier(Modifier::BOLD),
        )
    };
    let field_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.accent))
        .title(Span::styled(" 입력 ", Style::default().fg(th.accent)));
    let field_inner = field_block.inner(chunks[1]);
    f.render_widget(field_block, chunks[1]);
    f.render_widget(Paragraph::new(Line::from(shown)), field_inner);

    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(th.secondary))),
        chunks[2],
    );
    Some(field_inner)
}

fn place_cursor(f: &mut Frame, inner: Rect, text: &str, cursor: usize) {
    let w = inner.width.max(1);
    let (cx, cy) = cursor_xy(text, cursor.min(text.len()), w);
    let x = inner.x.saturating_add(cx);
    let y = inner.y.saturating_add(cy);
    if y < inner.y.saturating_add(inner.height) {
        f.set_cursor_position((x, y));
    }
}

fn draw_overlay(f: &mut Frame, area: Rect, title: &str, body: &str, th: &Pal) {
    draw_overlay_sized(f, area, title, body, 72, 22, th);
}

fn draw_overlay_sized(
    f: &mut Frame,
    area: Rect,
    title: &str,
    body: &str,
    max_w: u16,
    max_h: u16,
    th: &Pal,
) {
    let w = area.width.saturating_sub(8).min(max_w).max(24);
    let h = area.height.saturating_sub(4).min(max_h).max(8);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(th.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(th.accent))
        .style(Style::default().bg(th.bg).fg(th.accent));
    f.render_widget(
        Paragraph::new(body)
            .block(block)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(th.accent)),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hide_thinking_removes_complete_and_open_blocks() {
        assert_eq!(
            hide_thinking("before<think>secret</think>after"),
            "beforeafter"
        );
        // 스트리밍 중 닫히지 않은 블록도 숨긴다.
        assert_eq!(hide_thinking("before<think>still going"), "before");
    }

    #[test]
    fn hide_thinking_keeps_plain_text() {
        let s = "일반 답변입니다";
        assert_eq!(hide_thinking(s), s);
    }

    #[test]
    fn wrapped_rows_accounts_for_korean_width() {
        let line = Line::from("한글은 두 칸씩".to_string());
        assert_eq!(wrapped_rows(&line, 100), 1);
        assert!(wrapped_rows(&line, 4) >= 3);
    }
}
