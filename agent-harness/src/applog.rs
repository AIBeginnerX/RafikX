use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

fn logs_dir() -> Result<PathBuf> {
    let dir = crate::config::Config::data_dir()?.join("logs");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 운영 로그 — 서비스 기동·턴 시작/종료·오류 등 운영 관점 기록.
pub fn log_file_path() -> Result<PathBuf> {
    Ok(logs_dir()?.join("ops.log"))
}

/// 데이터(디버그) 로그 — 폴백 시도·바인딩 결정·도구 오류 등 상세 진단 기록.
pub fn debug_file_path() -> Result<PathBuf> {
    Ok(logs_dir()?.join("debug.log"))
}

fn append(path: PathBuf, line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

fn json_line(level: &str, msg: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    serde_json::json!({"level": level, "ts": ts, "msg": msg}).to_string()
}

/// 운영 로그에 info 수준 기록.
pub fn info(msg: &str) {
    if let Ok(p) = log_file_path() {
        append(p, &json_line("info", msg));
    }
}

#[allow(dead_code)]
pub fn warn(msg: &str) {
    if let Ok(p) = log_file_path() {
        append(p, &json_line("warn", msg));
    }
}

/// 운영 로그와 디버그 로그 양쪽에 오류 기록.
pub fn error(msg: &str) {
    let l = json_line("error", msg);
    if let Ok(p) = log_file_path() {
        append(p, &l);
    }
    debug(msg);
}

/// 디버그(데이터) 로그 전용 — 화면·운영 로그에 노출하지 않는 상세 진단.
pub fn debug(msg: &str) {
    if let Ok(p) = debug_file_path() {
        append(p, &json_line("debug", msg));
    }
}
