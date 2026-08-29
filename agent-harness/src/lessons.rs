use anyhow::Result;

use crate::applog;
use crate::config::Config;
use crate::db::{self, Db, LessonRow};
use crate::harness;
use crate::provider::{ChatRequest, ContentBlock, Message};

pub fn inject_block(db: &Db, task: &str, limit_chars: usize) -> String {
    if limit_chars == 0 {
        return String::new();
    }
    let Ok(rows) = db.lessons_for_inject(task, 5, 2) else {
        return String::new();
    };
    assemble_block(&rows, limit_chars)
}

pub fn inject_block_for_project(
    db: &Db,
    workspace: &std::path::Path,
    task: &str,
    limit_chars: usize,
) -> String {
    if limit_chars == 0 {
        return String::new();
    }
    let Ok(rows) = db.project_lessons_for_inject(workspace, task, 5, 2) else {
        return String::new();
    };
    assemble_block(&rows, limit_chars)
}

pub fn assemble_block(rows: &[LessonRow], limit_chars: usize) -> String {
    if rows.is_empty() || limit_chars == 0 {
        return String::new();
    }
    let mut body = String::from("[과거 교훈 — 같은 실수를 반복하지 말 것]\n");
    for row in rows {
        let line = format!("- (w{}) {}\n", row.weight, row.lesson.trim());
        if body.chars().count() + line.chars().count() > limit_chars {
            break;
        }
        body.push_str(&line);
    }
    if body.lines().count() <= 1 {
        return String::new();
    }
    body
}

pub fn keywords_from_text(text: &str) -> String {
    text.split_whitespace()
        .filter(|w| {
            w.chars()
                .any(|c| c.is_alphanumeric() || ('가'..='힣').contains(&c))
        })
        .take(6)
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn parse_reflection_json(text: &str) -> Option<(String, String)> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let v: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let keywords = match v.get("keywords") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    let lesson = v.get("lesson")?.as_str()?.trim().to_string();
    if lesson.is_empty() {
        return None;
    }
    let keywords = if keywords.trim().is_empty() {
        keywords_from_text(&lesson)
    } else {
        keywords
    };
    Some((keywords, lesson))
}

pub fn cmd_list(cfg: &Config) -> Result<()> {
    let db = Db::open(&Db::db_path()?)?;
    let rows = db.list_project_lessons(&cfg.workspace)?;
    if rows.is_empty() {
        println!("저장된 교훈이 없습니다.");
        return Ok(());
    }
    println!("id  w  trigger      lesson");
    for r in rows {
        println!("{:<3} {:<2} {:<12} {}", r.id, r.weight, r.trigger, r.lesson);
    }
    Ok(())
}

pub fn add_text(cfg: &Config, text: &str) -> Result<String> {
    let lesson = text.trim();
    if lesson.is_empty() {
        anyhow::bail!("교훈 문장이 비어 있습니다");
    }
    let db = Db::open(&Db::db_path()?)?;
    let keywords = keywords_from_text(lesson);
    let max = cfg.file.memory.max_lessons;
    let msg = match db.add_project_lesson(&cfg.workspace, "manual", &keywords, lesson, max)? {
        db::LessonWrite::Inserted { id } => format!("교훈을 저장했습니다 (id={id})"),
        db::LessonWrite::Bumped { id } => {
            format!("비슷한 교훈이 있어 가중치만 올렸습니다 (id={id})")
        }
    };
    println!("{msg}");
    Ok(msg)
}

pub fn cmd_add(cfg: &Config, text: &str) -> Result<()> {
    add_text(cfg, text)?;
    Ok(())
}

pub fn cmd_rm(id: i64) -> Result<()> {
    let db = Db::open(&Db::db_path()?)?;
    if db.delete_lesson(id)? {
        println!("교훈 {id} 를 삭제했습니다.");
    } else {
        println!("교훈 {id} 를 찾지 못했습니다.");
    }
    Ok(())
}

pub fn cmd_clear() -> Result<()> {
    let db = Db::open(&Db::db_path()?)?;
    let n = db.clear_lessons()?;
    println!("교훈 {n}건을 모두 삭제했습니다.");
    Ok(())
}

/// 트리거가 있으면 리플렉션을 백그라운드로 돌린다. 메인 흐름을 막지 않는다.
pub fn maybe_spawn(cfg: &Config, task: &str, outcome: &crate::agent::AgentOutcome) {
    if !cfg.file.memory.enabled {
        return;
    }
    let (trigger, detail) = match detect_trigger(outcome) {
        Some(v) => v,
        None => return,
    };
    let cfg = cfg.clone();
    let task: String = task.chars().take(500).collect();
    let detail: String = detail.chars().take(500).collect();
    tokio::spawn(async move {
        if let Err(e) = reflect_and_save(&cfg, &task, &trigger, &detail).await {
            applog::error(&format!("reflection skip: {e:#}"));
        }
    });
}

