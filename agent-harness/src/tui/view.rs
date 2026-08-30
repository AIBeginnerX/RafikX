use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::md::MdKind;
use super::{App, EntryKind};
use crate::palette::{self, Theme};

const CONTINUATION_PREFIX: &str = "       ";

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
    pub success: Color,
    pub err: Color,
    pub kw: Color,
    pub panel: Color,
    pub border: Color,
    pub thinking: Color,
}

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

pub fn theme_of(app: &App) -> Pal {
    let name = app.session.cfg.file.ui.theme.as_str();
    pal_of(if name.is_empty() {
        &palette::RAFIKX
    } else {
        palette::by_name(name)
    })
}

pub(crate) fn pal_of(t: &Theme) -> Pal {
    Pal {
        bg: rgb(t.bg),
        accent: rgb(t.accent),
        secondary: rgb(t.secondary),
        code: rgb(t.code),
        text: rgb(t.text),
        body: rgb(t.body),
        mute: rgb(t.mute),
        warn: rgb(t.warn),
        success: rgb(t.success),
        err: rgb(t.err),
        kw: rgb(t.kw),
        panel: rgb(t.panel),
        border: rgb(t.border),
        thinking: rgb(t.thinking),
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
    let wanted_pal_h = if slash_hits.is_empty() {
        0u16
    } else {
        slash_palette_height(slash_hits.len())
    };
    let working = render_working_rows(&app.workers, &app.mode_line, area.width, &th);
    let rows = responsive_rows(
        area.width,
        area.height,
        todo_panel_height(app, area.width),
        wanted_pal_h,
        input_height(app, area.width),
        working_panel_height(working.len()),
    );

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(rows[0]),
            Constraint::Length(rows[1]),
            Constraint::Length(rows[2]),
            Constraint::Length(rows[3]),
            Constraint::Length(rows[4]),
            Constraint::Length(rows[5]),
            Constraint::Length(rows[6]),
        ])
        .split(area);

    draw_header(f, app, chunks[0], &th);
    if app.tree.is_some() {
        // 파일 탐색기가 열리면 대화 영역을 좌우로 나눈다 — 왼쪽 트리, 오른쪽 대화.
        let halves = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(chunks[1]);
        draw_file_tree(f, app, halves[0], &th);
        draw_transcript_frame(f, app, halves[1], &th);
    } else {
        draw_transcript_frame(f, app, chunks[1], &th);
    }
    if rows[2] > 0 {
        draw_todo_panel(f, app, chunks[2], &th);
    }
    draw_input(f, app, chunks[3], &th);
    if rows[4] > 0 {
        draw_slash_palette(f, chunks[4], &slash_hits, &th);
    }
    if rows[5] > 0 {
        draw_working_panel(f, chunks[5], working, &th);
    }
    draw_footer(f, app, chunks[6], &th);

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
    // 승인은 팝업 없이 인라인으로 — 미리보기는 트랜스크립트에, 선택은 푸터에서
    // Y/N/A 키로 (pi 스타일: 화면을 덮는 permission popup 을 두지 않는다).
}

/// 파일 탐색기 패널 — 위 트리, 아래 선택 파일 미리보기(최대 40줄).
fn draw_file_tree(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    let Some(tree) = &app.tree else { return };
    let halves = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let list = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.accent))
        .title(Span::styled(
            " 파일 탐색기 (Ctrl+T 닫기) ",
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = list.inner(halves[0]);
    f.render_widget(list, halves[0]);

    let visible = inner.height.max(1) as usize;
    // 커서가 항상 화면 안에 오도록 오프셋을 계산한다.
    let offset = tree.cursor.saturating_sub(visible.saturating_sub(1));
    let mut lines: Vec<Line> = Vec::new();
    for (index, row) in tree.rows.iter().enumerate().skip(offset) {
        if lines.len() >= visible {
            break;
        }
        let indent = "  ".repeat(row.depth);
        let marker = if row.is_dir {
            if row.expanded { "▾ " } else { "▸ " }
        } else {
            "  "
        };
        let text = format!("{indent}{marker}{}", row.name);
        let style = if index == tree.cursor {
            Style::default()
                .bg(th.panel)
                .fg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else if row.is_dir {
            Style::default().fg(th.secondary)
        } else {
            Style::default().fg(th.body)
        };
        lines.push(Line::from(Span::styled(text, style)));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "(비어 있음)",
            Style::default().fg(th.mute),
        )));
    }
    f.render_widget(Paragraph::new(lines).scroll((0, 0)), inner);

    let (preview_title, preview_lines) = match &tree.preview {
        Some(p) => (
            format!(" {} ", p.name),
            p.lines.iter().cloned().collect::<Vec<_>>(),
        ),
        None => (" 미리보기 ".to_string(), Vec::new()),
    };
    let preview_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.mute))
        .title(Span::styled(preview_title, Style::default().fg(th.mute)));
    let preview_inner = preview_block.inner(halves[1]);
    f.render_widget(preview_block, halves[1]);
    if preview_lines.is_empty() {
        f.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(
                "파일을 선택하세요 (Enter)",
                Style::default().fg(th.mute),
            ))]),
            preview_inner,
        );
    } else {
        let rows: Vec<Line> = preview_lines
            .into_iter()
            .map(|line| Line::from(Span::styled(line, Style::default().fg(th.body))))
            .collect();
        f.render_widget(Paragraph::new(rows), preview_inner);
    }
}

fn slash_palette_height(hit_count: usize) -> u16 {
    if hit_count == 0 {
        0
    } else {
        // 상단 경계선 1행 + 개수 헤더 1행 + 실제 명령 목록.
        (hit_count.min(9) + 2) as u16
    }
}

fn responsive_rows(
    _width: u16,
    height: u16,
    wanted_todo: u16,
    wanted_palette: u16,
    wanted_input: u16,
    wanted_status: u16,
) -> [u16; 7] {
    let header = u16::from(height >= 5);
    let footer = u16::from(height >= 3);
    let fixed = header + footer;
    let remaining = height.saturating_sub(fixed);
    let input = wanted_input.max(1).min(remaining);
    let after_input = remaining.saturating_sub(input);
    // 상태 슬롯은 working 패널이 쓴다 — 입력창과 푸터 사이 (§16.2).
    let status = if height >= 8 {
        wanted_status.min(after_input.saturating_sub(3))
    } else {
        0
    };
    let after_status = after_input.saturating_sub(status);
    let todo = if height >= 8 {
        wanted_todo.min(after_status.saturating_sub(3))
    } else {
        0
    };
    let after_todo = after_status.saturating_sub(todo);
    let palette = if height >= 8 {
        wanted_palette.min(after_todo.saturating_sub(3))
    } else {
        0
    };
    let transcript = after_todo.saturating_sub(palette);
    [header, transcript, todo, input, palette, status, footer]
}

