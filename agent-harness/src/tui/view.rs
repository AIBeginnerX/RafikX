use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::md::{markdown_segs, MdKind};
use super::{App, EntryKind};

pub const GOLD: Color = Color::Rgb(232, 213, 163);
pub const VIOLET: Color = Color::Rgb(107, 92, 255);
pub const CYAN: Color = Color::Rgb(94, 231, 255);
pub const BG: Color = Color::Rgb(3, 6, 14);
pub const MUTE: Color = Color::Rgb(92, 97, 120);

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(
        Block::default().style(Style::default().bg(BG).fg(GOLD)),
        area,
    );

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(input_height(app, area.width)),
            Constraint::Length(2),
        ])
        .split(area);

    draw_header(f, app, chunks[0]);
    draw_transcript(f, app, chunks[1]);
    draw_input(f, app, chunks[2]);
    draw_footer(f, app, chunks[3]);

    if app.help {
        draw_overlay(f, area, "키", super::md::KEY_HELP);
    }
    if let Some(p) = &app.picker {
        draw_picker(f, area, p);
    }
    if let Some(t) = &app.text {
        let field = draw_field_overlay(
            f,
            area,
            &t.title,
            "",
            &t.hint,
            if t.buf.is_empty() {
                ""
            } else {
                &t.buf
            },
            t.buf.is_empty(),
            false,
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
        );
        if let Some(inner) = field {
            place_cursor(f, inner, &shown, shown.len());
        }
    }
    if let Some(c) = &app.confirm {
        draw_overlay(f, area, &c.title, &c.body);
    }
    if let Some(p) = &app.approval {
        let body = format!(
            "{}\n\n[y] 이번만   [n] 거부   [a] 이번 실행 모두",
            p.preview
        );
        draw_overlay(f, area, "도구 승인", &body);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            " RAFIKX ",
            Style::default()
                .fg(BG)
                .bg(GOLD)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  v0.1.0  ", Style::default().fg(MUTE)),
        Span::styled(&app.binding, Style::default().fg(CYAN)),
        Span::raw("  "),
        Span::styled(&app.cwd, Style::default().fg(VIOLET)),
    ]);
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(VIOLET))
        .style(Style::default().bg(BG));
    f.render_widget(Paragraph::new(title).block(block), area);
}

fn draw_transcript(f: &mut Frame, app: &App, area: Rect) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for e in &app.transcript {
        let (tag, style) = match e.kind {
            EntryKind::User => ("you", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            EntryKind::Assistant => ("rafikx", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
            EntryKind::System => ("sys", Style::default().fg(MUTE)),
            EntryKind::Tool => ("tool", Style::default().fg(VIOLET)),
            EntryKind::Warn => ("!", Style::default().fg(Color::Yellow)),
        };
        lines.push(Line::from(Span::styled(format!(" {tag}"), style)));
        if e.kind == EntryKind::Assistant {
            for seg in markdown_segs(&e.text) {
                let st = match seg.kind {
                    MdKind::Heading => Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                    MdKind::Emphasis => Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                    MdKind::Code | MdKind::CodeBlock => Style::default().fg(CYAN),
                    MdKind::Text => Style::default().fg(Color::White),
                };
                for piece in seg.text.split('\n') {
                    lines.push(Line::from(Span::styled(format!("  {piece}"), st)));
                }
            }
        } else {
            for row in e.text.split('\n') {
                lines.push(Line::from(Span::styled(
                    format!("  {row}"),
                    Style::default().fg(if e.kind == EntryKind::Warn {
                        Color::Yellow
                    } else {
                        Color::Gray
                    }),
                )));
            }
        }
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  할 일을 말하면 실행합니다.  ?  키 도움말",
            Style::default().fg(MUTE),
        )));
    }

    let total = lines.len() as u16;
    let vis = area.height.saturating_sub(1);
    let max_scroll = total.saturating_sub(vis);
    let scroll = if app.follow {
        max_scroll
    } else {
        app.scroll.min(max_scroll)
    };
    let _ = inner_w;

    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().bg(BG));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

