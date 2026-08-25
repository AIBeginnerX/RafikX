/// Display width: ASCII=1, other (Korean etc.)=2.
pub fn ch_width(ch: char) -> usize {
    if ch == '\n' || ch == '\r' {
        0
    } else if ch.is_ascii() {
        1
    } else {
        2
    }
}

#[allow(dead_code)]
pub fn display_width(s: &str) -> usize {
    s.chars().map(ch_width).sum()
}

pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    for para in s.split('\n') {
        if para.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut w = 0usize;
        for ch in para.chars() {
            let cw = ch_width(ch).max(1);
            if w + cw > width && !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                w = 0;
            }
            cur.push(ch);
            w += cw;
        }
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdKind {
    Text,
    Heading,
    Emphasis,
    Code,
    CodeBlock,
    /// /model · /connect 같은 명령어 토큰 — 답변 안에서 강조색으로 표시된다.
    Command,
    /// 정렬된 표 — 셀 폭을 계산해 그리드로 재조립한 덩어리.
    Table,
    /// 유니코드 막대그래프 — ```chart 블록의 라벨:수치 행을 실제 도표로 그린다.
    Chart,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdSeg {
    pub kind: MdKind,
    pub text: String,
    /// 코드블록 언어 태그(```rust 등) — syntax highlighting 에 사용.
    pub lang: Option<String>,
}

impl MdSeg {
    fn plain(kind: MdKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            lang: None,
        }
    }

    fn with_lang(kind: MdKind, text: impl Into<String>, lang: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            lang: Some(lang.into()),
        }
    }
}

/// Lightweight markdown split for TUI styling. Not a full parser.
pub fn markdown_segs(input: &str) -> Vec<MdSeg> {
    let lines: Vec<&str> = input.lines().collect();
    let mut out: Vec<MdSeg> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("```") {
            let lang = line[3..].trim();
            // ```chart 블록 — 라벨:수치 행을 유니코드 막대그래프로 그린다.
            if matches!(lang, "chart" | "bar" | "graph") {
                i += 1;
                let mut body: Vec<&str> = Vec::new();
                while i < lines.len() && !lines[i].starts_with("```") {
                    body.push(lines[i]);
                    i += 1;
                }
                i += 1; // 닫는 fence 소비
                out.push(MdSeg::plain(MdKind::Chart, render_chart(&body)));
                continue;
            }
            // 일반 코드블록 — fence 전체를 하나의 세그먼트로 묶어 하이라이팅한다.
            i += 1;
            let mut body: Vec<&str> = Vec::new();
            while i < lines.len() && !lines[i].starts_with("```") {
                body.push(lines[i]);
                i += 1;
            }
            i += 1; // 닫는 fence 소비 (없으면 EOF)
            out.push(if lang.is_empty() {
                MdSeg::plain(MdKind::CodeBlock, body.join("\n"))
            } else {
                MdSeg::with_lang(MdKind::CodeBlock, body.join("\n"), lang)
            });
            continue;
        }
        if line.starts_with('#') {
            out.push(MdSeg::plain(MdKind::Heading, line));
            i += 1;
            continue;
        }
        // 표 시작 — | 헤더 다음 줄이 |---| 구분자면 표 덩어리로 흡수한다.
        if is_table_line(line) && i + 1 < lines.len() && is_table_delim(lines[i + 1]) {
            let mut rows = vec![split_row(line)];
            i += 2; // 헤더와 구분자 소비
            while i < lines.len() && is_table_line(lines[i]) {
                rows.push(split_row(lines[i]));
                i += 1;
            }
            out.push(MdSeg::plain(MdKind::Table, render_grid(&rows)));
            continue;
        }
        push_inline(line, &mut out);
        i += 1;
    }
    if out.is_empty() {
        out.push(MdSeg::plain(MdKind::Text, ""));
    }
    upgrade_fenced_tables(&mut out);
    out
}

/// "라벨: 수치" 행을 유니코드 막대그래프로 변환한다. 파싱 불가한 입력은 원문을 돌려준다.
fn render_chart(body: &[&str]) -> String {
    let mut rows: Vec<(String, f64)> = Vec::new();
    for l in body {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }
        let (label, num) = match t.split_once([':', ',']) {
            Some((a, b)) => (a.trim(), b.trim()),
            None => continue,
        };
        let value: f64 = num.trim_end_matches('%').trim().parse().unwrap_or(-1.0);
        if label.is_empty() || value < 0.0 {
            continue;
        }
        rows.push((label.to_string(), value));
    }
    if rows.is_empty() {
        return body.join("\n");
    }
    let max_v = rows
        .iter()
        .map(|(_, v)| *v)
        .fold(0.0_f64, |a, b| a.max(b))
        .max(1e-9);
    let label_w = rows.iter().map(|(l, _)| display_width(l)).max().unwrap_or(0);
    const BAR_MAX: usize = 26;
    let mut out: Vec<String> = Vec::new();
    for (label, v) in &rows {
        let filled = ((*v / max_v) * BAR_MAX as f64).round() as usize;
        let filled = filled.min(BAR_MAX);
        let pad = label_w - display_width(label);
        out.push(format!(
            "{label}{} │{}{} {}",
            " ".repeat(pad),
            "█".repeat(filled),
            "░".repeat(BAR_MAX - filled),
            fmt_num(*v)
        ));
    }
    out.join("\n")
}