fn todo_panel_height(app: &App, width: u16) -> u16 {
    if app.todos.is_empty() {
        0
    } else {
        let content_width = width.saturating_sub(4).max(1) as usize;
        let todo_rows = app
            .todos
            .iter()
            .map(|item| {
                super::md::wrap_text(&item.content, content_width)
                    .len()
                    .max(1)
            })
            .sum();
        progress_panel_height(todo_rows, width)
    }
}

fn progress_panel_height(todo_count: usize, _width: u16) -> u16 {
    (todo_count + 2).min(u16::MAX as usize) as u16
}

/// working 패널에 한 번에 보일 워커 줄 수 상한 — 넘치면 오래된 워커를 생략한다.
const WORKING_MAX_ROWS: usize = 6;

/// working 패널이 요구하는 높이 — 본문 행 + 위쪽 구분선 1행. 본문이 없으면 0행.
fn working_panel_height(row_count: usize) -> u16 {
    if row_count == 0 {
        0
    } else {
        (row_count + 1).min(u16::MAX as usize) as u16
    }
}

/// working 패널 본문 — 에이전트별 한 줄 + 마지막 mode 줄. 순수 함수라 단위 테스트한다.
/// 워커도 mode 줄도 없으면 빈 목록(패널 0행)이다.
pub fn render_working_rows(
    workers: &[crate::ui::AgentProgress],
    mode_line: &str,
    width: u16,
    th: &Pal,
) -> Vec<Line<'static>> {
    let mode_rows = usize::from(!mode_line.trim().is_empty());
    if workers.is_empty() && mode_rows == 0 {
        return Vec::new();
    }
    // 상한을 넘으면 최근 워커를 남긴다 — 지금 일하는 쪽이 뒤에 있다.
    let cap = WORKING_MAX_ROWS.saturating_sub(mode_rows);
    let shown = &workers[workers.len().saturating_sub(cap)..];
    // 접두("working"/"mode") 2칸 들여쓰기 + 라벨 7칸 + 공백 2칸을 뺀 나머지가 본문 폭.
    let body_width = (width as usize).saturating_sub(11).max(8);
    let mut rows: Vec<Line<'static>> = Vec::new();
    for worker in shown {
        let mut body = String::new();
        for part in [
            worker.role.as_str(),
            worker.model.as_str(),
            worker.activity.as_str(),
        ] {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if !body.is_empty() {
                body.push_str("  ");
            }
            body.push_str(part);
        }
        rows.push(Line::from(vec![
            Span::styled(
                " working  ".to_string(),
                Style::default()
                    .fg(th.secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate_display(&body, body_width),
                Style::default().fg(th.body),
            ),
        ]));
    }
    if mode_rows == 1 {
        rows.push(Line::from(vec![
            Span::styled(" mode     ".to_string(), Style::default().fg(th.mute)),
            Span::styled(
                truncate_display(mode_line.trim(), body_width),
                Style::default().fg(th.mute),
            ),
        ]));
    }
    rows
}

fn draw_working_panel(f: &mut Frame, area: Rect, rows: Vec<Line<'static>>, th: &Pal) {
    if area.height == 0 || rows.is_empty() {
        return;
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(th.border))
        .style(Style::default().bg(th.bg));
    f.render_widget(Paragraph::new(rows).block(block), area);
}