fn detect_trigger(outcome: &crate::agent::AgentOutcome) -> Option<(String, String)> {
    if !outcome.deny_reasons.is_empty() {
        return Some(("user_deny".into(), outcome.deny_reasons.join("; ")));
    }
    if !outcome.tool_errors.is_empty() {
        return Some(("tool_error".into(), outcome.tool_errors.join("; ")));
    }
    if let Some(v) = &outcome.verify_fail {
        return Some(("verify_fail".into(), v.clone()));
    }
    // 검증이 한 번 깨졌다가 재시도로 통과한 실행 — 결과는 성공이지만 그 실패에서
    // 배울 것은 남아 있다. verify_fail 이 최종 실패만 담게 된 뒤에도(§15.4)
    // 회복 교훈 수집이 끊기지 않도록 여기서 이어받는다.
    if let Some(v) = &outcome.verify_recovered {
        return Some(("verify_recovered".into(), v.clone()));
    }
    if outcome.status == "fail" || outcome.status == "limit" {
        let d = outcome
            .error
            .clone()
            .unwrap_or_else(|| outcome.status.clone());
        return Some(("run_fail".into(), d));
    }
    None
}

async fn reflect_and_save(cfg: &Config, task: &str, trigger: &str, detail: &str) -> Result<()> {
    let default = cfg.file.general.default_provider.clone();
    let order = harness::fallback_order(cfg, &default, None);
    let req = ChatRequest {
        model: String::new(),
        system: "너는 실수 기록가다. 아래 오류와 맥락에서 다음에 지킬 교훈을 딱 1개,\nJSON {\"keywords\":\"공백구분 키워드 3~6개\",\"lesson\":\"명령형 1~2문장\"} 형식으로만 출력하라.".into(),
        messages: vec![Message::user_text(format!(
            "[작업 요약]\n{task}\n\n[오류/사유]\n{detail}"
        ))],
        tools: vec![],
        max_tokens: 256,
        stream: false,
    };
    // 백그라운드 반성 실패는 사용자 화면에 오류로 보이면 안 된다 — 로그로만.
    harness::set_fallback_quiet(true);
    let call = harness::chat_with_fallback(cfg, &order, "small", req.clone()).await;
    harness::set_fallback_quiet(false);
    let (_name, resp) = call?;
    let text = resp
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("");
    // G13 — 구조화 출력 파싱 실패 시 오류를 첨부해 1회 재요청한다.
    let (keywords, lesson) = match parse_reflection_json(text) {
        Some(ok) => ok,
        None => {
            let retry_req = ChatRequest {
                max_tokens: 256,
                messages: vec![
                    Message::user_text(format!(
                        "[작업 요약]\n{task}\n\n[오류/사유]\n{detail}"
                    )),
                    Message::user_text(format!(
                        "네 출력은 JSON 형식이 아니었다: {text}\n형식 오류를 고쳐 같은 스키마로 다시만 출력하라."
                    )),
                ],
                ..req.clone()
            };
            let retry = harness::chat_with_fallback(cfg, &order, "small", retry_req).await;
            let retry_text = retry
                .ok()
                .and_then(|(_, resp)| {
                    resp.content.iter().find_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                })
                .unwrap_or_default();
            match parse_reflection_json(&retry_text) {
                Some(ok) => ok,
                None => return Ok(()),
            }
        }
    };
    let db = Db::open(&Db::db_path()?)?;
    let _ = db.add_project_lesson(
        &cfg.workspace,
        trigger,
        &keywords,
        &lesson,
        cfg.file.memory.max_lessons,
    )?;
    applog::info(&format!("lesson saved trigger={trigger} {lesson}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::LessonRow;

    #[test]
    fn verify_recovery_still_triggers_a_lesson() {
        // verify_fail 이 최종 실패만 담게 된 뒤에도(§15.4) 회복 사례의 교훈은 남아야 한다.
        let recovered = crate::agent::AgentOutcome {
            verify_recovered: Some("cargo check 실패".into()),
            ..Default::default()
        };
        let (trigger, detail) = detect_trigger(&recovered).expect("회복 트리거");
        assert_eq!(trigger, "verify_recovered");
        assert_eq!(detail, "cargo check 실패");

        // 최종 실패가 있으면 그쪽이 우선한다 (더 구체적인 원인).
        let failed = crate::agent::AgentOutcome {
            verify_fail: Some("cargo test 실패".into()),
            verify_recovered: Some("cargo check 실패".into()),
            ..Default::default()
        };
        assert_eq!(detect_trigger(&failed).unwrap().0, "verify_fail");

        // 아무 흔적도 없는 성공은 트리거가 없다.
        assert!(detect_trigger(&crate::agent::AgentOutcome::default()).is_none());
    }

    #[test]
    fn parse_json_object() {
        let (k, l) = parse_reflection_json(
            "blah {\"keywords\":\"read_file edit\",\"lesson\":\"수정 전 원문을 읽는다.\"} tail",
        )
        .unwrap();
        assert!(k.contains("read_file"));
        assert!(l.contains("원문"));
    }

    #[test]
    fn inject_respects_char_limit() {
        let rows = vec![LessonRow {
            id: 1,
            created_at: 0,
            last_hit: 0,
            trigger: "manual".into(),
            keywords: "a".into(),
            lesson: "아주 긴 교훈을 반복한다.".repeat(20),
            weight: 3,
        }];
        let block = assemble_block(&rows, 40);
        assert!(block.chars().count() <= 40);
        let empty = assemble_block(&rows, 10);
        assert!(empty.is_empty());
    }
}
