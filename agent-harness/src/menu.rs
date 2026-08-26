use std::io::{self, Write};

/// 번호 메뉴 파싱. 번호는 1부터 `max`까지. `0`은 완료/뒤로.
/// `allow_multi`이면 `1,2,3` 또는 `1 2 3` 을 받는다.
pub fn parse_numbers(
    input: &str,
    max: usize,
    allow_multi: bool,
    allow_zero: bool,
) -> Option<Vec<usize>> {
    let t = input.trim();
    if t.is_empty() {
        return None;
    }
    let parts: Vec<&str> = if t.contains(',') {
        t.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    } else if t.split_whitespace().count() > 1 {
        t.split_whitespace().collect()
    } else {
        vec![t]
    };
    if parts.is_empty() {
        return None;
    }
    if !allow_multi && parts.len() > 1 {
        return None;
    }
    let mut out = Vec::new();
    for p in &parts {
        let Ok(n) = p.parse::<usize>() else {
            return None;
        };
        if n == 0 {
            if !allow_zero || parts.len() != 1 {
                return None;
            }
            return Some(vec![0]);
        }
        if n < 1 || n > max {
            return None;
        }
        if !out.contains(&n) {
            out.push(n);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// 1-based indices whose label uniquely matches `query` (case-insensitive).
pub fn match_items(query: &str, items: &[String]) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut exact = Vec::new();
    let mut prefix = Vec::new();
    let mut contains = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let n = i + 1;
        let t = item.to_lowercase();
        if t == q {
            exact.push(n);
            continue;
        }
        let words: Vec<&str> = t
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        if t.starts_with(&q) || words.iter().any(|w| *w == q || w.starts_with(&q)) {
            prefix.push(n);
        } else if t.contains(&q) {
            contains.push(n);
        }
    }
    if !exact.is_empty() {
        exact
    } else if prefix.len() == 1 {
        prefix
    } else if prefix.is_empty() && contains.len() == 1 {
        contains
    } else if prefix.len() > 1 {
        prefix
    } else {
        contains
    }
}

pub fn print_menu(title: &str, items: &[String], extra: &str) {
    crate::ui::section(title);
    for (i, item) in items.iter().enumerate() {
        println!("   {}  {item}", crate::ui::cyan(&format!("[{}]", i + 1)));
    }
    println!("   {}  완료 / 뒤로", crate::ui::dim("[0]"));
    if !extra.is_empty() {
        println!("   {}", crate::ui::dim(extra));
    }
}

/// 유효한 번호가 나올 때까지 다시 묻는다. 빈 입력·범위 밖은 재입력.
pub fn prompt_choice(
    title: &str,
    items: &[String],
    allow_multi: bool,
    extra: &str,
) -> io::Result<Vec<usize>> {
    loop {
        println!();
        print_menu(title, items, extra);
        print!("{} ", crate::ui::gold("번호 또는 이름 ›"));
        io::stdout().flush()?;
        let mut line = String::new();
        let n = io::stdin().read_line(&mut line)?;
        if n == 0 {
            return Ok(vec![0]);
        }
        match parse_numbers(&line, items.len(), allow_multi, true) {
            Some(v) => return Ok(v),
            None => {
                if !allow_multi {
                    let hits = match_items(line.trim(), items);
                    if hits.len() == 1 {
                        return Ok(hits);
                    }
                    if hits.len() > 1 {
                        println!("여러 항목이 맞습니다. 번호를 쓰세요.");
                        continue;
                    }
                }
                println!("없는 번호입니다. 이름(예: zen) 또는 번호를 다시 고르세요.");
            }
        }
    }
}

pub fn prompt_line(label: &str) -> io::Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

pub fn prompt_secret(label: &str) -> io::Result<String> {
    prompt_line(label)
}

/// 콘솔용 키 입력 칸. 붙여넣기(Ctrl+V / 오른쪽 클릭)가 stdin으로 들어온다.
pub fn prompt_api_key_box(
    provider_label: &str,
    url: Option<&str>,
    env_hint: &str,
) -> io::Result<Option<String>> {
    println!();
    println!("   {}", crate::ui::gold("── API 키 ──"));
    println!("   {provider_label}");
    if let Some(u) = url {
        println!("   키 발급  {}", crate::ui::cyan(u));
    }
    if !env_hint.is_empty() {
        println!("   환경변수  {env_hint}");
    }
    println!(
        "   {}",
        crate::ui::dim("키를 붙여넣으세요 (Ctrl+V) · Enter 저장 · 빈 줄 취소")
    );
    print!("   {} ", crate::ui::gold("키 ›"));
    io::stdout().flush()?;
    let mut line = String::new();
    let n = io::stdin().read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    let key = crate::accounts_ui::sanitize_pasted_key(&line);
    if key.is_empty() {
        Ok(None)
    } else {
        Ok(Some(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_and_zero() {
        assert_eq!(parse_numbers("1", 5, false, true), Some(vec![1]));
        assert_eq!(parse_numbers(" 5 ", 5, false, true), Some(vec![5]));
        assert_eq!(parse_numbers("0", 5, false, true), Some(vec![0]));
        assert_eq!(parse_numbers("6", 5, false, true), None);
        assert_eq!(parse_numbers("abc", 5, false, true), None);
        assert_eq!(parse_numbers("", 5, false, true), None);
        assert_eq!(parse_numbers("1,2", 5, false, true), None);
    }

    #[test]
    fn parse_multi_select() {
        assert_eq!(parse_numbers("1,2,3", 5, true, true), Some(vec![1, 2, 3]));
        assert_eq!(parse_numbers("1, 2, 3", 5, true, true), Some(vec![1, 2, 3]));
        assert_eq!(parse_numbers("1 2 3", 5, true, true), Some(vec![1, 2, 3]));
        assert_eq!(parse_numbers("1,1,2", 5, true, true), Some(vec![1, 2]));
        assert_eq!(parse_numbers("1,0", 5, true, true), None);
        assert_eq!(parse_numbers("9,1", 5, true, true), None);
    }

    #[test]
    fn match_zen_and_go_labels() {
        let items = vec![
            "OpenCode Zen  (키)".into(),
            "OpenCode Go  (키)".into(),
            "Anthropic  (로그인)".into(),
        ];
        assert_eq!(match_items("zen", &items), vec![1]);
        assert_eq!(match_items("go", &items), vec![2]);
        assert_eq!(match_items("anthropic", &items), vec![3]);
    }
}