fn draw_todo_panel(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    if area.height == 0 || app.todos.is_empty() {
        return;
    }
    let progress = crate::tools_more::todo_progress(&app.todos);
    let bar_width = area.width.saturating_sub(28).clamp(4, 20) as usize;
    let filled = progress
        .completed
        .saturating_mul(bar_width)
        .checked_div(progress.total)
        .unwrap_or(0);
    let mut rows = vec![Line::from(vec![
        Span::styled(
            format!(" Todo {}/{} ", progress.completed, progress.total),
            Style::default()
                .fg(th.secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("■".repeat(filled), Style::default().fg(th.accent)),
        Span::styled("·".repeat(bar_width - filled), Style::default().fg(th.mute)),
        Span::styled(
            format!(" {}%", progress.percent),
            Style::default().fg(th.mute),
        ),
    ])];
    for item in &app.todos {
        let (mark, style) = match item.status.as_str() {
            "completed" => (
                "o",
                Style::default()
                    .fg(th.mute)
                    .add_modifier(Modifier::CROSSED_OUT),
            ),
            "in_progress" => (
                "●",
                Style::default().fg(th.code).add_modifier(Modifier::BOLD),
            ),
            _ => ("○", Style::default().fg(th.body)),
        };
        rows.push(Line::from(vec![
            Span::styled(format!(" {mark} "), style),
            Span::styled(item.content.clone(), style),
        ]));
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(th.border))
        .style(Style::default().bg(th.bg));
    f.render_widget(
        Paragraph::new(rows).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_header(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    let version = env!("CARGO_PKG_VERSION");
    let mut spans = vec![
        Span::styled(
            " RAFIKX ",
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" v{version} "), Style::default().fg(th.mute)),
        Span::raw(" "),
        // Harness 정보 (모델명 제외 — 자동/수동 + 엔진)
        Span::styled(
            harness_header_label(
                app.session
                    .cfg
                    .file
                    .harness
                    .selection
                    .eq_ignore_ascii_case("manual"),
                app.session.cfg.file.general.engine.as_str(),
            ),
            Style::default().fg(th.code),
        ),
    ];
    // 시작 화면에선 스테이지 리본이 배너(좌측 패널)에 산다 — 헤더는 비워 시원하게.
    if !app.show_start {
        if area.width >= 112 {
            spans.push(Span::raw("   "));
            spans.extend(super::start::compact_signal(app, th));
        } else if area.width >= 72 {
            spans.push(Span::raw("   "));
            spans.extend(super::start::short_signal(app, th));
        }
    }
    if area.width >= 96 {
        append_workspace_path(
            &mut spans,
            &app.cwd,
            area.width.into(),
            Style::default().fg(th.secondary),
        );
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(th.bg)),
        area,
    );
}

fn append_workspace_path(
    spans: &mut Vec<Span<'static>>,
    path: &str,
    terminal_width: usize,
    style: Style,
) {
    const WORKSPACE_GAP: &str = "   ";
    let preceding_width = spans.iter().fold(0usize, |width, span| {
        width.saturating_add(super::md::display_width(&span.content))
    });
    let workspace_width = terminal_width
        .saturating_sub(preceding_width)
        .saturating_sub(super::md::display_width(WORKSPACE_GAP));
    let workspace = super::start::compact_workspace(path, workspace_width);
    if workspace.is_empty() {
        return;
    }
    spans.push(Span::raw(WORKSPACE_GAP));
    spans.push(Span::styled(workspace, style));
}

/// 대화 영역을 테두리로 감싼 뒤 내부에 트랜스크립트를 그린다.
fn draw_transcript_frame(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    f.render_widget(Clear, area);
    if area.width < 4 || area.height < 3 {
        draw_transcript(f, app, area, th);
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.mute))
        .style(Style::default().bg(th.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);
    draw_transcript(f, app, inner, th);
}

fn draw_transcript(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    if app.show_start {
        super::start::draw(f, app, area, th);
        return;
    }
    let width = area.width.max(1);
    let theme_name = app.session.cfg.file.ui.theme.clone();
    // 엔트리 단위 렌더 캐시 — 마크다운 파싱·syntect 하이라이트·줄바꿈까지
    // 끝난 비주얼 행을 (kind, text, width, theme) 해시로 재사용한다.
    // 확정된 과거 턴은 다시 파싱하지 않으므로 프레임 비용이 O(보이는 행)이다.
    let mut cache = app.render_cache.borrow_mut();
    let mut visual: Vec<Line> = Vec::new();
    let mut count = 0usize;
    let last = app.transcript.len();
    for (idx, e) in app.transcript.iter().enumerate() {
        // 스트리밍 중인 마지막 답변은 경량 렌더 — 청크가 올 때마다 전체
        // 마크다운 파싱과 syntect 하이라이팅을 다시 돌리면 긴 코딩 답변에서
        // CPU 가 폭증한다. 턴이 끝나면 풀 렌더로 확정되어 캐시에 들어간다.
        if app.busy && idx + 1 == last && e.kind == EntryKind::Assistant {
            visual.extend(render_streaming_tail(&e.text, width, th));
            continue;
        }
        // 턴 진행 중의 Assistant 엔트리는 전부 모델 작업 출력 — 흐린 기울임으로 그린다.
        // 턴이 끝나면 collapse_turn_noise 가 작업 엔트리를 걷어내고 최종 답변만 남기므로
        // 남은 답변은 work=false, 보통 스타일로 그려진다.
        let work = app.busy && e.kind == EntryKind::Assistant;
        let h = entry_render_hash(e, width, &theme_name, work);
        if count < cache.len() && cache[count].0 == h {
            visual.extend(cache[count].1.iter().cloned());
        } else {
            let rows = render_entry(e, width, th, work);
            let slot = (h, rows.clone());
            if count < cache.len() {
                cache[count] = slot;
            } else {
                cache.push(slot);
            }
            visual.extend(rows);
        }
        count += 1;
    }
    cache.truncate(count);
    drop(cache);

    if let Some(approval) = &app.approval {
        visual.push(Line::default());
        visual.push(Line::from(Span::styled(
            " APPROVAL  도구 실행 승인 필요",
            Style::default().fg(th.warn).add_modifier(Modifier::BOLD),
        )));
        let content_width = width.saturating_sub(3).max(1) as usize;
        for row in approval.preview.lines() {
            // diff 표기(+/-/@@)를 줄 단위로 색칠해 변경 전후가 한눈에 보이게.
            let row_style = preview_line_style(row, th);
            for wrapped in super::md::wrap_text(row, content_width) {
                visual.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(th.warn)),
                    Span::styled(wrapped, row_style),
                ]));
            }
        }
    }

    if visual.is_empty() {
        visual.push(Line::from(Span::styled(
            "  할 일을 말하면 실행합니다.  ?  키 도움말",
            Style::default().fg(th.mute),
        )));
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
    let visible: Vec<Line> = visual[start..end]
        .iter()
        .cloned()
        .map(|line| pad_line_to_width(line, width as usize, th.bg))
        .collect();

    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().bg(th.bg));
    f.render_widget(Paragraph::new(visible).block(block), area);
}

/// 스트리밍 중 답변의 경량 렌더 — 마크다운 파싱·코드 하이라이팅 없이
/// 태그와 줄바꿈만. 완료되면 render_entry 의 풀 렌더로 교체된다.
/// 작업 중 스트리밍 텍스트 스타일 — 흐린 회색 + 기울임 (모델 작업임을 시각히 구분).
/// 본문만 흐리게 하고 태그는 그대로 둬 어느 모델 출력인지는 계속 식별된다.
fn work_body_style(th: &Pal) -> Style {
    Style::default()
        .fg(th.thinking)
        .add_modifier(Modifier::ITALIC)
}

fn render_streaming_tail(text: &str, width: u16, th: &Pal) -> Vec<Line<'static>> {
    let shown = compact_blank(&display_model_work(text));
    let content_width = width.saturating_sub(8).max(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut first = true;
    let body = work_body_style(th);
    for row in shown.lines() {
        let pieces = super::md::wrap_text(row, content_width);
        let pieces = if pieces.is_empty() {
            vec![String::new()]
        } else {
            pieces
        };
        for piece in pieces {
            if first {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {:<6}", "rafikx"),
                        Style::default().fg(th.code).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {piece}"), body),
                ]));
                first = false;
            } else {
                lines.push(Line::from(vec![
                    Span::styled(
                        CONTINUATION_PREFIX.to_string(),
                        Style::default().fg(th.mute),
                    ),
                    Span::styled(format!(" {piece}"), body),
                ]));
            }
        }
    }
    lines
}

/// 렌더 캐시 키 — 내용·폭·테마·작업 여부가 같으면 렌더 결과도 같다.
/// 작업 여부가 키에 들어가는 이유: 같은 Assistant 텍스트라 턴 진행 중(흐린 기울임)과
/// 완료 답변(보통 스타일)은 다르게 그려진다.
fn entry_render_hash(e: &super::Entry, width: u16, theme: &str, work: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (e.kind as u8).hash(&mut h);
    e.text.hash(&mut h);
    width.hash(&mut h);
    theme.hash(&mut h);
    work.hash(&mut h);
    h.finish()
}

/// 승인 프리뷰 한 줄의 스타일 — unified diff 표기를 색으로 구분한다.
/// (+ 추가=녹색, - 삭제=빨강, @@ 헝크=보조색, `--- diff/patch ---` 헤더=흐림)
fn preview_line_style(row: &str, th: &Pal) -> Style {
    if row == "--- diff ---" || row == "--- patch ---" {
        return Style::default().fg(th.mute);
    }
    if row.starts_with("@@") {
        return Style::default().fg(th.secondary);
    }
    if row.starts_with('+') {
        return Style::default().fg(th.success);
    }
    if row.starts_with('-') {
        return Style::default().fg(th.err);
    }
    Style::default().fg(th.text)
}

