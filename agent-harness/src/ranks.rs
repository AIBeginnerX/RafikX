use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::Config;

const BUNDLED: &str = include_str!("../data/model_ranks.json");
const STALE_SECS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankTable {
    pub updated: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub fetch_url: String,
    #[serde(default)]
    pub last_fetch_attempt: String,
    pub models: Vec<RankEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankEntry {
    pub id_aliases: Vec<String>,
    pub score: i32,
    pub tier: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Top5,
    Strong,
    Other,
}

impl Tier {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "top5" => Tier::Top5,
            "strong" => Tier::Strong,
            _ => Tier::Other,
        }
    }
}

pub fn bundled() -> RankTable {
    serde_json::from_str(BUNDLED).unwrap_or_else(|_| RankTable {
        updated: "unknown".into(),
        source: "bundle parse failed".into(),
        fetch_url: String::new(),
        last_fetch_attempt: String::new(),
        models: Vec::new(),
    })
}

fn ranks_path() -> Result<PathBuf> {
    Ok(Config::data_dir()?.join("ranks.json"))
}

pub fn load() -> RankTable {
    if let Ok(path) = ranks_path() {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(t) = serde_json::from_str::<RankTable>(&raw) {
                if !t.models.is_empty() {
                    return t;
                }
            }
        }
    }
    bundled()
}

pub fn save(table: &RankTable) -> Result<()> {
    let path = ranks_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(table)?)?;
    Ok(())
}

pub fn normalize_id(id: &str) -> String {
    let mut s = id.rsplit('/').next().unwrap_or(id).to_ascii_lowercase();
    s = s.replace('_', "-");
    if let Some(idx) = s.find(':') {
        // keep ollama tag prefix like qwen3:8b as-is except lowercased
        let _ = idx;
    }
    s = s.trim_end_matches("-latest").trim_end_matches(":latest").to_string();
    // strip trailing YYYYMMDD or YYYY-MM-DD conservatively
    let nlen = s.len();
    if nlen >= 9 {
        let tail = s[nlen - 9..].to_string();
        if tail.starts_with('-') && tail[1..].chars().all(|c| c.is_ascii_digit()) {
            s.truncate(nlen - 9);
        }
    }
    let nlen = s.len();
    if nlen >= 11 {
        let tail = s[nlen - 11..].to_string();
        if tail.len() == 11
            && tail.as_bytes()[0] == b'-'
            && tail.as_bytes()[5] == b'-'
            && tail.as_bytes()[8] == b'-'
            && tail[1..].chars().all(|c| c.is_ascii_digit() || c == '-')
        {
            s.truncate(nlen - 11);
        }
    }
    s
}

pub fn match_entry<'a>(table: &'a RankTable, model_id: &str) -> Option<&'a RankEntry> {
    let norm = normalize_id(model_id);
    let mut best: Option<(&RankEntry, usize)> = None;
    for e in &table.models {
        for alias in &e.id_aliases {
            let a = normalize_id(alias);
            if a.is_empty() {
                continue;
            }
            let hit = norm == a
                || (a.len() >= 4 && (norm.contains(&a) || a.contains(&norm)));
            if hit {
                let score = a.len();
                if best.map(|(_, n)| n).unwrap_or(0) < score {
                    best = Some((e, score));
                }
            }
        }
    }
    best.map(|(e, _)| e)
}

pub fn score_of(table: &RankTable, model_id: &str) -> Option<i32> {
    match_entry(table, model_id).map(|e| e.score)
}

#[allow(dead_code)]
pub fn tier_of(table: &RankTable, model_id: &str) -> Option<Tier> {
    match_entry(table, model_id).map(|e| Tier::parse(&e.tier))
}

pub fn is_cheap_id(model_id: &str) -> bool {
    let n = normalize_id(model_id);
    ["haiku", "flash", "mini", "nano", "lite", "small", "turbo", "instant", "luna"]
        .iter()
        .any(|k| n.contains(k))
}

fn today() -> String {
    // YYYY-MM-DD from unix days — good enough for stale checks; display uses file mtime too.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    // 1970-01-01 + days, civil date (proleptic gregorian)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn file_age_secs(path: &std::path::Path) -> Option<u64> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    SystemTime::now().duration_since(modified).ok().map(|d| d.as_secs())
}

pub fn is_stale() -> bool {
    let Ok(path) = ranks_path() else {
        return true;
    };
    if !path.exists() {
        return true;
    }
    match file_age_secs(&path) {
        Some(age) => age > STALE_SECS,
        None => true,
    }
}

