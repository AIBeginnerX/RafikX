use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::applog;
use crate::config::Config;
use crate::db::{self, Db, RunRow};
use crate::harness;
use crate::lessons;
use crate::provider::{ChatRequest, ContentBlock, Message};

const ANALYZE_PROMPT: &str = "\
당신은 RafikX Inspector다. 아래 통계는 코드가 계산한 사실이다. 숫자를 다시 계산하지 마라.\n\
도구를 쓰지 마라. 파일을 수정하거나 명령을 제안 실행하지 마라.\n\
마크다운만 출력하라. 섹션:\n\
## 기간 요약\n\
## 건강 신호등\n\
## 반복 실패 패턴 Top5\n\
## 제안 교훈\n\
(각 항목은 명령형 1문장. `- ` 목록)\n\
## 제안 설정 변경\n\
(진단과 사유만. 자동 적용 금지)\n\
## 사용자 액션 아이템\n\
데이터가 10건 미만이면 과잉 해석하지 말고 데이터 부족을 명시하라.\n\
API 키·토큰·파일 전문을 절대 쓰지 마라.\n";

pub async fn cmd_inspect(
    cfg: &Config,
    last: u32,
    apply: bool,
    subagent: Option<&str>,
) -> Result<()> {
    let (_summary, body) = generate_report(cfg, last, subagent).await?;
    println!("{body}");
    if apply {
        let db = Db::open(&Db::db_path()?)?;
        apply_lessons(cfg, &db, &body)?;
    }
    Ok(())
}

/// 점검 리포트를 만들고 `(요약, 본문)`을 반환한다. CLI 없이 스케줄러가 호출한다.
pub async fn generate_report(
    cfg: &Config,
    last: u32,
    subagent: Option<&str>,
) -> Result<(String, String)> {
    let n = if last == 0 { 200 } else { last as usize };
    build_report(cfg, n, subagent).await
}

pub fn cmd_report_last() -> Result<()> {
    let db = Db::open(&Db::db_path()?)?;
    let Some(row) = db.last_report()? else {
        println!("저장된 리포트가 없습니다. 먼저 inspect 를 실행하세요.");
        return Ok(());
    };
    println!("리포트 {} ({})\n", row.id, row.body_path);
    match fs::read_to_string(&row.body_path) {
        Ok(body) => println!("{body}"),
        Err(_) => println!("{}", row.summary),
    }
    Ok(())
}

async fn build_report(
    cfg: &Config,
    last: usize,
    subagent_override: Option<&str>,
) -> Result<(String, String)> {
    let (stats_md, call_model) = {
        let db = Db::open(&Db::db_path()?)?;
        let runs = db.recent_runs(last)?;
        let call_model = match db.last_report() {
            Ok(Some(r)) => runs.iter().any(|x| x.started_at > r.created_at),
            _ => !runs.is_empty(),
        };
        let stats = compute_stats(&runs);
        let lessons = db.list_lessons().unwrap_or_default();
        let log_tail = error_log_tail(80);
        let doctor = doctor_snapshot(cfg);
        (
            format_stats(&stats, runs.len(), lessons.len(), &log_tail, &doctor),
            call_model,
        )
    };

    let analysis = if !call_model {
        String::from("## 분석\n새 작업이 없어 모델 호출을 건너뛰었습니다. 위 통계만 참고하세요.\n")
    } else {
        match call_inspector(cfg, subagent_override, &stats_md).await {
            Ok(text) if !text.trim().is_empty() => redact(&text),
            Ok(_) => {
                String::from("## 분석\n모델이 빈 응답을 반환했습니다. 위 통계만 참고하세요.\n")
            }
            Err(e) => format!(
                "## 분석\n모델 호출을 건너뛰었습니다 ({e}). 위 통계는 코드가 계산한 사실입니다.\n"
            ),
        }
    };

    let body = format!("{stats_md}\n{analysis}\n");
    let id = Db::new_id();
    let path = save_markdown(&id, &body)?;
    let summary = summarize(&body);
    {
        let db = Db::open(&Db::db_path()?)?;
        db.save_report(&id, &summary, &path.to_string_lossy())?;
    }
    println!("리포트 저장: {}", path.display());
    Ok((summary, body))
}

struct Stats {
    total: usize,
    ok: usize,
    fail: usize,
    denied: usize,
    limit: usize,
    by_class: HashMap<String, usize>,
    by_provider_err: HashMap<String, usize>,
    avg_iter: f64,
    tokens_in: i64,
    tokens_out: i64,
    top_errors: Vec<(String, usize)>,
}

