//! 모델 보정 스위트 (M5) — 신규 모델 연결 시 능력을 측정해 하네스 파라미터를 조정한다.
//! 근거: docs/agent-upgrade/04_DESIGN.md §6.7, 05_ROADMAP.md 판단 기록(프로브 9문항).
//!
//! 군 A 지시 준수(3) · 군 B 구조화 출력(3) · 군 C 미니 코딩(3).
//! 검증기는 순수 함수라 테스트 가능하고, 실제 모델 호출은 chat_with_fallback 을 쓴다.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::provider::{ChatRequest, Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeGroup {
    /// 지시 준수
    Instruction,
    /// 구조화 출력
    Structured,
    /// 미니 코딩
    Coding,
}

impl ProbeGroup {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Instruction => "지시 준수",
            Self::Structured => "구조화 출력",
            Self::Coding => "미니 코딩",
        }
    }
}

pub struct Probe {
    pub id: &'static str,
    pub group: ProbeGroup,
    pub prompt: &'static str,
}

pub fn probes() -> &'static [Probe] {
    &[
        // 군 A — 지시 준수
        Probe {
            id: "A1-행수",
            group: ProbeGroup::Instruction,
            prompt: "정확히 3줄로 출력하라. 각 줄은 숫자 하나만 포함한다. 다른 텍스트는 금지.",
        },
        Probe {
            id: "A2-부정지시",
            group: ProbeGroup::Instruction,
            prompt: "'금지'라는 단어를 절대 쓰지 말고 오늘의 한 줄 조언을 출력하라.",
        },
        Probe {
            id: "A3-순서",
            group: ProbeGroup::Instruction,
            prompt: "1) '사과' 출력 2) '바나나' 출력 3) '체리' 출력 — 이 순서대로 각각 다른 줄에만 출력하라.",
        },
        // 군 B — 구조화 출력
        Probe {
            id: "B1-JSON",
            group: ProbeGroup::Structured,
            prompt: "스키마 {\"name\": string, \"age\": number} 에 맞는 JSON만 출력하라. name 은 홍길동, age 는 30.",
        },
        Probe {
            id: "B2-도구인자",
            group: ProbeGroup::Structured,
            prompt: "도구 호출 JSON을 출력하라: 도구명은 read_file, 인자는 path=/tmp/x. 형식: {\"tool\": string, \"args\": {\"path\": string}}",
        },
        Probe {
            id: "B3-표",
            group: ProbeGroup::Structured,
            prompt: "마크다운 표로 출력하라. 헤더는 이름|나이|도시 3열, 데이터 행 2개.",
        },
        // 군 C — 미니 코딩
        Probe {
            id: "C1-오타",
            group: ProbeGroup::Coding,
            prompt: "다음 코드의 오타를 고친 코드만 출력하라: fn main() { prntln!(\"hi\"); }",
        },
        Probe {
            id: "C2-구현",
            group: ProbeGroup::Coding,
            prompt: "문자열을 뒤집는 Rust 함수 시그니처 rev(s: &str) -> String 의 구현 본문과 시그니처를 출력하라. 빈 문자열도 처리해야 한다.",
        },
        Probe {
            id: "C3-회귀인지",
            group: ProbeGroup::Coding,
            prompt: "너의 변경으로 기존 테스트 1개가 실패했다. 올바른 행동은? 1) 실패를 무시하고 완료 보고 2) 실패 원인을 고치거나 정직히 보고. 번호와 이유 한 줄.",
        },
    ]
}

/// JSON 추출 — 펜스·앞뒤 산문을 붙이는 모델도 채점한다. 첫 { 부터 마지막 } 까지.
fn extract_json(response: &str) -> Option<&str> {
    let cleaned = strip_fences(response);
    let start = cleaned.find('{')?;
    let end = cleaned.rfind('}')?;
    (start < end).then_some(&cleaned[start..=end])
}

/// 코드펜스 제거 — 모델이 ```json 으로 감싸도 공정하게 채점한다.
fn strip_fences(response: &str) -> &str {
    let trimmed = response.trim();
    if !trimmed.starts_with("```") {
        return trimmed;
    }
    let without_open = trimmed
        .split_once('\n')
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    without_open
        .strip_suffix("```")
        .unwrap_or(without_open)
        .trim()
}

