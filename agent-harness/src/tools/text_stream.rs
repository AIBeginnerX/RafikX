use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Result, anyhow};
use regex::Regex;

use super::{MAX_FILE_BYTES, MAX_GREP_LINES, hashline};

/// A selected physical line and the final rendered response are both bounded.
pub(crate) const MAX_TEXT_LINE_BYTES: usize = MAX_FILE_BYTES as usize;
pub(crate) const MAX_TEXT_OUTPUT_BYTES: usize = MAX_FILE_BYTES as usize;
pub(crate) const TRUNCATION_MARKER: &str = "... (출력이 256KB에서 잘렸습니다)";

/// Reads one `str::lines`-compatible line without retaining bytes when not selected.
/// LF terminates a line, with an immediately preceding CR removed as CRLF.
struct PhysicalLineReader<R> {
    reader: R,
}

impl<R: BufRead> PhysicalLineReader<R> {
    fn new(reader: R) -> Self {
        Self { reader }
    }

    /// Returns false only at EOF before a new line starts. `line` is cleared first.
    fn next_line(&mut self, line: Option<&mut Vec<u8>>) -> Result<bool> {
        let mut line = line;
        if let Some(line) = &mut line {
            line.clear();
        }
        let mut saw_bytes = false;
        loop {
            let buffer = self.reader.fill_buf()?;
            if buffer.is_empty() {
                if line
                    .as_ref()
                    .is_some_and(|captured| captured.len() > MAX_TEXT_LINE_BYTES)
                {
                    return Err(anyhow!(
                        "물리 줄이 {}KB 제한을 넘습니다",
                        MAX_TEXT_LINE_BYTES / 1024
                    ));
                }
                return Ok(saw_bytes);
            }
            if line
                .as_ref()
                .is_some_and(|captured| captured.len() > MAX_TEXT_LINE_BYTES)
            {
                if buffer.first() == Some(&b'\n')
                    && line
                        .as_ref()
                        .is_some_and(|captured| captured.last() == Some(&b'\r'))
                {
                    if let Some(line) = &mut line {
                        line.pop();
                    }
                    self.reader.consume(1);
                    return Ok(true);
                }
                return Err(anyhow!(
                    "물리 줄이 {}KB 제한을 넘습니다",
                    MAX_TEXT_LINE_BYTES / 1024
                ));
            }
            let delimiter = buffer.iter().position(|byte| *byte == b'\n');
            let content_len = delimiter.unwrap_or(buffer.len());
            if let Some(line) = &mut line {
                let new_len = line.len().saturating_add(content_len);
                if new_len > MAX_TEXT_LINE_BYTES + 1 {
                    return Err(anyhow!(
                        "물리 줄이 {}KB 제한을 넘습니다",
                        MAX_TEXT_LINE_BYTES / 1024
                    ));
                }
                line.extend_from_slice(&buffer[..content_len]);
            }
            saw_bytes |= content_len > 0;
            match delimiter {
                Some(index) => {
                    self.reader.consume(index + 1);
                    if let Some(line) = &mut line {
                        if line.last() == Some(&b'\r') {
                            line.pop();
                        }
                        if line.len() > MAX_TEXT_LINE_BYTES {
                            return Err(anyhow!(
                                "물리 줄이 {}KB 제한을 넘습니다",
                                MAX_TEXT_LINE_BYTES / 1024
                            ));
                        }
                    }
                    return Ok(true);
                }
                None => {
                    if line.as_ref().is_some_and(|captured| {
                        captured.len() > MAX_TEXT_LINE_BYTES && captured.last() != Some(&b'\r')
                    }) {
                        return Err(anyhow!(
                            "물리 줄이 {}KB 제한을 넘습니다",
                            MAX_TEXT_LINE_BYTES / 1024
                        ));
                    }
                    let consumed = buffer.len();
                    self.reader.consume(consumed);
                }
            }
        }
    }
}

fn append_line(output: &mut String, text: &str, max_bytes: usize) -> Result<()> {
    let separator_len = usize::from(!output.is_empty());
    let new_len = output
        .len()
        .saturating_add(separator_len)
        .saturating_add(text.len());
    if new_len > max_bytes {
        return Err(anyhow!(
            "선택한 내용이 {}KB 제한을 넘습니다",
            max_bytes / 1024
        ));
    }
    if separator_len == 1 {
        output.push('\n');
    }
    output.push_str(text);
    Ok(())
}