fn compute_stats(runs: &[RunRow]) -> Stats {
    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut denied = 0usize;
    let mut limit = 0usize;
    let mut by_class: HashMap<String, usize> = HashMap::new();
    let mut by_provider_err: HashMap<String, usize> = HashMap::new();
    let mut err_counts: HashMap<String, usize> = HashMap::new();
    let mut iter_sum = 0i64;
    let mut tokens_in = 0i64;
    let mut tokens_out = 0i64;
    for r in runs {
        match r.status.as_str() {
            "ok" => ok += 1,
            "denied" => denied += 1,
            "limit" => limit += 1,
            _ => fail += 1,
        }
        let class = r.class.clone().unwrap_or_else(|| "(없음)".into());
        *by_class.entry(class).or_insert(0) += 1;
        iter_sum += r.iterations;
        tokens_in += r.input_tokens;
        tokens_out += r.output_tokens;
        if let Some(err) = &r.error {
            let key = normalize_error(err);
            *err_counts.entry(key).or_insert(0) += 1;
            let p = r.provider.clone().unwrap_or_else(|| "(없음)".into());
            let low = err.to_lowercase();
            if low.contains("429") || low.contains("timeout") || low.contains("timed out") {
                *by_provider_err.entry(p).or_insert(0) += 1;
            }
        }
    }
    let mut top_errors: Vec<(String, usize)> = err_counts.into_iter().collect();
    top_errors.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    top_errors.truncate(5);
    let n = runs.len() as f64;
    Stats {
        total: runs.len(),
        ok,
        fail,
        denied,
        limit,
        by_class,
        by_provider_err,
        avg_iter: if n == 0.0 { 0.0 } else { iter_sum as f64 / n },
        tokens_in,
        tokens_out,
        top_errors,
    }
}

fn format_stats(s: &Stats, n: usize, lesson_n: usize, log_tail: &str, doctor: &str) -> String {
    let rate = if s.total == 0 {
        0.0
    } else {
        (s.ok as f64) * 100.0 / s.total as f64
    };
    let light = if s.total < 10 {
        "데이터 부족"
    } else if rate >= 80.0 {
        "🟢"
    } else if rate >= 50.0 {
        "🟡"
    } else {
        "🔴"
    };
    let mut class_s = String::new();
    let mut keys: Vec<_> = s.by_class.keys().cloned().collect();
    keys.sort();
    for k in keys {
        class_s.push_str(&format!("- {k}: {}\n", s.by_class[&k]));
    }
    let mut err_s = String::new();
    for (e, c) in &s.top_errors {
        err_s.push_str(&format!("- ({c}) {e}\n"));
    }
    if err_s.is_empty() {
        err_s = "- (없음)\n".into();
    }
    let mut prov = String::new();
    for (p, c) in &s.by_provider_err {
        prov.push_str(&format!("- {p}: {c}\n"));
    }
    if prov.is_empty() {
        prov = "- (없음)\n".into();
    }
    format!(
        "# Inspector 통계 (코드 계산, 모델 산수 아님)\n\n\
         표본: {n}건 / lessons {lesson_n}건 / 신호등 {light}\n\
         성공 {ok} / 실패 {fail} / 거부 {denied} / 상한 {limit} / 성공률 {rate:.1}%\n\
         평균 반복 {avg:.2} / 토큰 in={tin} out={tout}\n\n\
         ## 분류별 건수\n{class_s}\n\
         ## 프로바이더 429/timeout\n{prov}\n\
         ## 최다 오류 Top5\n{err_s}\n\
         ## doctor 스냅샷\n{doctor}\n\
         ## agent.log 오류 꼬리\n```\n{log_tail}\n```\n",
        ok = s.ok,
        fail = s.fail,
        denied = s.denied,
        limit = s.limit,
        avg = s.avg_iter,
        tin = s.tokens_in,
        tout = s.tokens_out,
    )
}

fn doctor_snapshot(cfg: &Config) -> String {
    let mut lines = Vec::new();
    lines.push(format!("workspace: {}", cfg.workspace.display()));
    let mut names: Vec<_> = cfg.file.providers.keys().cloned().collect();
    names.sort();
    for name in names {
        let connected = crate::auth::resolve_credential(cfg, &name)
            .ok()
            .flatten()
            .is_some();
        let mode = cfg
            .provider(&name)
            .map(|p| crate::auth::auth_mode(&name, p))
            .unwrap_or("?");
        lines.push(format!(
            "- {name} ({mode}): {}",
            if connected || mode == "none" {
                "연결/불필요"
            } else {
                "미연결"
            }
        ));
    }
    lines.join("\n")
}

fn error_log_tail(max_lines: usize) -> String {
    let Ok(path) = applog::log_file_path() else {
        return "(로그 없음)".into();
    };
    let Ok(file) = fs::File::open(path) else {
        return "(로그 없음)".into();
    };
    let lines: Vec<String> = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| l.contains("\"error\"") || l.to_lowercase().contains("error"))
        .map(|l| redact(&l))
        .collect();
    if lines.is_empty() {
        return "(오류 라인 없음)".into();
    }
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