pub fn status_line() -> String {
    let t = load();
    format!(
        "모델 순위 기준일: {}  ({})",
        t.updated,
        if t.source.is_empty() {
            "번들"
        } else {
            t.source.split('.').next().unwrap_or(t.source.as_str())
        }
    )
}

/// doctor/settings/ask 시작 시 싼 검사. 실패해도 본 작업은 계속.
pub async fn maybe_refresh_quiet() {
    if !is_stale() {
        return;
    }
    match refresh(false).await {
        Ok(msg) => println!("{msg}"),
        Err(_) => println!("순위는 번들 기준, 오프라인이면 로컬 유지"),
    }
}

pub async fn refresh(force: bool) -> Result<String> {
    let mut table = load();
    if !force && !is_stale() {
        return Ok(format!(
            "순위표가 최신입니다 (기준일 {}).",
            table.updated
        ));
    }
    let url = if table.fetch_url.trim().is_empty() {
        bundled().fetch_url
    } else {
        table.fetch_url.clone()
    };
    table.last_fetch_attempt = today();
    if url.trim().is_empty() {
        let _ = save(&table);
        return Ok("순위는 번들 기준, 오프라인이면 로컬 유지".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()?;
    let resp = match client.get(&url).header("accept", "application/json").send().await {
        Ok(r) => r,
        Err(_) => {
            let _ = save(&table);
            return Ok("순위는 번들 기준, 오프라인이면 로컬 유지".into());
        }
    };
    if !resp.status().is_success() {
        let _ = save(&table);
        return Ok("순위는 번들 기준, 오프라인이면 로컬 유지".into());
    }
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ctype.contains("text/html") {
        let _ = save(&table);
        return Ok("순위는 번들 기준, 오프라인이면 로컬 유지".into());
    }
    let text = resp.text().await.unwrap_or_default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        let _ = save(&table);
        return Ok("순위는 번들 기준, 오프라인이면 로컬 유지".into());
    };
    let merged = merge_remote(table, &v);
    save(&merged)?;
    Ok(format!(
        "순위표를 갱신했습니다 (기준일 {}).",
        merged.updated
    ))
}

fn merge_remote(mut table: RankTable, v: &serde_json::Value) -> RankTable {
    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| v.as_array());
    let Some(arr) = arr else {
        return table;
    };
    let mut changed = false;
    for item in arr {
        let slug = item
            .get("slug")
            .or_else(|| item.get("name"))
            .or_else(|| item.get("id"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if slug.is_empty() {
            continue;
        }
        let score = item
            .pointer("/evaluations/artificial_analysis_intelligence_index")
            .or_else(|| item.get("score"))
            .and_then(|x| x.as_f64())
            .map(|f| f.round() as i32);
        let Some(score) = score else { continue };
        if let Some(e) = table
            .models
            .iter_mut()
            .find(|e| e.id_aliases.iter().any(|a| ids_close(a, slug)))
        {
            if e.score != score {
                e.score = score;
                changed = true;
            }
        }
    }
    if changed {
        table.updated = today();
        table.source = format!(
            "원격 JSON 병합 + 번들 별칭 ({})",
            table.source
        );
    }
    table
}

fn ids_close(alias: &str, slug: &str) -> bool {
    let a = normalize_id(alias);
    let s = normalize_id(slug);
    a == s || (a.len() >= 4 && (s.contains(&a) || a.contains(&s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_aliases_and_dates() {
        let t = bundled();
        assert!(!t.models.is_empty());
        let e = match_entry(&t, "anthropic/claude-opus-5-20260801").expect("opus");
        assert!(e.score >= 60);
        assert_eq!(Tier::parse(&e.tier), Tier::Top5);
        let e = match_entry(&t, "gpt-5.6-sol").expect("gpt");
        assert!(e.id_aliases.iter().any(|a| a.contains("gpt-5.6")));
        let e = match_entry(&t, "claude-haiku-4-5").expect("haiku");
        assert_eq!(Tier::parse(&e.tier), Tier::Other);
        assert_eq!(tier_of(&t, "claude-haiku-4-5"), Some(Tier::Other));
        assert!(match_entry(&t, "totally-unknown-xyz").is_none());
    }

    #[test]
    fn normalize_strips_dates_conservatively() {
        assert_eq!(
            normalize_id("Claude-Opus-5-20260801"),
            "claude-opus-5"
        );
        assert_eq!(normalize_id("openai/gpt-4.1"), "gpt-4.1");
        assert!(is_cheap_id("gemini-2.5-flash"));
        assert!(!is_cheap_id("claude-opus-5"));
    }
}