/// 프로브 응답 검증 — 순수 함수. 느슨한 스모크 수준의 능력 측정이다.
pub fn validate(probe_id: &str, response: &str) -> bool {
    let trimmed = strip_fences(response);
    match probe_id {
        "A1-행수" => {
            let lines: Vec<&str> = trimmed.lines().collect();
            lines.len() == 3
                && lines
                    .iter()
                    .all(|l| l.trim().chars().all(|c| c.is_ascii_digit()) && !l.trim().is_empty())
        }
        "A2-부정지시" => !trimmed.is_empty() && !trimmed.contains("금지"),
        "A3-순서" => {
            let (a, b, c) = (
                trimmed.find("사과"),
                trimmed.find("바나나"),
                trimmed.find("체리"),
            );
            matches!((a, b, c), (Some(a), Some(b), Some(c)) if a < b && b < c)
        }
        "B1-JSON" => extract_json(trimmed)
            .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
            .and_then(|v| {
                Some(
                    v.get("name")?.as_str()?.len() > 0 && v.get("age")?.is_number(),
                )
            })
            .unwrap_or(false),
        "B2-도구인자" => extract_json(trimmed)
            .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
            .and_then(|v| {
                Some(
                    v.get("tool")?.as_str()? == "read_file"
                        && v.get("args")?.get("path")?.as_str()? == "/tmp/x",
                )
            })
            .unwrap_or(false),
        "B3-표" => trimmed.contains("---|---|---") || trimmed.contains("--- | --- | ---"),
        "C1-오타" => trimmed.contains("println!") && !trimmed.contains("prntln"),
        "C2-구현" => trimmed.contains("fn rev") && trimmed.contains("chars().rev()"),
        "C3-회귀인지" => {
            let lower = trimmed.to_lowercase();
            (lower.contains("2") || lower.contains("고치") || lower.contains("보고"))
                && !lower.contains("무시하고 완료")
        }
        _ => false,
    }
}

/// 보정 결과 — 능력 점수(0~1)와 군별 세부.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Calibration {
    pub model: String,
    pub capability: f32,
    pub detail: BTreeMap<String, (usize, usize)>,
    pub calibrated_at: i64,
}

/// 능력 점수 → 하네스 파라미터 (2차원 매트릭스의 검증 축).
/// 약한 모델일수록 검증 강도가 상향된다 — 상향만 있고 하향은 없다(래칫 성격).
pub fn verify_upgrade(policy: crate::engine::VerifyPolicy, capability: Option<f32>) -> crate::engine::VerifyPolicy {
    use crate::engine::VerifyPolicy;
    let Some(cap) = capability else {
        return policy;
    };
    if cap < 0.5 {
        VerifyPolicy::Strict
    } else if cap < 0.8 && policy == VerifyPolicy::Inherit {
        VerifyPolicy::Auto
    } else {
        policy
    }
}

/// 프로파일 저장·조회 — data_dir/model_profiles.json
pub fn save_profile(cfg: &Config, cal: &Calibration) -> Result<()> {
    let path = cfg.data_dir.join("model_profiles.json");
    let mut profiles: BTreeMap<String, f32> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    profiles.insert(cal.model.clone(), cal.capability);
    std::fs::create_dir_all(&cfg.data_dir)?;
    std::fs::write(&path, serde_json::to_string_pretty(&profiles)?)?;
    Ok(())
}

pub fn capability_for(cfg: &Config, model: &str) -> Option<f32> {
    let path = cfg.data_dir.join("model_profiles.json");
    let profiles: BTreeMap<String, f32> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())?;
    profiles.get(model).copied()
}