fn fmt_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

/// 코드블록 안에 통째로 담긴 표(모델이 흔히 그렇게 출력함)도 그리드로 승격시킨다.
fn upgrade_fenced_tables(out: &mut Vec<MdSeg>) {
    for i in 0..out.len() {
        if out[i].kind != MdKind::CodeBlock {
            continue;
        }
        let body_lines: Vec<&str> = out[i].text.split('\n').collect();
        if body_lines.len() < 2 || !body_lines.iter().all(|l| is_table_line(l)) {
            continue;
        }
        let mut rows: Vec<Vec<String>> = body_lines.iter().map(|l| split_row(l)).collect();
        // 두 번째 줄이 |---|---| 구분자면 버린다.
        if rows.len() > 1
            && rows[1]
                .iter()
                .all(|c| c.is_empty() || c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')))
        {
            rows.remove(1);
        }
        let grid = render_grid(&rows);
        out[i] = MdSeg::plain(MdKind::Table, grid);
    }
}

fn is_table_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.ends_with('|') && t.len() > 1
}

/// | --- | :---: | 같은 헤더 구분자 줄.
fn is_table_delim(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('-') || !is_table_line(t) {
        return false;
    }
    t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn split_row(line: &str) -> Vec<String> {
    let t = line.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|').map(|c| c.trim().to_string()).collect()
}

/// 셀 폭(한글 2칸 포함)의 최댓값으로 그리드를 맞춰 재조립한다.
fn render_grid(rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; ncols];
    for r in rows {
        for (idx, cell) in r.iter().enumerate() {
            let w = display_width(cell);
            if w > widths[idx] {
                widths[idx] = w;
            }
        }
    }
    // 유니코드 박스 드로잉 — omo/omp 처럼 실제 도표 형태로 보이게 한다.
    let hline = |l: &str, m: &str, r: &str| -> String {
        let mut s = String::from(l);
        for w in &widths {
            s.push_str(&"─".repeat(w + 2));
            s.push_str(m);
        }
        // 마지막 교차점을 오른쪽 모서리로
        s.pop();
        s.push_str(r);
        s
    };
    let top = hline("┌", "┬", "┐");
    let mid = hline("├", "┼", "┤");
    let bot = hline("└", "┴", "┘");
    let pad_cell = |cell: &str, w: usize| -> String {
        // 개행·탭은 정렬을 깨므로 공백으로 치환
        let clean: String = cell
            .chars()
            .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
            .collect();
        let d = display_width(&clean);
        let mut out = String::with_capacity(w + 2);
        out.push(' ');
        out.push_str(&clean);
        if d < w {
            out.push_str(&" ".repeat(w - d));
        }
        out.push(' ');
        out
    };
    let fmt = |r: &[String]| -> String {
        let mut s = String::from("│");
        for idx in 0..ncols {
            let cell = r.get(idx).map(String::as_str).unwrap_or("");
            s.push_str(&pad_cell(cell, widths[idx]));
            s.push('│');
        }
        s
    };
    let mut body: Vec<String> = vec![top, fmt(&rows[0]), mid];
    for r in &rows[1..] {
        body.push(fmt(r));
    }
    body.push(bot);
    body.join("\n")
}

/// 실제 슬래시 명령어인지 검사 — /etc/hosts 같은 경로는 강조하지 않는다.
fn is_known_command(tok: &str) -> bool {
    crate::chat::SLASH_COMMANDS.iter().any(|(name, _)| *name == tok)
}