/// Streams a requested page. Bytes outside that page are never accumulated or UTF-8 decoded.
pub(crate) fn read_page(
    path: &Path,
    offset: Option<u64>,
    limit: Option<u64>,
    hashline_enabled: bool,
) -> Result<String> {
    if limit == Some(0) {
        return Ok(String::new());
    }
    let file = File::open(path)?;
    let mut reader = PhysicalLineReader::new(BufReader::new(file));
    let first_line = offset.unwrap_or(1).max(1);
    let mut line_number = 1_u64;
    let mut selected = 0_u64;
    let mut bytes = Vec::new();
    let mut output = String::new();
    let mut pending_empty = None;

    while line_number < first_line {
        if !reader.next_line(None)? {
            return Ok(output);
        }
        line_number = line_number.saturating_add(1);
    }
    while !limit.is_some_and(|count| selected >= count) && reader.next_line(Some(&mut bytes))? {
        let line = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow!("선택한 범위에 UTF-8이 아닌 텍스트가 있습니다"))?;
        if hashline_enabled {
            if let Some(empty_line) = pending_empty.take() {
                append_line(
                    &mut output,
                    &format!("{}#{}|", empty_line, hashline::line_hash("")),
                    MAX_TEXT_OUTPUT_BYTES,
                )?;
            }
            if line.is_empty() {
                pending_empty = Some(line_number);
            } else {
                append_line(
                    &mut output,
                    &format!("{}#{}|{line}", line_number, hashline::line_hash(line)),
                    MAX_TEXT_OUTPUT_BYTES,
                )?;
            }
        } else {
            append_line(&mut output, line, MAX_TEXT_OUTPUT_BYTES)?;
        }
        selected = selected.saturating_add(1);
        line_number = line_number.saturating_add(1);
    }
    Ok(output)
}

pub(crate) struct GrepFileResult {
    pub(crate) output: String,
    pub(crate) emitted: usize,
    pub(crate) truncated: bool,
}

struct FileOutput {
    output: String,
    emitted: usize,
    max_lines: usize,
    max_bytes: usize,
    truncated: bool,
}

impl FileOutput {
    fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            output: String::new(),
            emitted: 0,
            max_lines,
            max_bytes,
            truncated: false,
        }
    }

    fn push(&mut self, prefix: &str, line_number: u64, line: &str) {
        if self.emitted >= self.max_lines {
            self.truncated = true;
            return;
        }
        let rendered = format!("{prefix}{line_number}:{line}");
        if append_line(&mut self.output, &rendered, self.max_bytes).is_err() {
            self.truncated = true;
            return;
        }
        self.emitted += 1;
    }

    fn separator(&mut self) {
        if self.output.is_empty() || self.truncated {
            return;
        }
        if self.emitted >= self.max_lines
            || append_line(&mut self.output, "--", self.max_bytes).is_err()
        {
            self.truncated = true;
            return;
        }
        self.emitted += 1;
    }
}

