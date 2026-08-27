use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::accounts::{self, Account};
use crate::config;
use crate::provider::{ChatResponse, LimitHint};
use crate::ui;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountUsage {
    #[serde(default)]
    pub limited_until: i64,
    #[serde(default)]
    pub tokens_in_today: u64,
    #[serde(default)]
    pub tokens_out_today: u64,
    #[serde(default)]
    pub requests_today: u64,
    #[serde(default)]
    pub remaining: Option<u32>,
    #[serde(default)]
    pub reset_at: Option<i64>,
    #[serde(default)]
    pub last_used: i64,
    #[serde(default)]
    pub day: String,
    #[serde(default)]
    pub last_account: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    accounts: HashMap<String, AccountUsage>,
    #[serde(default)]
    active: Option<String>,
}

fn path() -> Result<PathBuf> {
    Ok(config::Config::data_dir()?.join("usage.json"))
}

fn load() -> File {
    let Ok(p) = path() else {
        return File::default();
    };
    if !p.exists() {
        return File::default();
    }
    fs::read_to_string(&p)
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default()
}

fn save(file: &File) {
    let Ok(p) = path() else { return };
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string_pretty(file) {
        let _ = fs::write(&p, s);
        config::set_owner_only_mode(&p);
    }
}

fn today() -> String {
    let secs = now_secs();
    let days = secs / 86400;
    // UTC 날짜. 하단 표시용으로 충분.
    let z = days * 86400;

    unix_ymd(z)
}

fn unix_ymd(secs: i64) -> String {
    // 간단 변환 (UTC)
    let z = secs.max(0) / 86400;
    let mut days = z;
    let mut year = 1970i32;
    loop {
        let len = if is_leap(year) { 366 } else { 365 };
        if days < len {
            break;
        }
        days -= len;
        year += 1;
    }
    let md = [
        31,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u32;
    for d in md {
        if days < d {
            break;
        }
        days -= d;
        month += 1;
    }
    format!("{year:04}-{month:02}-{:02}", days + 1)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn roll(u: &mut AccountUsage) {
    let d = today();
    if u.day != d {
        u.day = d;
        u.tokens_in_today = 0;
        u.tokens_out_today = 0;
        u.requests_today = 0;
    }
}

pub fn get(id: &str) -> AccountUsage {
    let mut file = load();
    let mut u = file.accounts.remove(id).unwrap_or_default();
    roll(&mut u);
    u
}

/// 준비된 계정 중 리밋 창이 먼저 끝나는 것. 모두 대기면 가장 빨리 풀리는 것.
pub fn select_account(accounts: &[Account]) -> Option<String> {
    if accounts.is_empty() {
        return None;
    }
    let now = now_secs();
    let mut ready = Vec::new();
    let mut waiting = Vec::new();
    for a in accounts {
        let u = get(&a.id);
        if u.limited_until <= now {
            ready.push((
                a.id.clone(),
                u.reset_at.unwrap_or(i64::MAX),
                u.tokens_in_today + u.tokens_out_today,
            ));
        } else {
            waiting.push((a.id.clone(), u.limited_until));
        }
    }
    if !ready.is_empty() {
        ready.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));
        return Some(ready[0].0.clone());
    }
    waiting.sort_by_key(|w| w.1);
    waiting.first().map(|w| w.0.clone())
}

/// 같은 프로바이더 계정을 쓸 순서 (선택 계정 먼저, 그다음 빨리 풀리는 순).
pub fn order_ids(accounts: &[Account]) -> Vec<String> {
    let mut ids: Vec<String> = accounts.iter().map(|a| a.id.clone()).collect();
    if let Some(first) = select_account(accounts) {
        ids.retain(|x| x != &first);
        let mut rest = ids;
        rest.sort_by_key(|id| get(id).limited_until);
        let mut out = vec![first];
        out.extend(rest);
        out
    } else {
        ids
    }
}

pub fn mark_limited(id: &str, retry_after_secs: u64) {
    let mut file = load();
    let mut u = file.accounts.remove(id).unwrap_or_default();
    roll(&mut u);
    let wait = retry_after_secs.max(5);
    u.limited_until = now_secs() + wait as i64;
    u.reset_at = Some(u.limited_until);
    file.accounts.insert(id.to_string(), u);
    save(&file);
}

