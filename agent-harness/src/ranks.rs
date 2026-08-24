use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::Config;

const BUNDLED: &str = include_str!("../data/model_ranks.json");
/// 주 1회 갱신 (사용자 요구: 1주 1회 최신 모델 확인)
const STALE_SECS: u64 = 7 * 24 * 60 * 60;
/// 교차 검증용 제2 평가원 (LMArena 계열 Elo). 실패해도 조용히 건너뛴다.
const LMARENA_URLS: &[&str] = &[
    "https://lmarena.ai/api/v1/leaderboard/all",
    "https://lmarena.ai/api/v1/leaderboard/text",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankTable {
    pub updated: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub fetch_url: String,
    #[serde(default)]
    pub last_fetch_attempt: String,
    /// 실제로 점수에 반영된 평가기관 목록 (2곳 이상이면 교차 검증 완료)
    #[serde(default)]
    pub sources: Vec<String>,
    pub models: Vec<RankEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankEntry {
    pub id_aliases: Vec<String>,
    pub score: i32,
    pub tier: String,
    /// 이 점수에 합산된 평가기관 수 (2 이상 = 교차 검증)
    #[serde(default)]
    pub sources_count: i32,
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
        sources: Vec::new(),
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
    let cross = if t.sources.len() >= 2 {
        "교차검증"
    } else {
        "단일소스"
    };
    format!(
        "모델 순위 기준일: {}  ({} · {cross})",
        t.updated,
        if t.source.is_empty() {
            "번들"
        } else {
            t.source.split('(').next().unwrap_or(&t.source).trim()
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

/// 프로세스당 1회만. 네트워크 실패·지연이 본 작업을 절대 막지 않는다.
/// 주 1회(STALE_SECS) 기준으로 만료된 경우에만 백그라운드 갱신.
/// Tokio 런타임 밖(데스크탑 메인 스레드 등)에서 불려도 안전하다.
pub fn spawn_weekly_refresh() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if !is_stale() {
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                let _ = refresh(false).await; // 조용히 — 출력 없음
            });
        }
        Err(_) => {
            // 런타임이 없으면 전용 스레드에서 미니 런타임으로 실행.
            let _ = std::thread::Builder::new()
                .name("rafikx-ranks-refresh".into())
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build();
                    if let Ok(rt) = rt {
                        rt.block_on(async {
                            let _ = refresh(false).await;
                        });
                    }
                });
        }
    }
}