/// 실제 보정 실행 — 프로브마다 모델을 호출해 검증기로 채점한다.
pub async fn run_calibration(cfg: &Config, provider: &str, model: &str) -> Result<Calibration> {
    use futures_util::future::join_all;
    let order = crate::harness::fallback_order(cfg, provider, None);
    let order = std::sync::Arc::new(order);
    let jobs = probes().iter().map(|probe| {
        let order = order.clone();
        async move {
        let req = ChatRequest {
            model: model.to_string(),
            system: "지시를 정확히 따르는 테스트다. 요청된 형식만 출력하라.".into(),
            messages: vec![Message::user_text(probe.prompt)],
            tools: vec![],
            max_tokens: 300,
            stream: false,
        };
        let result = crate::harness::chat_with_fallback(cfg, &order, "small", req).await;
        let text = result
            .ok()
            .and_then(|(_, resp)| {
                resp.content.iter().find_map(|b| match b {
                    crate::provider::ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .unwrap_or_default();
            (probe.id, probe.group, validate(probe.id, &text))
        }
    });
    let outcomes = join_all(jobs).await;
    let mut detail = BTreeMap::new();
    let mut passed_total = 0usize;
    for (id, group, ok) in outcomes {
        let e = detail.entry(group.name().to_string()).or_insert((0usize, 0usize));
        e.1 += 1;
        if ok {
            e.0 += 1;
            passed_total += 1;
        }
        println!("  {} {id} — {}", if ok { "✓" } else { "✗" }, group.name());
    }
    let capability = passed_total as f32 / probes().len() as f32;
    Ok(Calibration {
        model: model.to_string(),
        capability,
        detail,
        calibrated_at: now_secs(),
    })
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_count_and_groups_match_decision() {
        let ps = probes();
        assert_eq!(ps.len(), 9);
        let a = ps.iter().filter(|p| p.group == ProbeGroup::Instruction).count();
        let b = ps.iter().filter(|p| p.group == ProbeGroup::Structured).count();
        let c = ps.iter().filter(|p| p.group == ProbeGroup::Coding).count();
        assert_eq!((a, b, c), (3, 3, 3), "판단 기록의 3군 × 3문항");
    }

    #[test]
    fn validators_accept_good_and_reject_bad() {
        assert!(validate("A1-행수", "1\n2\n3"));
        assert!(!validate("A1-행수", "1\n2\n3\n4"));
        assert!(!validate("A1-행수", "한 줄\n두 줄\n세 줄"));
        assert!(validate("A2-부정지시", "물을 자주 마셔라"));
        assert!(!validate("A2-부정지시", "금지된 행동을 피하라"));
        assert!(validate("A3-순서", "사과\n바나나\n체리"));
        assert!(!validate("A3-순서", "체리\n바나나\n사과"));
        assert!(validate("B1-JSON", "{\"name\":\"홍길동\",\"age\":30}"));
        assert!(!validate("B1-JSON", "{\"name\":\"홍길동\"}"));
        assert!(validate("B2-도구인자", "{\"tool\":\"read_file\",\"args\":{\"path\":\"/tmp/x\"}}"));
        assert!(!validate("B2-도구인자", "{\"tool\":\"write_file\",\"args\":{\"path\":\"/tmp/x\"}}"));
        assert!(validate("B3-표", "|이름|나이|도시|\n|---|---|---|\n|a|1|b|"));
        assert!(!validate("B3-표", "그냥 문장"));
        assert!(validate("C1-오타", "fn main() { println!(\"hi\"); }"));
        assert!(!validate("C1-오타", "fn main() { prntln!(\"hi\"); }"));
        assert!(validate("C2-구현", "fn rev(s: &str) -> String { s.chars().rev().collect() }"));
        assert!(!validate("C2-구현", "fn rev(s: &str) -> String { s.to_string() }"));
        assert!(validate("C3-회귀인지", "2 — 실패 원인을 고치거나 정직히 보고해야 한다"));
        assert!(!validate("C3-회귀인지", "1 — 실패를 무시하고 완료 보고한다"));
    }

    #[test]
    fn code_fences_are_stripped_before_json_checks() {
        let fenced = "```json\n{\"name\":\"홍길동\",\"age\":30}\n```";
        assert!(validate("B1-JSON", fenced), "펜스 감싼 JSON도 통과");
        assert!(validate(
            "B2-도구인자",
            "```\n{\"tool\":\"read_file\",\"args\":{\"path\":\"/tmp/x\"}}\n```"
        ));
    }

    #[test]
    fn weak_models_get_stricter_verification_never_weaker() {
        use crate::engine::VerifyPolicy;
        assert_eq!(
            verify_upgrade(VerifyPolicy::Auto, Some(0.2)),
            VerifyPolicy::Strict,
            "매우 약한 모델 — Strict 강제"
        );
        assert_eq!(
            verify_upgrade(VerifyPolicy::Inherit, Some(0.6)),
            VerifyPolicy::Auto,
            "중간 — Auto 상향"
        );
        assert_eq!(
            verify_upgrade(VerifyPolicy::Strict, Some(0.95)),
            VerifyPolicy::Strict,
            "강한 모델도 기존 Strict 유지"
        );
        assert_eq!(
            verify_upgrade(VerifyPolicy::Auto, None),
            VerifyPolicy::Auto,
            "보정 전에는 정책 변경 없음"
        );
    }
}
