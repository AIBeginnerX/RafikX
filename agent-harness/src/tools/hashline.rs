//! 해시 앵커 편집 (Hashline) — oh-my-openagent 의 hashline 에서 영감.
//!
//! read 출력의 각 줄에 `N#HASH|` 태그를 붙이고, 편집 요청이 앵커(시작·끝)를
//! 들고 오면 현재 파일의 해당 줄 해시와 대조한다. 어긋나면 쓰기 전에 원자적으로
//! 거부한다 — 모델이 본 내용을 그대로 재생산해야 하는 old_str 방식의
//! stale-line 실패를 구조적으로 제거하는 것이 목적이다.

use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};

use crate::tools::ToolCtx;

const B36: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// old_str 모드 실패 시 붙이는 안내 — 앵커 모드로 유도.
pub const ANCHOR_HINT: &str = "힌트: read_file 출력의 N#HASH 태그를 anchors(start/end) 인자로 쓰면 내용 재생산 없이 정확히 편집할 수 있습니다.";

/// 줄 내용의 단축 해시 — sha256 앞 2바이트를 base36 3글자로 (결정적).
pub fn line_hash(line: &str) -> String {
    let digest = Sha256::digest(line.as_bytes());
    let mut n = (((digest[0] as u32) << 8) | digest[1] as u32) % (36 * 36 * 36);
    let mut out = vec![b'0'; 3];
    for i in (0..3).rev() {
        out[i] = B36[(n % 36) as usize];
        n /= 36;
    }
    String::from_utf8(out).unwrap()
}