/// 엔트리 하나를 래핑 완료된 비주얼 행으로 렌더한다 (순수 함수 — 캐시 대상).
/// `work` 는 턴 진행 중인 Assistant 엔트리 — 모델 작업 출력이므로 흐린 기울임으로 그린다.
fn render_entry(e: &super::Entry, width: u16, th: &Pal, work: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    let text = match e.kind {
        EntryKind::Assistant => compact_blank(&display_model_work(&e.text)),
        EntryKind::User | EntryKind::Queued => super::collapsed_input(&e.text),
        _ => e.text.clone(),
    };
    if text.trim().is_empty() && e.kind == EntryKind::Assistant {
        return Vec::new();
    }
    let role = coding_role(&text);
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
        EntryKind::Tool => match role {
            CodeRole::Create => ("create", Style::default().fg(Color::Green)),
            CodeRole::Edit => ("edit", Style::default().fg(th.warn)),
            CodeRole::Delete => ("delete", Style::default().fg(th.err)),
            CodeRole::Verify => ("verify", Style::default().fg(th.secondary)),
            CodeRole::Other => ("tool", Style::default().fg(th.secondary)),
        },
        EntryKind::Warn => (
            "!",
            Style::default().fg(th.err).add_modifier(Modifier::BOLD),
        ),
    };
    // 컴팩트 레이아웃: 태그를 첫 줄에 인라인으로 붙이고 이후 줄은 세로선만.
    let tag_pad = format!("{tag:<6}");
    let mut first = true;
    let push_row = |lines: &mut Vec<Line>, text: &str, st: Style, first: &mut bool| {
        if *first {
            lines.push(Line::from(vec![
                Span::styled(format!(" {tag_pad}"), style),
                Span::styled(format!(" {text}"), st),
            ]));
            *first = false;
        } else {
            lines.push(Line::from(vec![
                Span::styled(CONTINUATION_PREFIX, Style::default().fg(th.mute)),
                Span::styled(format!(" {text}"), st),
            ]));
        }
    };

    if e.kind == EntryKind::Assistant {
        let content_width = width.saturating_sub(8).max(1) as usize;
        let work_heading = Style::default()
            .fg(th.thinking)
            .add_modifier(Modifier::ITALIC)
            .add_modifier(Modifier::BOLD);
        for (is_work, section) in split_model_work(&text) {
            if is_work {
                let work_style = work_body_style(th);
                push_row(&mut lines, "모델 작업", work_style, &mut first);
                for row in super::md::wrap_text(&section, content_width) {
                    push_row(&mut lines, &row, work_style, &mut first);
                }
                continue;
            }
            let segs = crate::tui::md::markdown_segs_with_width(&section, Some(content_width));
            for seg in &segs {
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
                // 작업 진행 중이면 본문은 흐린 기울임 한 가지로 통일하고,
                // 제목(헤더)만 굵게 남겨 한국어 제목이 눈에 걸리게 한다.
                let st = if work {
                    match seg.kind {
                        MdKind::Heading | MdKind::Emphasis => work_heading,
                        _ => work_body_style(th),
                    }
                } else {
                    match seg.kind {
                        MdKind::Heading => {
                            Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
                        }
                        MdKind::Emphasis => {
                            Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
                        }
                        MdKind::Code | MdKind::CodeBlock | MdKind::Table | MdKind::Chart => {
                            Style::default().fg(th.code).bg(th.panel)
                        }
                        MdKind::Command => Style::default()
                            .fg(th.secondary)
                            .add_modifier(Modifier::BOLD),
                        MdKind::Text => Style::default().fg(th.text),
                    }
                };
                for piece in seg.text.split('\n') {
                    push_row(&mut lines, piece, st, &mut first);
                }
            }
        }
    } else if e.kind == EntryKind::System {
        if text.starts_with("Run summary") {
            for (index, row) in text.split('\n').enumerate() {
                let style = if index == 0 {
                    Style::default().fg(th.code).add_modifier(Modifier::BOLD)
                } else if row.trim_start().starts_with('✓') {
                    Style::default().fg(th.success).add_modifier(Modifier::BOLD)
                } else if row.trim_start().starts_with('!') {
                    Style::default().fg(th.err).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(th.body)
                };
                lines.push(Line::from(Span::styled(format!("   {row}"), style)));
            }
        } else {
            // 시스템 안내는 태그 없이 들여쓰기만 (sys 접두어가 거슬리므로)
            for row in text.split('\n') {
                lines.push(Line::from(Span::styled(
                    format!("   · {row}"),
                    Style::default().fg(th.mute),
                )));
            }
        }
    } else {
        let failed_tool = e.kind == EntryKind::Tool && text.contains("도구 오류");
        if matches!(e.kind, EntryKind::Warn) || failed_tool {
            // 에러는 붉은 계열 본문 + 중요 단어만 다크엘로우 굵게.
            for row in text.split('\n') {
                let mut spans: Vec<Span> = Vec::new();
                if first {
                    spans.push(Span::styled(format!(" {tag_pad}"), style));
                    first = false;
                } else {
                    spans.push(Span::styled(
                        CONTINUATION_PREFIX,
                        Style::default().fg(th.mute),
                    ));
                }
                append_alert_spans(&mut spans, row, th);
                lines.push(Line::from(spans));
            }
        } else {
            let st = match role {
                CodeRole::Create => Style::default().fg(Color::Green),
                CodeRole::Edit => Style::default().fg(th.warn),
                CodeRole::Delete => Style::default().fg(th.err),
                CodeRole::Verify => Style::default().fg(th.secondary),
                CodeRole::Other => Style::default().fg(th.body),
            };
            for row in text.split('\n') {
                push_row(&mut lines, row, st, &mut first);
            }
        }
    }
    // 빈 줄은 도구·경고 뒤에만 — 대화 본문은 태그 색으로 구분해 밀도를 높인다.
    if matches!(e.kind, EntryKind::Tool | EntryKind::Warn) {
        lines.push(Line::from(""));
    }

    // 논리 줄을 터미널 폭 기준 비주얼 행으로 잘라 반환한다.
    let w = width.max(1) as usize;
    let mut visual: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    for l in &lines {
        if wrapped_rows(l, width) <= 1 {
            visual.push(l.clone());
        } else {
            for row in wrap_spans(&l.spans, w) {
                visual.push(Line::from(row));
            }
        }
    }
    visual
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeRole {
    Create,
    Edit,
    Delete,
    Verify,
    Other,
}

