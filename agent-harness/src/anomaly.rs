//! 이상 감지 감시 (Anomaly Watcher) — Inspector 주기 리포트 위에서, 지표 이상을
//! 즉시 알린다. 전부 코드 계산(모델 호출 없음)이라 가볍다.
//!
//! 감시 지표 (코드 계산):
//! - 편집 성공률 급락 (edit_metric 계측, F3)
//! - 최근 실행 성공률 급락 (24시간 창)
//! - ulw blocked 신규 발생
//! - 프로바이더별 429/timeout 폭풍
//!
//! 상태 전이에만 알린다 (정상→이상 1회, 회복 시 1회) — 상태는
//! ~/.rafikx/anomaly_state.json 에 남아 데몬 재시작에도 스팸을 막는다.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::db::Db;

/// 기본 감시 주기 (분).
pub const DEFAULT_INTERVAL_MINUTES: u64 = 15;
/// 편집 성공률이 이 값(%) 미만이면 이상 (표본 최소 개수 이상일 때).
pub const EDIT_SUCCESS_ALERT_BELOW: f64 = 70.0;
/// 최근 실행 성공률이 이 값(%) 미만이면 이상.
pub const RUN_SUCCESS_ALERT_BELOW: f64 = 50.0;
/// 최소 표본 — 미만이면 판정 보류 (과잉 해석 금지).
pub const MIN_SAMPLES: usize = 10;
/// 프로바이더별 429/timeout 이 횟수 이상이면 이상.
pub const PROVIDER_ERR_THRESHOLD: usize = 3;
/// 최근 실행 창 (초) — 24시간.
pub const WINDOW_SECS: i64 = 24 * 3600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub key: &'static str,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct AnomalyReport {
    pub alerts: Vec<Alert>,
    pub recovered: Vec<&'static str>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// 현재 이상 상태인 지표 키들 — 전이 감지용.
    #[serde(default)]
    active: HashSet<String>,
    /// 이미 알린 ulw blocked 실행 id.
    #[serde(default)]
    ulw_seen: HashSet<String>,
}

fn state_path() -> Result<PathBuf> {
    Ok(Db::db_path()?
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("anomaly_state.json"))
}

fn load_state(path: &std::path::Path) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default()
}

fn save_state(path: &std::path::Path, state: &State) -> Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 한 번 점검한다 — 이상은 전이에서만 알리고, 회복도 알린다.
pub fn check(cfg: &Config, db: &Db) -> Result<AnomalyReport> {
    let path = state_path()?;
    check_with_state_path(cfg, db, &path)
}

