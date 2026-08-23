/// Display width: ASCII=1, other (Korean etc.)=2. Good enough for cmd/WT.
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdSeg {
    pub kind: MdKind,
    pub text: String,
}

/// Lightweight markdown split for TUI styling. Not a full parser.
pub fn markdown_segs(input: &str) -> Vec<MdSeg> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in input.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        let nl = if line.ends_with('\n') { "\n" } else { "" };
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out.push(MdSeg {
                kind: MdKind::CodeBlock,
                text: format!("{trimmed}{nl}"),
            });
            continue;
        }
        if in_fence {
            out.push(MdSeg {
                kind: MdKind::CodeBlock,
                text: line.to_string(),
            });
            continue;
        }
        if trimmed.starts_with('#') {
            out.push(MdSeg {
                kind: MdKind::Heading,
                text: line.to_string(),
            });
            continue;
        }
        push_inline(line, &mut out);
    }
    if out.is_empty() {
        out.push(MdSeg {
            kind: MdKind::Text,
            text: String::new(),
        });
    }
    out
}

fn push_inline(line: &str, out: &mut Vec<MdSeg>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut buf = String::new();
    let flush = |kind: MdKind, buf: &mut String, out: &mut Vec<MdSeg>| {
        if !buf.is_empty() {
            out.push(MdSeg {
                kind,
                text: std::mem::take(buf),
            });
        }
    };
    while i < chars.len() {
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
            out.push(MdSeg {
                kind: MdKind::Code,
                text: code,
            });
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
            out.push(MdSeg {
                kind: MdKind::Emphasis,
                text: em,
            });
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
Ctrl+C           종료 (실행 중이면 이번 턴이 끝난 뒤)\n\
Esc              도움말·선택·승인 닫기\n\
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
    fn key_help_documents_enter_and_newline() {
        assert!(KEY_HELP.contains("Enter"));
        assert!(KEY_HELP.contains("Ctrl+J"));
        assert!(KEY_HELP.contains("y / n / a"));
    }
}