fn coding_role(text: &str) -> CodeRole {
    if text.contains("[코드 변경] 등록") {
        CodeRole::Create
    } else if text.contains("[코드 변경] 수정") {
        CodeRole::Edit
    } else if text.contains("[코드 변경] 삭제") {
        CodeRole::Delete
    } else if text.contains("[검증]") || text.contains("test result:") {
        CodeRole::Verify
    } else {
        CodeRole::Other
    }
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

fn pad_line_to_width(mut line: Line<'static>, width: usize, background: Color) -> Line<'static> {
    let used: usize = line
        .spans
        .iter()
        .map(|span| super::md::display_width(&span.content))
        .sum();
    if used < width {
        line.spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(background),
        ));
    }
    line
}

fn input_height(app: &App, width: u16) -> u16 {
    let w = width.saturating_sub(2).max(8) as usize;
    let shown = super::collapsed_input(&app.input);
    let rows = super::md::wrap_text(&shown, w).len().max(1);
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

/// 모델 답변의 단락 구분은 한 줄만 보존하고 연속된 빈 줄만 접는다.
fn compact_blank(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_fence = false;
    let mut pending_blank = false;
    for line in s.split('\n') {
        if line.trim_start().starts_with("```") {
            if pending_blank && !out.is_empty() {
                out.push('\n');
                pending_blank = false;
            }
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
            if !out.is_empty() {
                pending_blank = true;
            }
            continue;
        }
        if pending_blank {
            out.push('\n');
            pending_blank = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// 오류·경고 본문의 중요 단어 사전 — 붉은 본문 위에 다크엘로우 굵게 칠한다.
const ALERT_KEYWORDS: &[&str] = &[
    "오류", "실패", "거부", "중단", "경고", "위험", "error", "Error", "ERROR", "failed", "Failed",
    "FAILED", "fail", "warning", "Warning", "denied", "refused", "timeout",
];

fn append_alert_spans(spans: &mut Vec<Span<'static>>, text: &str, th: &Pal) {
    let err_style = Style::default().fg(th.err);
    let kw_style = Style::default().fg(th.kw).add_modifier(Modifier::BOLD);
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
                    spans.push(Span::styled(text[rest_start..p].to_string(), err_style));
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
                format!(" {tag_pad}"),
                Style::default().fg(th.code).add_modifier(Modifier::BOLD),
            ));
            *first = false;
        } else {
            all.push(Span::styled(
                CONTINUATION_PREFIX,
                Style::default().fg(th.mute),
            ));
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
        out.push(vec![Span::styled(
            " ".to_string(),
            Style::default().bg(th.panel),
        )]);
    }
    out
}

/// 공급자가 공개 응답으로 보낸 작업 블록을 일반 답변과 구분해 그대로 표시한다.
pub fn display_model_work(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some((start, open)) = THINK_OPEN
            .iter()
            .filter_map(|open| rest.find(open).map(|i| (i, *open)))
            .min_by_key(|(i, _)| *i)
        else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..start]);
        let after = &rest[start + open.len()..];
        out.push_str("\n[모델 작업]\n");
        if let Some((end, close)) = THINK_CLOSE
            .iter()
            .filter_map(|close| after.find(close).map(|i| (i, *close)))
            .min_by_key(|(i, _)| *i)
        {
            out.push_str(&after[..end]);
            out.push_str("\n[/모델 작업]\n");
            rest = &after[end + close.len()..];
        } else {
            out.push_str(after);
            return out;
        }
    }
}

fn split_model_work(text: &str) -> Vec<(bool, String)> {
    const OPEN: &str = "[모델 작업]";
    const CLOSE: &str = "[/모델 작업]";
    let mut sections = Vec::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find(OPEN) else {
            let plain = rest.trim_matches('\n');
            if !plain.is_empty() {
                sections.push((false, plain.to_string()));
            }
            break;
        };
        let plain = rest[..start].trim_matches('\n');
        if !plain.is_empty() {
            sections.push((false, plain.to_string()));
        }
        let work = &rest[start + OPEN.len()..];
        if let Some(end) = work.find(CLOSE) {
            sections.push((true, work[..end].trim_matches('\n').to_string()));
            rest = &work[end + CLOSE.len()..];
        } else {
            sections.push((true, work.trim_matches('\n').to_string()));
            break;
        }
    }
    if sections.is_empty() {
        sections.push((false, String::new()));
    }
    sections
}

fn draw_input(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    // opencode 스타일 — 테두리 없는 "> " 프롬프트. busy 중엔 "…" 로 상태를 표시한다.
    let (mark, mark_style) = if app.busy {
        ("…", Style::default().fg(th.mute))
    } else {
        (">", Style::default().fg(th.accent))
    };
    let body_style = Style::default().fg(th.accent).add_modifier(if app.busy {
        Modifier::empty()
    } else {
        Modifier::BOLD
    });
    let mut lines: Vec<Line> = Vec::new();
    let shown = super::collapsed_input(&app.input);
    let mut rows = shown.split('\n');
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
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(th.bg)),
        area,
    );

    if shown == app.input
        && !app.busy
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