/// Searches a single file in one pass. Invalid UTF-8 or an oversized physical line rejects
/// only this file, matching the previous whole-file UTF-8 behavior without retaining it.
pub(crate) fn grep_file(
    path: &Path,
    display_path: &str,
    regex: &Regex,
    context: usize,
    max_lines: usize,
    max_bytes: usize,
) -> Result<Option<GrepFileResult>> {
    let file = File::open(path)?;
    let mut reader = PhysicalLineReader::new(BufReader::new(file));
    let mut before = VecDeque::<(u64, String)>::with_capacity(context);
    let mut bytes = Vec::new();
    let mut output = FileOutput::new(max_lines, max_bytes);
    let mut line_number = 0_u64;
    let mut after_through = 0_u64;
    let mut in_block = false;

    loop {
        let has_line = match reader.next_line(Some(&mut bytes)) {
            Ok(has_line) => has_line,
            Err(_) => return Ok(None),
        };
        if !has_line {
            break;
        }
        line_number = line_number.saturating_add(1);
        let line = match std::str::from_utf8(&bytes) {
            Ok(line) => line,
            Err(_) => return Ok(None),
        };
        let matches = regex.is_match(line);
        let included = matches || line_number <= after_through;

        if matches {
            if !in_block {
                if context > 0 {
                    output.separator();
                }
                for (previous_number, previous) in &before {
                    output.push(&format!("{display_path}-"), *previous_number, previous);
                }
                in_block = true;
            }
            output.push(&format!("{display_path}:"), line_number, line);
            after_through = after_through.max(line_number.saturating_add(context as u64));
        } else if included {
            output.push(&format!("{display_path}-"), line_number, line);
        } else {
            in_block = false;
        }

        if context > 0 {
            before.push_back((line_number, line.to_owned()));
            if before.len() > context {
                before.pop_front();
            }
        }
    }

    if output.emitted == 0 && !output.truncated {
        Ok(None)
    } else {
        Ok(Some(GrepFileResult {
            output: output.output,
            emitted: output.emitted.min(MAX_GREP_LINES),
            truncated: output.truncated,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn fixture(name: &str, body: &[u8]) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("rafikx-text-stream-{name}-{}", std::process::id()));
        fs::write(&path, body).expect("fixture write");
        path
    }

    #[test]
    fn read_page_skips_multi_megabyte_line_without_retaining_it() {
        let mut body = vec![b'x'; 3 * 1024 * 1024];
        body.extend_from_slice(b"\nsecond\n");
        let path = fixture("skip-large", &body);
        assert_eq!(
            read_page(&path, Some(2), Some(1), false).expect("page"),
            "second"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn read_page_preserves_pages_newlines_unicode_eof_and_hashline_numbers() {
        let path = fixture("pages", "one\r\ntwo\r셋\n끝".as_bytes());
        assert_eq!(
            read_page(&path, Some(1), Some(1), false).expect("first"),
            "one"
        );
        assert_eq!(
            read_page(&path, Some(2), Some(1), false).expect("middle"),
            "two\r셋"
        );
        assert_eq!(
            read_page(&path, Some(3), Some(1), false).expect("last"),
            "끝"
        );
        let tagged = read_page(&path, Some(2), Some(2), true).expect("tagged");
        assert!(tagged.starts_with(&format!("2#{}|two\r셋", hashline::line_hash("two\r셋"))));
        assert!(tagged.contains(&format!("\n3#{}|끝", hashline::line_hash("끝"))));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn read_page_only_validates_selected_utf8_and_bounds_selected_data() {
        let path = fixture("utf8", b"good\n\xffbad");
        assert_eq!(
            read_page(&path, Some(1), Some(1), false).expect("selected page"),
            "good"
        );
        assert!(read_page(&path, Some(2), Some(1), false).is_err());
        assert_eq!(
            read_page(&path, Some(99), Some(0), false).expect("zero"),
            ""
        );
        let _ = fs::remove_file(path);

        let oversized = fixture("oversized", &vec![b'x'; MAX_TEXT_LINE_BYTES + 1]);
        assert!(read_page(&oversized, Some(1), Some(1), false).is_err());
        let _ = fs::remove_file(oversized);

        let aggregate_body = "a\n".repeat(MAX_TEXT_OUTPUT_BYTES / 2 + 1);
        let aggregate = fixture("aggregate", aggregate_body.as_bytes());
        assert!(read_page(&aggregate, Some(1), None, false).is_err());
        let _ = fs::remove_file(aggregate);
    }

    #[test]
    fn hashline_omits_only_the_terminal_selected_empty_line() {
        let sole_empty = fixture("sole-empty", b"\n");
        assert_eq!(
            read_page(&sole_empty, Some(1), Some(1), true).expect("sole empty"),
            ""
        );
        let _ = fs::remove_file(sole_empty);

        let preceding_empty = fixture("preceding-empty", b"\n\n");
        assert_eq!(
            read_page(&preceding_empty, Some(1), Some(2), true).expect("two empties"),
            format!("1#{}|", hashline::line_hash(""))
        );
        let _ = fs::remove_file(preceding_empty);
    }

    #[test]
    fn grep_streams_late_matches_merges_context_and_skips_binary_files() {
        let late = fixture(
            "late",
            format!("{}needle\n", "before\n".repeat(40_000)).as_bytes(),
        );
        let regex = Regex::new("needle").expect("regex");
        let late_result = grep_file(
            &late,
            "late",
            &regex,
            0,
            MAX_GREP_LINES,
            MAX_TEXT_OUTPUT_BYTES,
        )
        .expect("grep")
        .expect("late match");
        assert!(late_result.output.ends_with("late:40001:needle"));
        let _ = fs::remove_file(late);

        let context = fixture("context", b"a\nb\nneedle-1\nd\nneedle-2\nf\ng\n");
        let result = grep_file(
            &context,
            "context",
            &regex,
            1,
            MAX_GREP_LINES,
            MAX_TEXT_OUTPUT_BYTES,
        )
        .expect("grep")
        .expect("context match");
        assert_eq!(
            result.output,
            "context-2:b\ncontext:3:needle-1\ncontext-4:d\ncontext:5:needle-2\ncontext-6:f"
        );
        let _ = fs::remove_file(context);

        let binary = fixture("binary", b"needle\n\xff");
        assert!(
            grep_file(
                &binary,
                "binary",
                &regex,
                0,
                MAX_GREP_LINES,
                MAX_TEXT_OUTPUT_BYTES
            )
            .expect("grep")
            .is_none()
        );
        let _ = fs::remove_file(binary);
    }

    #[test]
    fn grep_marks_truncation_and_counts_block_separators() {
        let path = fixture("grep-cap", b"needle\na\na\nneedle\na\na\nneedle\n");
        let regex = Regex::new("needle").expect("regex");
        let result = grep_file(&path, "cap", &regex, 1, 5, MAX_TEXT_OUTPUT_BYTES)
            .expect("grep")
            .expect("matches");
        assert!(result.truncated);
        assert_eq!(result.emitted, 5);
        assert_eq!(result.output.lines().count(), 5);
        let _ = fs::remove_file(path);
    }
}