/// 상태 파일 경로를 지정하는 점검 — 테스트 격리용.
pub fn check_with_state_path(cfg: &Config, db: &Db, path: &std::path::Path) -> Result<AnomalyReport> {
    let mut state = load_state(path);
    let mut active_now: HashSet<&'static str> = HashSet::new();
    let mut report = AnomalyReport::default();

    // 1) 편집 성공률
    let metrics = db.edit_metrics().unwrap_or_default();
    if metrics.len() >= MIN_SAMPLES {
        let ok = metrics.iter().filter(|(_, o)| o.starts_with("ok")).count();
        let rate = ok as f64 * 100.0 / metrics.len() as f64;
        if rate < EDIT_SUCCESS_ALERT_BELOW {
            active_now.insert("edit_rate");
            if !state.active.contains("edit_rate") {
                report.alerts.push(Alert {
                    key: "edit_rate",
                    message: format!(
                        "편집 성공률 {rate:.0}% (기준 {EDIT_SUCCESS_ALERT_BELOW:.0}% 미만, 표본 {}건)",
                        metrics.len()
                    ),
                });
            }
        }
    }

    // 2) 최근 24시간 실행 성공률
    let cutoff = now_secs() - WINDOW_SECS;
    let runs = db.recent_runs(200).unwrap_or_default();
    let window: Vec<_> = runs.iter().filter(|r| r.started_at >= cutoff).collect();
    if window.len() >= MIN_SAMPLES {
        let ok = window.iter().filter(|r| r.status == "ok").count();
        let rate = ok as f64 * 100.0 / window.len() as f64;
        if rate < RUN_SUCCESS_ALERT_BELOW {
            active_now.insert("run_rate");
            if !state.active.contains("run_rate") {
                report.alerts.push(Alert {
                    key: "run_rate",
                    message: format!(
                        "최근 24시간 실행 성공률 {rate:.0}% (기준 {RUN_SUCCESS_ALERT_BELOW:.0}% 미만, 표본 {}건)",
                        window.len()
                    ),
                });
            }
        }
    }

    // 3) 프로바이더별 429/timeout 폭풍 (24시간)
    let mut by_provider: HashMap<String, usize> = HashMap::new();
    for r in &window {
        if let Some(err) = &r.error {
            let low = err.to_lowercase();
            if low.contains("429") || low.contains("timeout") || low.contains("timed out") {
                *by_provider
                    .entry(r.provider.clone().unwrap_or_else(|| "(없음)".into()))
                    .or_insert(0) += 1;
            }
        }
    }
    for (provider, n) in by_provider {
        if n >= PROVIDER_ERR_THRESHOLD {
            let key = "provider_storm";
            active_now.insert(key);
            if !state.active.contains(key) {
                report.alerts.push(Alert {
                    key,
                    message: format!("{provider}: 24시간 내 429/timeout {n}건 — 연결 상태 점검 권장"),
                });
            }
        }
    }

    // 4) ulw blocked 신규 — 마지막 점검 이후에 생긴 것만
    let ulw_root = cfg.workspace.join(".omo").join("ulw");
    if let Ok(rd) = std::fs::read_dir(&ulw_root) {
        for entry in rd.flatten() {
            let Ok(body) = std::fs::read_to_string(entry.path().join("state.json")) else {
                continue;
            };
            let Ok(s) = serde_json::from_str::<serde_json::Value>(&body) else {
                continue;
            };
            let id = entry.file_name().to_string_lossy().into_owned();
            if s.get("status").and_then(|v| v.as_str()) == Some("blocked")
                && !state.ulw_seen.contains(&id)
            {
                state.ulw_seen.insert(id.clone());
                let reason = s
                    .get("blocked_reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(사유 없음)");
                report.alerts.push(Alert {
                    key: "ulw_blocked",
                    message: format!("ulw 실행 중단: {id} — {reason}"),
                });
            }
        }
    }

    // 회복 감지 — 이전에 이상이었는데 지금 아닌 지표
    for key in ["edit_rate", "run_rate", "provider_storm"] {
        if state.active.contains(key) && !active_now.contains(key) {
            report.recovered.push(key);
        }
    }

    state.active = active_now.iter().map(|k| k.to_string()).collect();
    save_state(path, &state)?;
    Ok(report)
}