/// 단일 푸터 (pi 스타일) — 모드 배지 · 상태(스피너/승인/완료) · 모델 · 통계를
/// 한 줄에 담는다. 옛 상태 스트립의 역할(스피너·승인 버튼)을 흡수했다.
fn draw_footer(f: &mut Frame, app: &App, area: Rect, th: &Pal) {
    let badge_text = if app.session.is_plan_mode() {
        " PLAN "
    } else {
        " BUILD "
    };
    let badge_style = Style::default()
        .fg(th.bg)
        .bg(if app.session.is_plan_mode() {
            th.secondary
        } else {
            th.code
        })
        .add_modifier(Modifier::BOLD);
    let model = selected_model_label(app);
    let context = if app.ctx.is_empty() {
        selected_context_label(app)
    } else {
        app.ctx.clone()
    };

    let width = area.width as usize;
    let mut line_spans = vec![Span::styled(badge_text, badge_style)];
    let mut used = super::md::display_width(badge_text);

    // 권한무시(YOLO) 상시 표시 — 자동 승인 중임을 항상 인지하게 한다.
    if app.session.yes {
        let yolo = " YOLO ";
        line_spans.push(Span::styled(
            yolo,
            Style::default()
                .fg(th.bg)
                .bg(th.err)
                .add_modifier(Modifier::BOLD),
        ));
        used += super::md::display_width(yolo);
    }

    // 상태 — 승인 대기가 최우선, 그다음 실행 스피너, 마지막 idle 결과.
    if let Some(ap) = &app.approval {
        for (i, (label, color)) in [("Yes", Color::Green), ("No", th.err), ("Always", th.warn)]
            .iter()
            .enumerate()
        {
            let picked = i == ap.selected;
            let text = if picked {
                format!(" ▸[{label}]")
            } else {
                format!("  {label} ")
            };
            let style = if picked {
                Style::default()
                    .fg(*color)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(th.mute)
            };
            used += super::md::display_width(&text);
            line_spans.push(Span::styled(text, style));
        }
        let hint = " ←→ 이동 · Enter 확정 · y/n/a";
        used += super::md::display_width(hint);
        line_spans.push(Span::styled(hint, Style::default().fg(th.mute)));
    } else if app.busy {
        let state = format!(" {} {}", braille_spin(), active_status_label(&app.status));
        used += super::md::display_width(&state);
        line_spans.push(Span::styled(
            state,
            Style::default().fg(th.code).add_modifier(Modifier::BOLD),
        ));
    } else {
        let state = format!(
            " {} {}",
            idle_status_mark(&app.status),
            footer_state_label(&app.status)
        );
        used += super::md::display_width(&state);
        line_spans.push(Span::styled(
            state,
            Style::default().fg(idle_status_color(&app.status, th)),
        ));
    }

    let mut secondary = vec![(
        model,
        Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
    )];
    if !app.tokens.is_empty() {
        secondary.push((app.tokens.clone(), Style::default().fg(th.mute)));
    }
    if !app.todos.is_empty() {
        let progress = crate::tools_more::todo_progress(&app.todos);
        secondary.push((
            format!("Todo {}/{}", progress.completed, progress.total),
            Style::default().fg(th.secondary),
        ));
    }
    if !app.queue.is_empty() {
        secondary.push((
            format!("Queued {}", app.queue.len()),
            Style::default().fg(th.secondary),
        ));
    }

    let right_budget = width.saturating_sub(used + 1);
    let shown_context = truncate_display(&context, right_budget);
    let right_width = super::md::display_width(&shown_context);
    used += right_width;
    for (text, style) in secondary {
        let needed = 3 + super::md::display_width(&text);
        if used + needed + 1 > width {
            continue;
        }
        line_spans.push(Span::styled(" · ", Style::default().fg(th.border)));
        line_spans.push(Span::styled(text, style));
        used += needed;
    }
    if right_width > 0 {
        let padding = width.saturating_sub(used).max(1);
        line_spans.push(Span::raw(" ".repeat(padding)));
        line_spans.push(Span::styled(
            shown_context,
            context_usage_style(&context, th),
        ));
    }

    f.render_widget(
        Paragraph::new(Line::from(line_spans)).style(Style::default().bg(th.bg)),
        area,
    );
}

fn truncate_display(text: &str, width: usize) -> String {
    if super::md::display_width(text) <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let target = width.saturating_sub(1);
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = super::md::ch_width(ch);
        if used + ch_width > target {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}

pub(super) fn selected_model_label(app: &App) -> String {
    app.session
        .model
        .clone()
        .or_else(|| {
            app.session
                .cfg
                .provider(&app.session.cfg.file.general.default_provider)
                .ok()
                .map(|provider| provider.model.clone())
        })
        .unwrap_or_else(|| "auto".into())
}

fn harness_header_label(manual: bool, engine: &str) -> String {
    format!(
        "Harness {} · {engine}",
        if manual { "Manual" } else { "Auto" }
    )
}

fn footer_state_label(status: &str) -> &'static str {
    let normalized = status.trim().to_ascii_lowercase();
    if normalized.contains("fail")
        || normalized.contains("error")
        || status.contains("실패")
        || status.contains("오류")
    {
        "Failed"
    } else if normalized.contains("stop")
        || normalized.contains("interrupt")
        || status.contains("중단")
    {
        "Stopped"
    } else {
        "Ready"
    }
}

fn idle_status_mark(status: &str) -> &'static str {
    match footer_state_label(status) {
        "Failed" => "!",
        "Stopped" => "■",
        "Ready" => "✓",
        _ => "?",
    }
}

fn idle_status_color(status: &str, th: &Pal) -> Color {
    match footer_state_label(status) {
        "Failed" => th.err,
        "Stopped" => th.warn,
        "Ready" => th.success,
        _ => th.mute,
    }
}

fn active_status_label(status: &str) -> &'static str {
    if status.starts_with("Compacting") {
        "Auto-compacting"
    } else {
        "Working"
    }
}

fn selected_context_label(app: &App) -> String {
    let cfg = &app.session.cfg;
    let (provider, model) = match (&app.session.provider, &app.session.model) {
        (Some(provider), Some(model)) => (provider.as_str(), model.as_str()),
        _ => {
            let provider = cfg.file.general.default_provider.as_str();
            let model = cfg
                .provider(provider)
                .map(|p| p.model.as_str())
                .unwrap_or("");
            (provider, model)
        }
    };
    let window = crate::packer::context_window_for(provider, model, cfg.provider(provider).ok());
    if window == 0 {
        return String::new();
    }
    format!(
        "ctx 0/{} (auto) · mem {}",
        format_tokens(window),
        if app.session.obsidian_on { "on" } else { "off" }
    )
}

fn context_usage_style(context: &str, th: &Pal) -> Style {
    let percent = context
        .split('(')
        .nth(1)
        .and_then(|part| part.split_once("%)"))
        .map(|(raw, _)| raw)
        .and_then(|raw| raw.parse::<u32>().ok())
        .unwrap_or(0);
    let color = if percent > 90 {
        th.err
    } else if percent > 70 {
        th.warn
    } else {
        th.mute
    };
    Style::default().fg(color)
}

fn format_tokens(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// 부팅 후 경과 밀리초 기반 프레임 인덱스 — ratatui 리드로우 주기와 무관하게 균등 회전.
fn spin_frame_index(interval_ms: u64, frame_count: usize) -> usize {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    (ms / interval_ms) as usize % frame_count
}

fn spin_frame<'a>(frames: &[&'a str], interval_ms: u64) -> &'a str {
    frames[spin_frame_index(interval_ms, frames.len())]
}

/// opencode 동일 braille 10-frame (80ms).
fn braille_spin() -> &'static str {
    spin_frame(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"], 80)
}

/// 입력이 "/명령 접두어" 형태일 때 일치하는 명령 목록 (공백이 들어가면 숨긴다).
fn slash_matches(app: &App) -> Vec<&'static (&'static str, &'static str)> {
    slash_hits_for(&app.input)
}