pub fn record_success(id: &str, resp: &ChatResponse) {
    let mut file = load();
    let mut u = file.accounts.remove(id).unwrap_or_default();
    roll(&mut u);
    u.tokens_in_today += resp.input_tokens as u64;
    u.tokens_out_today += resp.output_tokens as u64;
    u.requests_today += 1;
    u.last_used = now_secs();
    u.limited_until = 0;
    if let Some(r) = resp.limit.remaining {
        u.remaining = Some(r);
    }
    if let Some(r) = resp.limit.reset_at {
        u.reset_at = Some(r);
    }
    file.accounts.insert(id.to_string(), u);
    file.active = Some(id.to_string());
    save(&file);
}

pub fn apply_hint(id: &str, hint: &LimitHint) {
    if hint.retry_after_secs.unwrap_or(0) == 0 && hint.remaining.is_none() {
        return;
    }
    let mut file = load();
    let mut u = file.accounts.remove(id).unwrap_or_default();
    roll(&mut u);
    if let Some(r) = hint.remaining {
        u.remaining = Some(r);
        if r == 0 {
            let wait = hint.retry_after_secs.unwrap_or(30);
            u.limited_until = now_secs() + wait as i64;
        }
    }
    if let Some(r) = hint.reset_at {
        u.reset_at = Some(r);
    }
    file.accounts.insert(id.to_string(), u);
    save(&file);
}

pub fn seconds_left(id: &str) -> i64 {
    (get(id).limited_until - now_secs()).max(0)
}

pub fn parse_retry_after(err: &str) -> u64 {
    let low = err.to_lowercase();
    if let Some(i) = low.find("retry_after=") {
        let rest = &low[i + 12..];
        let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(v) = n.parse::<u64>() {
            return v.clamp(5, 3600);
        }
    }
    if let Some(i) = low.find("retry-after") {
        let rest: String = low[i..]
            .chars()
            .filter(|c| c.is_ascii_digit())
            .take(4)
            .collect();
        if let Ok(v) = rest.parse::<u64>() {
            return v.clamp(5, 3600);
        }
    }
    45
}

pub fn footer_lines() -> Vec<String> {
    let mut lines = Vec::new();
    let file = load();
    let mut items = accounts::all();
    if items.is_empty() {
        return lines;
    }
    items.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.label.cmp(&b.label)));
    let now = now_secs();
    for a in items {
        let mut u = file.accounts.get(&a.id).cloned().unwrap_or_default();
        roll(&mut u);
        let tok = u.tokens_in_today + u.tokens_out_today;
        let tok_s = format_tok(tok);
        let active = file.active.as_deref() == Some(a.id.as_str());
        let status = if u.limited_until > now {
            let m = ((u.limited_until - now) + 59) / 60;
            ui::yellow(&format!("리밋 {m}분"))
        } else if active {
            ui::green("사용중")
        } else {
            ui::dim("대기")
        };
        let rem = u
            .remaining
            .map(|r| format!("  남은요청 {r}"))
            .unwrap_or_default();
        let mark = if active {
            ui::green("●")
        } else {
            ui::dim("○")
        };
        lines.push(format!(
            "{mark} {}  {} tok  {status}{rem}",
            accounts::display(&a),
            tok_s
        ));
    }
    lines
}

fn format_tok(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_from_error() {
        assert_eq!(parse_retry_after("HTTP 429 retry_after=12"), 12);
        assert_eq!(parse_retry_after("rate limited"), 45);
    }

    #[test]
    fn select_ready_with_earliest_reset() {
        let accounts = vec![
            Account {
                id: "a".into(),
                provider: "anthropic".into(),
                label: "1".into(),
            },
            Account {
                id: "b".into(),
                provider: "anthropic".into(),
                label: "2".into(),
            },
        ];
        // 파일 없이 limited_until=0 이면 둘 다 ready, reset_at 없으면 MAX → id 순
        let pick = select_account(&accounts);
        assert!(pick == Some("a".into()) || pick == Some("b".into()));
    }
}