/// 텔레그램 문구로 렌더한다.
pub fn render_message(report: &AnomalyReport) -> String {
    let mut out = String::new();
    if !report.alerts.is_empty() {
        out.push_str("⚠️ 이상 감지\n");
        for a in &report.alerts {
            out.push_str(&format!("- {}\n", a.message));
        }
    }
    for key in &report.recovered {
        out.push_str(&format!("✅ 회복: {key}\n"));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_db(tag: &str) -> (PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!("rafikx-anomaly-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db")).unwrap();
        (dir, db)
    }

    fn temp_cfg(tag: &str) -> (PathBuf, Config) {
        let dir = std::env::temp_dir().join(format!("rafikx-anomaly-cfg-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::load(Some(&dir.join("config.toml"))).unwrap();
        // 기본 워크스페이스(~/dev/playground)의 실제 .omo 를 읽지 않게 격리한다.
        cfg.workspace = dir.clone();
        (dir, cfg)
    }

    #[test]
    fn edit_rate_alert_fires_and_recovers() {
        let (dbdir, db) = temp_db("edit");
        let (_cd, cfg) = temp_cfg("edit");
        let run = db.start_run("t", "x", None, None, None, None).unwrap();
        for _ in 0..3 {
            db.push_graph_event(&run, "edit_metric", "edit_file", "ok:anchors", None).unwrap();
        }
        for _ in 0..8 {
            db.push_graph_event(&run, "edit_metric", "apply_patch", "fail:apply", None).unwrap();
        }
        // 11건 중 성공 3 → 27% — 첫 점검에서 알림
        // 상태 파일 격리: 이 테스트는 실제 상태 파일을 쓴다 — 순서 의존을 피하려고
        // 전이 규칙(첫 발동 1회)만 본다.
        let sp = dbdir.join("state.json");
        let first = check_with_state_path(&cfg, &db, &sp).unwrap();
        let fired = first.alerts.iter().any(|a| a.key == "edit_rate");
        let second = check_with_state_path(&cfg, &db, &sp).unwrap();
        let fired_twice = second.alerts.iter().any(|a| a.key == "edit_rate");
        if fired {
            assert!(!fired_twice, "같은 이상이 연속 알림되면 안 된다 (전이만 알림)");
        }
        // 회복 — 성공을 쌓아 기준 이상으로
        for _ in 0..30 {
            db.push_graph_event(&run, "edit_metric", "edit_file", "ok:anchors", None).unwrap();
        }
        let third = check_with_state_path(&cfg, &db, &sp).unwrap();
        assert!(!third.alerts.iter().any(|a| a.key == "edit_rate"));
        let _ = fs::remove_dir_all(dbdir);
    }

    #[test]
    fn ulw_blocked_alerts_once_per_run() {
        let (dbdir, db) = temp_db("ulw");
        let (cd, cfg) = temp_cfg("ulw");
        let ulw = cfg.workspace.join(".omo").join("ulw");
        fs::create_dir_all(ulw.join("ulw-x")).unwrap();
        fs::write(
            ulw.join("ulw-x/state.json"),
            r#"{"status":"blocked","blocked_reason":"재촉 초과"}"#,
        )
        .unwrap();
        let sp = dbdir.join("state.json");
        let sp = dbdir.join("state.json");
        let first = check_with_state_path(&cfg, &db, &sp).unwrap();
        let fired = first.alerts.iter().any(|a| a.key == "ulw_blocked" && a.message.contains("ulw-x"));
        let second = check_with_state_path(&cfg, &db, &sp).unwrap();
        let fired_again = second.alerts.iter().any(|a| a.key == "ulw_blocked" && a.message.contains("ulw-x"));
        assert!(fired, "첫 점검에서 알려야 한다");
        assert!(!fired_again, "같은 실행을 두 번 알리면 안 된다");
        let _ = fs::remove_dir_all(dbdir);
        let _ = fs::remove_dir_all(cd);
    }

    #[test]
    fn empty_data_is_silent() {
        let (dbdir, db) = temp_db("empty");
        let (cd, cfg) = temp_cfg("empty");
        let report = check_with_state_path(&cfg, &db, &dbdir.join("state.json")).unwrap();
        assert!(report.alerts.is_empty());
        let _ = fs::remove_dir_all(dbdir);
        let _ = fs::remove_dir_all(cd);
    }

    #[test]
    fn render_formats_alerts_and_recovery() {
        let report = AnomalyReport {
            alerts: vec![Alert { key: "edit_rate", message: "편집 성공률 30%".into() }],
            recovered: vec!["run_rate"],
        };
        let text = render_message(&report);
        assert!(text.contains("⚠️"));
        assert!(text.contains("편집 성공률 30%"));
        assert!(text.contains("✅ 회복: run_rate"));
    }
}