async fn call_inspector(
    cfg: &Config,
    subagent_override: Option<&str>,
    stats_md: &str,
) -> Result<String> {
    let profile = subagent_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| cfg.file.inspector.subagent.clone());
    let sub = cfg
        .file
        .subagents
        .get(&profile)
        .ok_or_else(|| anyhow!("점검 프로파일 '{profile}' 이(가) 없습니다"))?;
    let order = harness::fallback_order(cfg, &sub.provider, None);
    let req = ChatRequest {
        model: String::new(),
        system: ANALYZE_PROMPT.into(),
        messages: vec![Message::user_text(stats_md)],
        tools: vec![],
        max_tokens: 2048,
        stream: false,
    };
    let (_name, resp) = harness::chat_with_fallback(cfg, &order, &sub.model_role, req).await?;
    let mut text = String::new();
    for b in resp.content {
        if let ContentBlock::Text { text: t } = b {
            text.push_str(&t);
        }
    }
    let _ = sub.tools; // 도구는 요청에서 강제 제거됨
    Ok(text)
}

fn save_markdown(id: &str, body: &str) -> Result<PathBuf> {
    let dir = crate::config::Config::data_dir()?.join("reports");
    fs::create_dir_all(&dir)?;
    let name = format!("{}.md", utc_stamp());
    let path = dir.join(name);
    fs::write(&path, body)?;
    let _ = id;
    Ok(path)
}

fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d, hh, mm) = unix_to_utc(secs);
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}")
}

fn unix_to_utc(secs: u64) -> (i32, u32, u32, u32, u32) {
    let mins_all = secs / 60;
    let mm = (mins_all % 60) as u32;
    let hours_all = mins_all / 60;
    let hh = (hours_all % 24) as u32;
    let mut days = (hours_all / 24) as i64;
    let mut y = 1970i32;
    loop {
        let len = if is_leap(y) { 366 } else { 365 };
        if days < len {
            break;
        }
        days -= len;
        y += 1;
        if y > 3000 {
            break;
        }
    }
    let md = [
        31,
        if is_leap(y) { 29 } else { 28 },
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
    for dim in md {
        if days < dim as i64 {
            break;
        }
        days -= dim as i64;
        month += 1;
    }
    (y, month, days as u32 + 1, hh, mm)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn summarize(body: &str) -> String {
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .take(10)
        .collect::<Vec<_>>()
        .join("\n")
}

fn apply_lessons(cfg: &Config, db: &Db, body: &str) -> Result<()> {
    let items = parse_proposed_lessons(body);
    if items.is_empty() {
        println!("리포트에서 제안 교훈을 찾지 못했습니다.");
        return Ok(());
    }
    let max = cfg.file.memory.max_lessons;
    for lesson in items {
        let keywords = lessons::keywords_from_text(&lesson);
        match db.add_lesson("manual", &keywords, &lesson, max)? {
            db::LessonWrite::Inserted { id } => println!("교훈 저장 id={id}: {lesson}"),
            db::LessonWrite::Bumped { id } => println!("교훈 가중치 증가 id={id}: {lesson}"),
        }
    }
    Ok(())
}

pub fn parse_proposed_lessons(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with('#') && t.contains("제안 교훈") {
            in_section = true;
            continue;
        }
        if in_section && t.starts_with('#') {
            break;
        }
        if in_section {
            let item = t
                .trim_start_matches('-')
                .trim_start_matches('*')
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.')
                .trim();
            if !item.is_empty() && item.chars().count() < 200 {
                out.push(item.to_string());
            }
        }
    }
    out
}

fn normalize_error(err: &str) -> String {
    redact(err).chars().take(80).collect()
}

fn redact(s: &str) -> String {
    let mut out = s.to_string();
    for prefix in ["sk-ant-", "sk-or-", "sk-", "xai-", "gsk_", "Bearer "] {
        while let Some(i) = out.find(prefix) {
            let rest = &out[i + prefix.len()..];
            let n = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(rest.len());
            out.replace_range(i..i + prefix.len() + n, "[redacted]");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proposed_lessons() {
        let md = "## 반복 실패\n- skip\n## 제안 교훈\n- edit 전에 read_file 한다\n- jail 경로를 확인한다\n## 제안 설정 변경\n- 무시";
        let v = parse_proposed_lessons(md);
        assert_eq!(v.len(), 2);
        assert!(v[0].contains("read_file"));
    }

    #[test]
    fn inspector_request_has_no_tools() {
        let req = ChatRequest {
            model: "x".into(),
            system: ANALYZE_PROMPT.into(),
            messages: vec![],
            tools: vec![],
            max_tokens: 8,
            stream: false,
        };
        assert!(req.tools.is_empty());
    }
}