/// Artificial Analysis 지능/코딩 지수 (선택: AA_API_KEY 있으면 인증 요청).
async fn fetch_artificial_analysis(
    client: &reqwest::Client,
    url: &str,
) -> Option<Vec<(String, i32)>> {
    let mut req = client.get(url).header("accept", "application/json");
    if let Ok(key) = std::env::var("AA_API_KEY") {
        if !key.trim().is_empty() {
            req = req.header("x-api-key", key.trim());
        }
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| v.as_array())?;
    let mut out = Vec::new();
    for item in arr {
        let slug = ["slug", "name", "id", "model"]
            .iter()
            .find_map(|k| item.get(*k).and_then(|x| x.as_str()))
            .unwrap_or("");
        if slug.is_empty() {
            continue;
        }
        // 지능+코딩 지수 평균(코딩이 있으면), 없으면 지수만.
        let intel = item
            .pointer("/evaluations/artificial_analysis_intelligence_index")
            .or_else(|| item.get("score"))
            .and_then(|x| x.as_f64());
        let coding = item
            .pointer("/evaluations/artificial_analysis_coding_index")
            .and_then(|x| x.as_f64());
        let score = match (intel, coding) {
            (Some(a), Some(b)) => Some((0.5 * a + 0.5 * b).round() as i32),
            (Some(a), None) => Some(a.round() as i32),
            _ => None,
        };
        if let Some(s) = score {
            out.push((slug.to_string(), s.clamp(0, 100)));
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// 제2 평가원: LMArena 계열 Elo (비공식 엔드포인트, 실패 허용).
/// Elo 1000~1500 을 0~100 스케일로 선형 정규화한다.
async fn fetch_lmarena_elo(client: &reqwest::Client) -> Option<Vec<(String, i32)>> {
    for url in LMARENA_URLS {
        let resp = match client
            .get(*url)
            .header("accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !resp.status().is_success() {
            continue;
        }
        let text = match resp.text().await {
            Ok(t) => t,
            Err(_) => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let arr = v
            .get("data")
            .and_then(|d| d.as_array())
            .or_else(|| v.get("leaderboard").and_then(|d| d.as_array()))
            .or_else(|| v.as_array());
        let Some(arr) = arr else { continue };
        let mut out = Vec::new();
        for item in arr {
            let name = ["model_name", "modelName", "display_name", "name", "slug", "model"]
                .iter()
                .find_map(|k| item.get(*k).and_then(|x| x.as_str()))
                .or_else(|| item.pointer("/model/name").and_then(|x| x.as_str()))
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let elo = ["rating", "elo", "arena_score", "score"]
                .iter()
                .find_map(|k| item.get(*k).and_then(|x| x.as_f64()));
            let Some(elo) = elo else { continue };
            // Elo → 0~100 (1200점 ≈ 40, 1450점 ≈ 90)
            let norm = ((elo - 1000.0) / 5.0).round().clamp(0.0, 100.0) as i32;
            out.push((name, norm));
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    None
}

fn merge_scored(table: &mut RankTable, aa: &[(String, i32)], elo: &[(String, i32)]) -> bool {
    let mut changed = false;
    for e in table.models.iter_mut() {
        // 이 항목에 대응하는 원문 점수 수집
        let mut aa_hit: Option<i32> = None;
        for (slug, s) in aa {
            if e.id_aliases.iter().any(|a| ids_close(a, slug)) {
                aa_hit = Some(*s);
                break;
            }
        }
        let mut elo_hit: Option<i32> = None;
        for (name, s) in elo {
            if e.id_aliases.iter().any(|a| ids_close(a, name)) {
                elo_hit = Some(*s);
                break;
            }
        }
        // 교차 검증: 2곳 이상일 때만 가중 합산 적용, 1곳이면 그 값 유지(표기만).
        let (composite, count) = match (aa_hit, elo_hit) {
            (Some(a), Some(b)) => ((0.7 * a as f32 + 0.3 * b as f32).round() as i32, 2),
            (Some(a), None) => (a, 1),
            (None, Some(b)) => (b, 1),
            _ => continue,
        };
        if e.sources_count != count || e.score != composite {
            if e.score != composite {
                e.score = composite;
            }
            e.sources_count = count;
            changed = true;
        }
    }
    // top5 재계산: 교차 검증된 점수가 5개 이상일 때만 신뢰하고 다시 매긴다.
    let verified: Vec<(usize, i32)> = table
        .models
        .iter()
        .enumerate()
        .filter(|(_, e)| e.sources_count >= 2)
        .map(|(i, e)| (i, e.score))
        .collect();
    if verified.len() >= 5 {
        let mut sorted = verified.clone();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        let top: Vec<usize> = sorted.iter().take(5).map(|(i, _)| *i).collect();
        for (i, e) in table.models.iter_mut().enumerate() {
            if e.sources_count < 2 {
                continue;
            }
            let want = if top.contains(&i) { "top5" } else { "strong" };
            if e.tier != want {
                e.tier = want.to_string();
                changed = true;
            }
        }
    }
    changed
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

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let mut sources: Vec<String> = Vec::new();
    let aa = fetch_artificial_analysis(&client, &url).await.unwrap_or_default();
    if !aa.is_empty() {
        sources.push("Artificial Analysis".into());
    }
    let elo = fetch_lmarena_elo(&client).await.unwrap_or_default();
    if !elo.is_empty() {
        sources.push("LMArena Elo".into());
    }

    if sources.is_empty() {
        let _ = save(&table);
        return Ok("순위 갱신 실패 — 기존 표를 유지합니다 (오프라인 또는 소스 차단)".into());
    }

    if merge_scored(&mut table, &aa, &elo) {
        table.updated = today();
    }
    table.sources = sources.clone();
    table.source = format!(
        "{} 교차 검증{}",
        sources.join(" + "),
        if table.sources.len() >= 2 { "" } else { " (단일 소스)" }
    );
    save(&table)?;
    Ok(format!(
        "순위표 갱신: {} (기준일 {}, {})",
        table.source,
        table.updated,
        if table.sources.len() >= 2 {
            "2개 이상 평가기관 교차 검증"
        } else {
            "단일 소스 — 참고용"
        }
    ))
}

fn ids_close(alias: &str, slug: &str) -> bool {
    let a = normalize_id(alias);
    let s = normalize_id(slug);
    a == s || (a.len() >= 4 && (s.contains(&a) || a.contains(&s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(aliases: &[&str], score: i32) -> RankEntry {
        RankEntry {
            id_aliases: aliases.iter().map(|s| s.to_string()).collect(),
            score,
            tier: "other".into(),
            sources_count: 0,
        }
    }

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

    #[test]
    fn merge_requires_cross_verification_for_top5() {
        let mut t = RankTable {
            updated: "x".into(),
            source: String::new(),
            fetch_url: String::new(),
            last_fetch_attempt: String::new(),
            sources: Vec::new(),
            models: vec![
                entry(&["model-a"], 50),
                entry(&["model-b"], 50),
                entry(&["model-c"], 50),
                entry(&["model-d"], 50),
                entry(&["model-e"], 50),
                entry(&["model-f"], 50),
                entry(&["model-g"], 50),
            ],
        };
        let aa = vec![
            ("model-a".to_string(), 70),
            ("model-b".to_string(), 60),
            ("model-c".to_string(), 55),
            ("model-d".to_string(), 40),
            ("model-e".to_string(), 30),
            ("model-f".to_string(), 12),
        ];
        let elo = vec![
            ("Model-A".to_string(), 90), // 대소문자 다른 표기도 매칭
            ("model-b".to_string(), 50),
            ("model-c".to_string(), 45),
            ("model-d".to_string(), 60),
            ("model-e".to_string(), 20),
            ("model-f".to_string(), 8),
        ];
        assert!(merge_scored(&mut t, &aa, &elo));
        let a = &t.models[0];
        assert_eq!(a.sources_count, 2);
        // 0.7*70 + 0.3*90 = 76
        assert_eq!(a.score, 76);
        assert_eq!(a.tier, "top5");
        let d = &t.models[3];
        assert_eq!(d.score, 46); // 0.7*40+0.3*60
        assert_eq!(d.tier, "top5"); // 6개 검증 중 4위
        let f = &t.models[5];
        assert_eq!(f.score, 11); // 0.7*12+0.3*8
        assert_eq!(f.tier, "strong"); // 하위권 강등
        // 단일 소스만 있는 항목은 점수 유지 + 표기만
        let g = &t.models[6];
        assert_eq!(g.sources_count, 0);
        assert_eq!(g.tier, "other");
    }

    #[test]
    fn single_source_keeps_score_but_marks_count() {
        let mut t = RankTable {
            updated: "x".into(),
            source: String::new(),
            fetch_url: String::new(),
            last_fetch_attempt: String::new(),
            sources: Vec::new(),
            models: vec![entry(&["solo-model"], 42)],
        };
        let elo = vec![("solo-model".to_string(), 66)];
        assert!(merge_scored(&mut t, &[], &elo));
        assert_eq!(t.models[0].score, 66);
        assert_eq!(t.models[0].sources_count, 1);
    }
}
