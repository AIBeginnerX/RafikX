use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

pub fn log_file_path() -> Result<PathBuf> {
    let dir = crate::config::Config::data_dir()?.join("logs");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("agent.log"))
}

pub fn write(level: &str, msg: &str) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = serde_json::json!({
        "level": level,
        "ts": ts,
        "msg": msg,
    });
    if let Ok(path) = log_file_path() {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}

pub fn info(msg: &str) {
    write("info", msg);
}

#[allow(dead_code)]
pub fn warn(msg: &str) {
    write("warn", msg);
}

pub fn error(msg: &str) {
    write("error", msg);
}