/// 읽기 결과를 `N#HASH|내용` 줄로 태그한다. start_line 은 1부터.
#[cfg(test)]
pub fn tag_lines(slice: &str, start_line: usize) -> String {
    slice
        .lines()
        .enumerate()
        .map(|(i, l)| format!("{}#{}|{}", start_line + i, line_hash(l), l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `12#abc` 앵커를 (줄번호, 해시)로 파싱.
pub fn parse_anchor(raw: &str) -> Result<(usize, String)> {
    let (num, hash) = raw
        .trim()
        .split_once('#')
        .ok_or_else(|| anyhow!("앵커 형식이 아닙니다 (예: 12#abc): {raw}"))?;
    let line: usize = num
        .trim()
        .parse()
        .map_err(|_| anyhow!("앵커 줄번호가 숫자가 아닙니다: {raw}"))?;
    if line == 0 {
        return Err(anyhow!("앵커 줄번호는 1부터입니다"));
    }
    let hash = hash.trim().to_string();
    if hash.len() != 3 {
        return Err(anyhow!("앵커 해시는 3글자입니다: {raw}"));
    }
    Ok((line, hash))
}

/// 앵커 구간을 현재 본문과 대조해 (시작 인덱스, 끝 인덱스, 0-based)를 돌려준다.
/// 하나라도 어긋나면 Err — 호출부는 아무것도 쓰지 않는다 (원자 거부).
pub fn verify_span(body: &str, start_raw: &str, end_raw: &str) -> Result<(usize, usize)> {
    let (s_line, s_hash) = parse_anchor(start_raw)?;
    let (e_line, e_hash) = parse_anchor(end_raw)?;
    if e_line < s_line {
        return Err(anyhow!("앵커 순서가 뒤집혔습니다: {start_raw} ~ {end_raw}"));
    }
    let lines: Vec<&str> = body.lines().collect();
    if e_line > lines.len() {
        return Err(anyhow!(
            "앵커 줄({e_line})이 파일 줄 수({})를 넘습니다 — 파일을 다시 읽으세요.",
            lines.len()
        ));
    }
    let actual_s = line_hash(lines[s_line - 1]);
    if actual_s != s_hash {
        return Err(anyhow!(
            "시작 앵커 해시가 다릅니다 (줄 {s_line}: 기대 {s_hash}, 실제 {actual_s}) — 파일이 바뀌었습니다. 다시 읽고 앵커를 확인하세요."
        ));
    }
    let actual_e = line_hash(lines[e_line - 1]);
    if actual_e != e_hash {
        return Err(anyhow!(
            "끝 앵커 해시가 다릅니다 (줄 {e_line}: 기대 {e_hash}, 실제 {actual_e}) — 파일이 바뀌었습니다. 다시 읽고 앵커를 확인하세요."
        ));
    }
    Ok((s_line - 1, e_line - 1))
}

/// 검증이 끝난 구간을 new_text 로 치환한 새 본문을 만든다.
pub fn replace_span(body: &str, start_idx: usize, end_idx: usize, new_text: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut out: Vec<&str> = lines[..start_idx].to_vec();
    out.extend(new_text.lines());
    out.extend_from_slice(&lines[end_idx + 1..]);
    let mut joined = out.join("\n");
    if body.ends_with('\n') && !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

/// 편집 성공/실패를 run graph 에 기록 — Inspector 가 편집 성공률을 집계하는 근거.
pub fn record_metric(ctx: &ToolCtx, tool: &str, outcome: &str) {
    if let Some(run) = &ctx.run {
        crate::graph::node_in(run, "edit_metric", tool, outcome, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "fn main() {\n    println!(\"hi\");\n}\n";

    #[test]
    fn hash_is_deterministic_and_three_chars() {
        assert_eq!(line_hash("fn main() {").len(), 3);
        assert_eq!(line_hash("abc"), line_hash("abc"));
        assert_ne!(line_hash("abc"), line_hash("abd"));
    }

    #[test]
    fn tag_format() {
        let tagged = tag_lines("a\nb", 1);
        let lines: Vec<&str> = tagged.lines().collect();
        assert!(lines[0].starts_with("1#"));
        assert!(lines[0].ends_with("|a"));
        assert!(lines[1].starts_with("2#"));
    }

    #[test]
    fn anchor_roundtrip() {
        let tagged = tag_lines(BODY.trim_end(), 1);
        let first = tagged.lines().next().unwrap();
        let anchor = first.split('|').next().unwrap();
        let (line, hash) = parse_anchor(anchor).unwrap();
        assert_eq!(line, 1);
        assert_eq!(hash, line_hash("fn main() {"));
    }

    #[test]
    fn verify_span_accepts_matching_anchors() {
        let (s, e) = verify_span(
            BODY,
            &format!("1#{}", line_hash("fn main() {")),
            &format!("3#{}", line_hash("}")),
        )
        .unwrap();
        assert_eq!((s, e), (0, 2));
    }

    #[test]
    fn verify_span_rejects_stale_hash_atomically() {
        let err = verify_span(BODY, "1#zzz", "2#aaa").unwrap_err();
        assert!(err.to_string().contains("파일이 바뀌었습니다"));
    }

    #[test]
    fn verify_span_rejects_reversed_and_overflow() {
        assert!(verify_span(BODY, "3#aaa", "1#bbb").is_err());
        assert!(verify_span(BODY, "1#aaa", "99#bbb").is_err());
    }

    #[test]
    fn replace_span_swaps_range_and_keeps_trailing_newline() {
        let (s, e) = verify_span(
            BODY,
            &format!("2#{}", line_hash("    println!(\"hi\");")),
            &format!("2#{}", line_hash("    println!(\"hi\");")),
        )
        .unwrap();
        let out = replace_span(BODY, s, e, "    println!(\"bye\");");
        assert_eq!(out, "fn main() {\n    println!(\"bye\");\n}\n");
    }

    #[test]
    fn replace_span_can_delete_range() {
        let (s, e) = verify_span(
            BODY,
            &format!("2#{}", line_hash("    println!(\"hi\");")),
            &format!("2#{}", line_hash("    println!(\"hi\");")),
        )
        .unwrap();
        let out = replace_span(BODY, s, e, "");
        assert_eq!(out, "fn main() {\n}\n");
    }
}