fn push_inline(line: &str, out: &mut Vec<MdSeg>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut buf = String::new();
    let flush = |kind: MdKind, buf: &mut String, out: &mut Vec<MdSeg>| {
        if !buf.is_empty() {
            out.push(MdSeg::plain(kind, std::mem::take(buf)));
        }
    };
    while i < chars.len() {
        // /명령어 토큰 — 공백 전까지를 하나의 강조 세그먼트로 묶는다.
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1].is_ascii_alphabetic() {
            flush(MdKind::Text, &mut buf, out);
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            let tok: String = chars[start..i].iter().collect();
            let kind = if is_known_command(&tok) {
                MdKind::Command
            } else {
                MdKind::Text
            };
            out.push(MdSeg::plain(kind, tok));
            continue;
        }
        if chars[i] == '`' {
            flush(MdKind::Text, &mut buf, out);
            i += 1;
            let mut code = String::new();
            while i < chars.len() && chars[i] != '`' {
                code.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            out.push(MdSeg::plain(MdKind::Code, code));
            continue;
        }
        if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            flush(MdKind::Text, &mut buf, out);
            i += 2;
            let mut em = String::new();
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '*') {
                em.push(chars[i]);
                i += 1;
            }
            if i + 1 < chars.len() {
                i += 2;
            }
            out.push(MdSeg::plain(MdKind::Emphasis, em));
            continue;
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(MdKind::Text, &mut buf, out);
}

pub const KEY_HELP: &str = "\
Enter            보내기\n\
Ctrl+J / Shift+Enter  줄바꿈\n\
Tab              plan(읽기전용)/build 모드 전환\n\
Esc              생성 중단 · 도움말·선택·승인 닫기\n\
Ctrl+C           종료 (실행 중이면 이번 턴이 끝난 뒤)\n\
PgUp / PgDn      대화 스크롤\n\
?                이 도움말 (입력이 비었을 때)\n\
/model /provider /connect  목록에서 고르기\n\
/connect         키 붙여넣기 칸 (Ctrl+V)\n\
y / n / a        도구 승인 (이번만 / 거부 / 이번 실행 모두)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_ascii_and_korean() {
        let lines = wrap_text("hello world", 5);
        assert!(lines.len() >= 2);
        let k = wrap_text("안녕", 2);
        assert_eq!(k, vec!["안".to_string(), "녕".to_string()]);
        assert_eq!(display_width("안녕"), 4);
    }

    #[test]
    fn markdown_heading_and_code() {
        let segs = markdown_segs("# 제목\n`code` and **bold**\n");
        assert!(segs.iter().any(|s| s.kind == MdKind::Heading));
        assert!(segs.iter().any(|s| s.kind == MdKind::Code && s.text == "code"));
        assert!(segs.iter().any(|s| s.kind == MdKind::Emphasis && s.text == "bold"));
    }

    #[test]
    fn slash_commands_are_highlighted() {
        let segs = markdown_segs("모드는 /mode plan 으로 바꾸세요. 경로 /etc/hosts 는 아니다.");
        assert!(segs
            .iter()
            .any(|s| s.kind == MdKind::Command && s.text == "/mode"));
        // 숫자·기호로 시작하면 명령어가 아니라 일반 텍스트다.
        assert!(!segs
            .iter()
            .any(|s| s.kind == MdKind::Command && s.text.starts_with("/etc")));
    }

    #[test]
    fn tables_render_as_aligned_grid() {
        let md = "| 이름 | 값 |\n|---|---|\n| alpha | 1 |\n| 한글 | 22 |\n";
        let segs = markdown_segs(md);
        let table = segs
            .iter()
            .find(|s| s.kind == MdKind::Table)
            .expect("table seg");
        assert!(table.text.contains("┌───────┬────┐"));
        assert!(table.text.contains("│ 이름  │ 값 │"));
        assert!(table.text.contains("│ 한글  │ 22 │"));
        assert!(table.text.contains("├───────┼────┤"));
        assert!(table.text.ends_with("└───────┴────┘"));
        // 구분자 줄(---)은 그리드에서 사라진다.
        assert!(!table.text.contains("---|"));
    }

    #[test]
    fn fenced_tables_are_promoted_to_grids() {
        // 모델이 표를 ``` 블록에 통째로 담는 경우도 그리드로 승격되어야 한다.
        let md = "결과:\n```\n| A | B |\n|---|---|\n| 1 | 2 |\n```\n끝";
        let segs = markdown_segs(md);
        assert!(segs.iter().any(|s| s.kind == MdKind::Table));
        // 일반 코드블록은 그대로 유지된다.
        let code = markdown_segs("```rust\nfn a() {}\n```");
        assert!(!code.iter().any(|s| s.kind == MdKind::Table));
    }

    #[test]
    fn chart_blocks_render_unicode_bars() {
        let md = "```chart\nA: 100\nB: 50\n```\n";
        let segs = markdown_segs(md);
        let chart = segs
            .iter()
            .find(|s| s.kind == MdKind::Chart)
            .expect("chart seg");
        assert!(chart.text.contains('█'));
        assert!(chart.text.contains("100"));
        assert!(!chart.text.contains("```"));
    }

    #[test]
    fn key_help_documents_enter_and_newline() {
        assert!(KEY_HELP.contains("Enter"));
        assert!(KEY_HELP.contains("Ctrl+J"));
        assert!(KEY_HELP.contains("y / n / a"));
    }
}