fn input_height(app: &App, width: u16) -> u16 {
    let w = width.saturating_sub(4).max(8) as usize;
    let rows = super::md::wrap_text(&app.input, w).len().max(1);
    (rows as u16 + 2).clamp(3, 8)
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let title = if app.busy {
        " 실행 중 "
    } else {
        " 입력  Enter 전송  Ctrl+J 줄바꿈 "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.busy { MUTE } else { GOLD }))
        .title(Span::styled(title, Style::default().fg(GOLD)))
        .style(Style::default().bg(BG));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let para = Paragraph::new(app.input.as_str())
        .style(Style::default().fg(GOLD))
        .wrap(Wrap { trim: false });
    f.render_widget(para, inner);

    if !app.busy
        && app.approval.is_none()
        && !app.help
        && app.picker.is_none()
        && app.secret.is_none()
        && app.text.is_none()
        && app.confirm.is_none()
    {
        let w = inner.width.max(1);
        let (x, y) = cursor_xy(&app.input, app.cursor, w);
        let cx = inner.x.saturating_add(x);
        let cy = inner.y.saturating_add(y);
        if cy < inner.y.saturating_add(inner.height) {
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

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let hint = if app.busy {
        "실행 중"
    } else {
        "? 키   /connect 키등록   /help   Ctrl+C 종료"
    };
    let line = Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(&app.status, Style::default().fg(CYAN)),
        Span::raw("  "),
        Span::styled(&app.tokens, Style::default().fg(MUTE)),
        Span::raw("  "),
        Span::styled(hint, Style::default().fg(VIOLET)),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(BG)),
        area,
    );
}

fn draw_picker(f: &mut Frame, area: Rect, picker: &super::Picker) {
    let vis = 14usize;
    let start = picker.selected.saturating_sub(vis / 2);
    let end = (start + vis).min(picker.items.len());
    let start = end.saturating_sub(vis).min(start);
    let mut lines = Vec::new();
    for i in start..end {
        let mark = if i == picker.selected { "▸" } else { " " };
        let n = i + 1;
        lines.push(format!("{mark} [{n}] {}", picker.items[i]));
    }
    lines.push(String::new());
    let extra = if picker.kind == super::PickerKind::Manage {
        "↑↓ 이동  Enter 선택  e 키수정  d 삭제  Esc 취소"
    } else {
        "↑↓ 이동   Enter 선택   Esc 취소"
    };
    lines.push(extra.into());
    let body = lines.join("\n");
    let count = (end - start).min(16) as u16;
    draw_overlay_sized(f, area, &picker.title, &body, 80, count.saturating_add(6));
}

fn draw_field_overlay(
    f: &mut Frame,
    area: Rect,
    title: &str,
    header: &str,
    hint: &str,
    field: &str,
    empty: bool,
    _masked: bool,
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
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(CYAN))
        .style(Style::default().bg(BG).fg(GOLD));
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
            head.push(Line::from(Span::styled(row, Style::default().fg(CYAN))));
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
        Span::styled(placeholder, Style::default().fg(MUTE))
    } else {
        Span::styled(field, Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
    };
    let field_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(" 입력 ", Style::default().fg(GOLD)));
    let field_inner = field_block.inner(chunks[1]);
    f.render_widget(field_block, chunks[1]);
    f.render_widget(Paragraph::new(Line::from(shown)), field_inner);

    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(VIOLET))),
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

fn draw_overlay(f: &mut Frame, area: Rect, title: &str, body: &str) {
    draw_overlay_sized(f, area, title, body, 72, 22);
}

fn draw_overlay_sized(f: &mut Frame, area: Rect, title: &str, body: &str, max_w: u16, max_h: u16) {
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
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(GOLD))
        .style(Style::default().bg(BG).fg(GOLD));
    f.render_widget(
        Paragraph::new(body)
            .block(block)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(GOLD)),
        rect,
    );
}