fn slash_hits_for(input: &str) -> Vec<&'static (&'static str, &'static str)> {
    let t = input.trim_start();
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
    // 검색 불일치면 팔레트를 숨긴다 — 오타 한 글자에 전체 목록으로 화면
    // 하단을 채우지 않는다 (pi 스타일 저소음).
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
    for (i, &orig) in idxs.iter().enumerate().take(end).skip(start) {
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
    draw_overlay_sized(
        f,
        area,
        &picker.title,
        &body,
        80,
        shown.saturating_add(5),
        th,
    );
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
    let rect = overlay_rect(area, 76, 14);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
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
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
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
    let rect = overlay_rect(area, max_w, max_h);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
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

pub(super) fn overlay_rect(area: Rect, max_w: u16, max_h: u16) -> Rect {
    let tight = area.width <= max_w.saturating_add(4) || area.height <= max_h.saturating_add(2);
    if tight {
        return area;
    }
    let width = max_w.min(area.width).max(1);
    let height = max_h.min(area.height).max(1);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_work_keeps_complete_thinking_visible() {
        assert_eq!(
            display_model_work("before<think>검토 중</think>after"),
            "before\n[모델 작업]\n검토 중\n[/모델 작업]\nafter"
        );
    }

    #[test]
    fn model_work_keeps_open_thinking_visible_while_streaming() {
        assert_eq!(
            display_model_work("before<think>아직 검토 중"),
            "before\n[모델 작업]\n아직 검토 중"
        );
    }

    #[test]
    fn model_work_keeps_plain_text() {
        let s = "일반 답변입니다";
        assert_eq!(display_model_work(s), s);
    }

    #[test]
    fn final_answer_preserves_single_paragraph_spacing() {
        assert_eq!(
            compact_blank("첫 문단\n\n둘째 문단"),
            "첫 문단\n\n둘째 문단"
        );
        assert_eq!(
            compact_blank("첫 문단\n\n\n\n둘째 문단"),
            "첫 문단\n\n둘째 문단"
        );
    }

    #[test]
    fn model_work_sections_are_separated_from_final_answer() {
        let sections = split_model_work("[모델 작업]\n파일을 분석 중\n[/모델 작업]\n최종 답변");
        assert_eq!(sections.len(), 2);
        assert!(sections[0].0);
        assert_eq!(sections[0].1, "파일을 분석 중");
        assert!(!sections[1].0);
        assert_eq!(sections[1].1, "최종 답변");
    }

    #[test]
    fn wrapped_rows_accounts_for_korean_width() {
        let line = Line::from("한글은 두 칸씩".to_string());
        assert_eq!(wrapped_rows(&line, 100), 1);
        assert!(wrapped_rows(&line, 4) >= 3);
    }

    #[test]
    fn header_workspace_uses_unicode_width_and_keeps_the_path_tail() {
        let mut spans = vec![Span::raw(" RAFIKX "), Span::raw(" 준비 ")];
        let path = "/Users/노아/프로젝트/RafikX-작업";
        let terminal_width = 30;

        append_workspace_path(&mut spans, path, terminal_width, Style::default());

        let shown = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let shown_width = spans
            .iter()
            .map(|span| super::super::md::display_width(&span.content))
            .sum::<usize>();
        assert!(shown.ends_with("…/RafikX-작업"));
        assert_eq!(shown_width, terminal_width);
    }

    #[test]
    fn work_phase_assistant_renders_dim_italic_and_final_normal() {
        let th = pal_of(&palette::RAFIKX);
        let e = crate::tui::Entry {
            kind: EntryKind::Assistant,
            text: "Reading the fallback logic\n\n## 다음 단계\napply the fix".into(),
        };
        let has_italic = |rows: &[Line<'static>]| {
            rows.iter().any(|l| {
                l.spans
                    .iter()
                    .any(|s| s.style.add_modifier.contains(Modifier::ITALIC))
            })
        };
        // 턴 진행 중(work=true) — 모델 작업 출력은 흐린 기울임.
        assert!(has_italic(&render_entry(&e, 80, &th, true)));
        // 완료 답변(work=false) — 보통 스타일.
        assert!(!has_italic(&render_entry(&e, 80, &th, false)));
    }

    #[test]
    fn work_heading_stays_bold_while_body_is_dim() {
        let th = pal_of(&palette::RAFIKX);
        let e = crate::tui::Entry {
            kind: EntryKind::Assistant,
            text: "## 진행 상황\nEnglish narration body".into(),
        };
        let rows = render_entry(&e, 80, &th, true);
        // 제목 줄은 굵게+기울임, 본문 줄은 기울임만.
        let heading_bold = rows.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.content.contains("진행 상황")
                    && s.style.add_modifier.contains(Modifier::BOLD)
                    && s.style.add_modifier.contains(Modifier::ITALIC)
            })
        });
        assert!(heading_bold);
    }

    #[test]
    fn streaming_tail_renders_dim_italic() {
        let th = pal_of(&palette::RAFIKX);
        let rows = render_streaming_tail("Working on the fallback", 80, &th);
        assert!(rows.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::ITALIC))
        }));
    }

    #[test]
    fn narrow_layout_never_allocates_more_rows_than_available() {
        for height in 1..16 {
            let rows = responsive_rows(18, height, 4, 9, 3, 3);
            assert!(rows.iter().copied().sum::<u16>() <= height);
            if height < 8 {
                assert_eq!(rows[4], 0, "좁은 높이에서는 명령 팔레트를 접어야 한다");
                assert_eq!(rows[2], 0, "좁은 높이에서는 Todo 패널을 접어야 한다");
                assert_eq!(rows[5], 0, "좁은 높이에서는 working 패널을 접어야 한다");
            }
        }
    }

    #[test]
    fn working_panel_takes_the_status_slot_without_stealing_the_transcript() {
        // 넉넉한 높이에서는 요청한 만큼 status 슬롯(chunks[5])을 받는다.
        let rows = responsive_rows(80, 30, 4, 0, 3, 3);
        assert_eq!(rows[5], 3);
        assert!(rows.iter().copied().sum::<u16>() <= 30);
        // 요청이 없으면 0행 — 이전과 같은 레이아웃.
        let none = responsive_rows(80, 30, 4, 0, 3, 0);
        assert_eq!(none[5], 0);
        assert_eq!(none[1], rows[1] + 3, "패널이 접히면 그 행은 트랜스크립트로");
    }

    #[test]
    fn working_panel_height_adds_one_divider_row_only_when_used() {
        assert_eq!(working_panel_height(0), 0);
        assert_eq!(working_panel_height(1), 2);
        assert_eq!(working_panel_height(6), 7);
    }

    fn worker(id: &str, role: &str, model: &str, activity: &str) -> crate::ui::AgentProgress {
        crate::ui::AgentProgress {
            id: id.into(),
            role: role.into(),
            model: model.into(),
            activity: activity.into(),
            done: false,
        }
    }

    fn row_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn working_rows_show_role_model_and_activity_after_the_working_prefix() {
        let th = pal_of(&palette::RAFIKX);
        let rows = render_working_rows(
            &[worker("run-1", "dev", "minimax/MiniMax-M3", "반복 3/25")],
            "engine=minimax(고정) · team=single",
            120,
            &th,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(
            row_text(&rows[0]),
            " working  dev  minimax/MiniMax-M3  반복 3/25"
        );
        assert!(row_text(&rows[1]).starts_with(" mode     engine=minimax(고정)"));
        // 접두는 영문 소문자 그대로여야 한다 (§16.2).
        assert!(rows[0].spans[0].content.contains("working"));
    }

    #[test]
    fn working_rows_are_empty_without_workers_or_mode() {
        let th = pal_of(&palette::RAFIKX);
        assert!(render_working_rows(&[], "", 120, &th).is_empty());
        assert!(render_working_rows(&[], "   ", 120, &th).is_empty());
        // mode 줄만 있어도 패널은 열린다.
        assert_eq!(render_working_rows(&[], "engine=rafikx", 120, &th).len(), 1);
    }

    #[test]
    fn working_rows_cap_at_six_and_drop_the_oldest_workers() {
        let th = pal_of(&palette::RAFIKX);
        let many: Vec<_> = (0..8)
            .map(|i| worker(&format!("w{i}"), &format!("role{i}"), "p/m", "일하는 중"))
            .collect();
        let rows = render_working_rows(&many, "engine=rafikx", 120, &th);
        assert_eq!(rows.len(), WORKING_MAX_ROWS);
        // 최근 5명 + mode 줄 — 가장 오래된 w0..w2 는 생략된다.
        assert!(row_text(&rows[0]).contains("role3"));
        assert!(row_text(&rows[4]).contains("role7"));
        assert!(rows.iter().all(|r| !row_text(r).contains("role0")));

        // mode 줄이 없으면 워커가 상한을 다 쓴다.
        let rows = render_working_rows(&many, "", 120, &th);
        assert_eq!(rows.len(), WORKING_MAX_ROWS);
        assert!(row_text(&rows[0]).contains("role2"));
        assert!(row_text(&rows[5]).contains("role7"));
    }

    #[test]
    fn working_rows_stay_inside_the_terminal_width_with_korean_text() {
        let th = pal_of(&palette::RAFIKX);
        let rows = render_working_rows(
            &[worker(
                "run-1",
                "backend",
                "minimax/MiniMax-M3",
                "도구 호출 작성 중: write_file · 48KB",
            )],
            "engine=minimax(고정) · team=multi · discipline=graph · self v3 · gate on",
            40,
            &th,
        );
        for row in &rows {
            assert!(
                super::super::md::display_width(&row_text(row)) <= 40,
                "행이 터미널 폭을 넘었다: {}",
                row_text(row)
            );
        }
    }

    #[test]
    fn working_rows_skip_empty_fields_instead_of_printing_separators() {
        let th = pal_of(&palette::RAFIKX);
        let rows = render_working_rows(&[worker("run-1", "", "", "시작")], "", 120, &th);
        assert_eq!(row_text(&rows[0]), " working  시작");
    }

    #[test]
    fn narrow_overlay_uses_one_full_screen_box() {
        let area = Rect::new(0, 0, 45, 15);
        assert_eq!(overlay_rect(area, 72, 22), area);
        let wide = Rect::new(0, 0, 120, 40);
        assert_eq!(overlay_rect(wide, 72, 22), Rect::new(24, 9, 72, 22));
    }

    #[test]
    fn transcript_prefix_reclaims_pipe_column() {
        assert_eq!(crate::tui::md::display_width(CONTINUATION_PREFIX), 7);
        assert_eq!(CONTINUATION_PREFIX, "       ");
        assert_eq!(crate::tui::md::display_width(" rafikx"), 7);
    }

    #[test]
    fn exact_slash_command_stays_visible_with_arguments() {
        let hits = slash_hits_for("/model gpt-5");
        assert_eq!(hits.first().map(|(name, _)| *name), Some("/model"));
        assert_eq!(slash_palette_height(hits.len()), 3);
    }

    #[test]
    fn todo_panel_requests_every_item_row() {
        assert_eq!(progress_panel_height(8, 100), 10);
    }

    #[test]
    fn code_changes_have_distinct_visual_roles() {
        assert_eq!(
            coding_role("[코드 변경] 등록 src/a.rs:1-2"),
            CodeRole::Create
        );
        assert_eq!(coding_role("[코드 변경] 수정 src/a.rs:1-2"), CodeRole::Edit);
        assert_eq!(
            coding_role("[코드 변경] 삭제 src/a.rs:1-2"),
            CodeRole::Delete
        );
        assert_eq!(coding_role("[검증] cargo test"), CodeRole::Verify);
    }

    #[test]
    fn top_and_bottom_bar_labels_are_english() {
        assert_eq!(
            harness_header_label(false, "rafikx"),
            "Harness Auto · rafikx"
        );
        assert_eq!(harness_header_label(true, "pi"), "Harness Manual · pi");
        assert_eq!(footer_state_label("준비"), "Ready");
        assert_eq!(footer_state_label("실패"), "Failed");
        assert_eq!(footer_state_label("중단됨"), "Stopped");
        assert_eq!(idle_status_mark("준비"), "✓");
        assert_eq!(idle_status_mark("실패"), "!");
        assert_eq!(idle_status_mark("중단됨"), "■");
        assert_eq!(
            active_status_label("Compacting context at 80%"),
            "Auto-compacting"
        );
        assert_eq!(active_status_label("도구 실행 중"), "Working");
    }

    #[test]
    fn footer_truncation_respects_terminal_columns() {
        let shown = truncate_display("ctx 800k/1.0M (80%) · compact auto", 18);
        assert!(super::super::md::display_width(&shown) <= 18);
    }

    #[test]
    fn transcript_rows_overwrite_every_terminal_column() {
        let padded = pad_line_to_width(Line::from("짧은 답변"), 20, Color::Black);
        let width: usize = padded
            .spans
            .iter()
            .map(|span| super::super::md::display_width(&span.content))
            .sum();
        assert_eq!(width, 20);
    }
}

#[cfg(test)]
mod preview_style_tests {
    use super::*;

    #[test]
    fn diff_lines_are_colored_by_marker() {
        let th = pal_of(&palette::RAFIKX);
        assert_eq!(preview_line_style("+added line", &th).fg, Some(th.success));
        assert_eq!(preview_line_style("-removed line", &th).fg, Some(th.err));
        assert_eq!(
            preview_line_style("@@ -1,3 +1,4 @@", &th).fg,
            Some(th.secondary)
        );
        assert_eq!(preview_line_style("--- diff ---", &th).fg, Some(th.mute));
        assert_eq!(preview_line_style("--- patch ---", &th).fg, Some(th.mute));
        assert_eq!(preview_line_style(" context line", &th).fg, Some(th.text));
        assert_eq!(preview_line_style("plain text", &th).fg, Some(th.text));
    }
}
