//! 폴 fallback 아키텍트 (F6) — 모델이 설계 불확실성으로 실행 없이 끝날 때
//! 미해결 공학 질문을 아키텍트 레인(도구 0개, 순수 사고)으로 돌리고 실행을 재개한다.
//!
//! 3중 게이트로 오탐을 막는다: ① 파일 변경 없음 ② 사실상 도구 활동 없음(반복 1회)
//! ③ 불확실 신호 문구 — 셋 다 만족할 때만 발동. 도구가 필요 없는 정상 질의응답은
//! ①②에서 이미 걸러진다. 턴당 최대 1회 (호출부가 예산을 관리).

use anyhow::Result;

use crate::config::Config;
use crate::provider::{ChatRequest, ContentBlock, Message};
use crate::run::RunContext;

/// 기본 불확실 신호 — config [fallback] refusal_signals 로 재정의 가능.
pub const DEFAULT_SIGNALS: &[&str] = &[
    "확실하지 않",
    "설계를 먼저",
    "판단이 필요",
    "정책상",
    "모르겠",
    "can't",
    "cannot determine",
    "cannot decide",
];

pub const EXTRACT_SYSTEM: &str = "아래 답변에서 실행을 막은 공학 질문만 한 문장으로 추출하라. 질문이 없으면 '없음'이라고만 답하라.";

pub const ARCHITECT_SYSTEM: &str = "\
    20년 경력 아키텍트다. 실행을 막은 공학 질문에 트레이드오프를 들어 2~3개 안을 비교하고 \
    하나를 권고한다. 코드를 쓰지 않는다. 도구도 없다 — 사고만으로 판단한다.\n\
    출력 형식:\n\
    [권고] 채택할 방향 한 줄\n\
    [근거] 2~4줄 — 왜 이 안이 이 작업에서 맞는가\n\
    [대안과 기각 사유] 나머지 안 각각 한 줄";

/// 거부 후보인가 (순수 함수 — 3중 게이트).
pub fn is_refusal_candidate(
    answer: &str,
    no_changes: bool,
    single_iteration: bool,
    signals: &[String],
) -> bool {
    no_changes
        && single_iteration
        && signals
            .iter()
            .any(|s| !s.is_empty() && answer.contains(s.as_str()))
}

/// 설정의 신호 목록 — 비어 있으면 기본 목록.
pub fn refusal_signals(cfg: &Config) -> Vec<String> {
    let custom = &cfg.file.fallback.refusal_signals;
    if custom.is_empty() {
        DEFAULT_SIGNALS.iter().map(|s| s.to_string()).collect()
    } else {
        custom.clone()
    }
}

async fn call_once(
    cfg: &Config,
    run: Option<&RunContext>,
    role: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<String> {
    let order = crate::harness::fallback_order(cfg, &cfg.file.general.default_provider, None);
    let req = ChatRequest {
        model: String::new(),
        system: system.into(),
        messages: vec![Message::user_text(user)],
        tools: vec![],
        max_tokens,
        stream: false,
    };
    let (_name, resp) = match run {
        Some(run) => crate::harness::chat_with_fallback_in_run(cfg, run, &order, role, req).await?,
        None => crate::harness::chat_with_fallback(cfg, &order, role, req).await?,
    };
    Ok(resp
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.trim().to_string()),
            _ => None,
        })
        .unwrap_or_default())
}

/// 아키텍트 상담 — Some(판단)이면 재시도 가치 있음, None 이면 진짜 질문이 아님(발동 안 함).
pub async fn consult_architect(cfg: &Config, task: &str, answer: &str) -> Result<Option<String>> {
    consult_architect_inner(cfg, None, task, answer).await
}

pub(crate) async fn consult_architect_in_run(
    cfg: &Config,
    run: &RunContext,
    task: &str,
    answer: &str,
) -> Result<Option<String>> {
    consult_architect_inner(cfg, Some(run), task, answer).await
}

async fn consult_architect_inner(
    cfg: &Config,
    run: Option<&RunContext>,
    task: &str,
    answer: &str,
) -> Result<Option<String>> {
    let question = call_once(cfg, run, "small", EXTRACT_SYSTEM, answer, 256).await?;
    if question.is_empty()
        || question
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .starts_with("없음")
    {
        return Ok(None);
    }
    let judgment = call_once(
        cfg,
        run,
        "main",
        ARCHITECT_SYSTEM,
        &format!("원 작업: {task}\n\n실행을 막은 질문: {question}"),
        1024,
    )
    .await?;
    if judgment.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!("질문: {question}\n\n{judgment}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig() -> Vec<String> {
        DEFAULT_SIGNALS.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fires_only_when_all_three_gates_pass() {
        // 3중 게이트 통과 — 거부 후보
        assert!(is_refusal_candidate(
            "이 부분은 설계를 먼저 결정해야 합니다.",
            true,
            true,
            &sig()
        ));
        // 변경이 있으면(실행함) 미발동
        assert!(!is_refusal_candidate(
            "이 부분은 설계를 먼저 결정해야 합니다.",
            false,
            true,
            &sig()
        ));
        // 도구 활동이 있으면 미발동
        assert!(!is_refusal_candidate(
            "이 부분은 설계를 먼저 결정해야 합니다.",
            true,
            false,
            &sig()
        ));
        // 신호 문구가 없으면 미발동
        assert!(!is_refusal_candidate("완료했습니다.", true, true, &sig()));
    }

    #[test]
    fn normal_qa_never_fires() {
        // 도구 필요 없는 정상 질의응답 — 불확실 신호가 없는 한 미발동
        assert!(!is_refusal_candidate(
            "Rust의 소유권은 이렇게 동작합니다.",
            true,
            true,
            &sig()
        ));
    }

    #[test]
    fn signals_cover_english() {
        assert!(is_refusal_candidate(
            "I can't determine the right approach.",
            true,
            true,
            &sig()
        ));
    }

    #[test]
    fn empty_signal_entries_are_ignored() {
        let signals = vec![String::new()];
        assert!(!is_refusal_candidate("아무 답변", true, true, &signals));
    }

    #[test]
    fn custom_signals_replace_defaults_via_caller() {
        let custom = vec!["막히네요".to_string()];
        assert!(is_refusal_candidate(
            "이건 좀 막히네요",
            true,
            true,
            &custom
        ));
        assert!(!is_refusal_candidate(
            "설계를 먼저 결정해야 합니다",
            true,
            true,
            &custom
        ));
    }
}
