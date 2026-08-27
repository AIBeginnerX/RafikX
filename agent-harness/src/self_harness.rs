//! Self-Harness 엔진 — "Self-Harness: Harnesses That Improve Themselves"
//! (arXiv:2606.09498, Shanghai AI Lab 2026) 의 3단계 자기개선 루프 구현.
//!
//! 논문의 오프라인 벤치마크 루프(held-in/held-out 분할, K개 후보 병렬 평가)를
//! 대화형 에이전트에 맞게 온라인으로 번역했다:
//!
//! 1. Weakness Mining (3.2) — 매 에피소드 종료 시 verifier-grounded 실패
//!    시그니처 φ=(c,q,m) 를 추출한다. c(터미널 원인)는 AgentOutcome 에서
//!    결정적으로, q(인과 상태)·m(에이전트 메커니즘)은 소형 모델이 고정
//!    어휘에서 고른다 → 논문의 "정확 일치 클러스터링"이 유지된다.
//! 2. Harness Proposal (3.3) — 같은 시그니처의 지지도(support)가 임계값에
//!    도달하면, 모델을 proposer 로 호출해 K개의 서로 다른 최소 수정 후보를
//!    생성한다. 각 후보는 선언된 editable surface 하나만 바꾼다.
//! 3. Proposal Validation (3.4) — 후보를 순차 trial 로 활성화하고, 이후
//!    에피소드에서 (a) 타깃 시그니처 재발 없음(Δ_in ≥ 0 대응), (b) 전체
//!    성공률 비저하(Δ_ho ≥ 0 대응)를 모두 만족할 때만 승격(merge)한다.
//!    거부되면 Harness는 그대로 유지된다 (h_{t+1} = h_t).
//!
//! editable surfaces 는 논문 Figure 3 의 build_* 함수에 대응한다:
//! bootstrap/execution/verification/failure_recovery 지시문 + runtime_policy.
//! Harness 상태(h_t)는 ~/.rafikx/self_harness.json 에 버전·계보와 함께
//! 저장되어 모든 전이가 감사(audit) 가능하다.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::agent::AgentOutcome;
use crate::applog;
use crate::config::Config;
use crate::db::{Db, ShCandidateRow, ShEvidenceRow};
use crate::harness;
use crate::provider::{ChatRequest, ContentBlock, Message};

/// m(에이전트 메커니즘) 고정 어휘 — 논문 4.3 절의 관찰된 메커니즘에서 추렸다.
/// 자유 텍스트 대신 이 목록에서 고르게 해 시그니처 정확 일치 클러스터링을 지킨다.
pub const MECHANISMS: &[&str] = &[
    "missing_artifact",       // 요구 산출물을 만들지 않고 종료
    "unproductive_tool_loop", // 같은 도구·입력 반복, 진전 없는 루프
    "premature_conclusion",   // 검증 없이 완료 주장
    "blind_retry",            // 실패한 명령을 그대로 재시도
    "endless_exploration",    // 탐색만 계속하고 구현·검증으로 전환 실패
    "state_not_persisted",    // 셸 세션 간 환경·상태 유지 실패
    "dependency_missing",     // 의존성 사전 확인 누락
    "schema_mismatch",        // 도구 입력/출력 형식 오류
    "other",
];

const CAUSALS: &[&str] = &["direct", "contributing", "incidental"];

/// 편집 가능 surface 이름 — 논문 Figure 3 의 선언된 인터페이스에 대응.
pub const SURFACES: &[&str] = &[
    "bootstrap_instruction",
    "execution_instruction",
    "verification_instruction",
    "failure_recovery_instruction",
    "plan_instruction",
    "runtime_policy",
];

// ---------------------------------------------------------------------------
// Harness 상태 h_t
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Surfaces {
    pub bootstrap_instruction: String,
    pub execution_instruction: String,
    pub verification_instruction: String,
    pub failure_recovery_instruction: String,
    /// 계획 호출 전용 면 — decorate_system 이 아니라 계획 프롬프트에만 주입된다.
    /// 옛 self_harness.json(v2)에는 없으므로 serde default 로 채운다.
    #[serde(default = "default_plan_instruction")]
    pub plan_instruction: String,
}

fn default_plan_instruction() -> String {
    "계획을 세울 때 완료 기준을 검증 가능한 형태로 명시한다.".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimePolicy {
    #[serde(default)]
    pub enabled: bool,
    /// 에이전트 반복 상한 재정의 (binding.max_iterations 와 min 결합).
    #[serde(default)]
    pub max_iterations_override: Option<u32>,
    /// 루프 감지 시 개입 지시 — 시스템 프롬프트에 함께 주입된다.
    #[serde(default)]
    pub loop_break_instruction: Option<String>,
}

/// 검증 중인 후보 수정 — 활성 Harness에 임시로 겹쳐 적용된다 (h_t^(j) = Δ_j(h_t)).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialEdit {
    pub candidate_id: i64,
    pub surface: String,
    pub new_value: String,
    pub target_signature: String,
    pub started_at: i64,
}

/// 수락된 전이 기록 — 논문 3.4 의 "각 전이를 감사 가능하게" 요구 대응.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageEntry {
    pub version: u32,
    pub accepted_at: i64,
    pub surface: String,
    pub target_signature: String,
    pub summary: String,
    pub baseline_success: f64,
    pub trial_success: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfHarnessState {
    pub version: u32,
    pub surfaces: Surfaces,
    #[serde(default)]
    pub runtime_policy: RuntimePolicy,
    #[serde(default)]
    pub trial: Option<TrialEdit>,
    #[serde(default)]
    pub lineage: Vec<LineageEntry>,
}

impl Default for SelfHarnessState {
    /// h_0 — 논문 Figure 3 의 최소 초기 Harness를 한국어로 옮긴 것.
    fn default() -> Self {
        Self {
            version: 0,
            surfaces: Surfaces {
                bootstrap_instruction: "작업을 시작하면 먼저 워크스페이스를 조사해 가장 작은 관련 수정 지점을 찾는다.".into(),
                execution_instruction: "일반적인 조언 대신 구체적인 저장소 변경을 우선하고, 수정 범위를 작업에 필요한 최소로 유지한다.".into(),
                verification_instruction: "결론을 내리기 전에 실행 가능한 가장 표적화된 명령·파일 확인·테스트로 결과를 검증한다.".into(),
                failure_recovery_instruction: "도구 호출이 실패하면 오류를 살펴 접근을 바꾼다. 같은 행동을 그대로 재시도하지 않는다.".into(),
                plan_instruction: default_plan_instruction(),
            },
            runtime_policy: RuntimePolicy::default(),
            trial: None,
            lineage: Vec::new(),
        }
    }
}

impl SelfHarnessState {
    pub fn path() -> Result<PathBuf> {
        Ok(Config::data_dir()?.join("self_harness.json"))
    }

    /// 상태 로드 — 없거나 깨졌으면 초기 Harness h_0.
    pub fn load() -> Self {
        let Ok(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                applog::error(&format!("self-harness state parse 실패, h0 사용: {e}"));
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// 원자적 저장 (tmp 기록 후 rename) — 백그라운드 관찰과의 경합에서 파손 방지.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, &path).context("self_harness.json 저장 실패")?;
        Ok(())
    }

    fn surface_value(&self, surface: &str) -> String {
        match surface {
            "bootstrap_instruction" => self.surfaces.bootstrap_instruction.clone(),
            "execution_instruction" => self.surfaces.execution_instruction.clone(),
            "verification_instruction" => self.surfaces.verification_instruction.clone(),
            "failure_recovery_instruction" => self.surfaces.failure_recovery_instruction.clone(),
            "plan_instruction" => self.surfaces.plan_instruction.clone(),
            "runtime_policy" => serde_json::to_string(&self.runtime_policy).unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn set_surface(&mut self, surface: &str, value: &str) -> Result<()> {
        match surface {
            "bootstrap_instruction" => self.surfaces.bootstrap_instruction = value.to_string(),
            "execution_instruction" => self.surfaces.execution_instruction = value.to_string(),
            "verification_instruction" => {
                self.surfaces.verification_instruction = value.to_string()
            }
            "failure_recovery_instruction" => {
                self.surfaces.failure_recovery_instruction = value.to_string()
            }
            "plan_instruction" => self.surfaces.plan_instruction = value.to_string(),
            "runtime_policy" => {
                self.runtime_policy = serde_json::from_str(value)
                    .map_err(|e| anyhow!("runtime_policy JSON 이 아닙니다: {e}"))?;
            }
            other => anyhow::bail!("알 수 없는 surface: {other}"),
        }
        Ok(())
    }

    /// trial 수정을 겹쳐 적용한 현재 유효 상태 (h_t^(j)).
    fn effective(&self) -> Self {
        let mut s = self.clone();
        if let Some(t) = &self.trial {
            let _ = s.set_surface(&t.surface, &t.new_value);
        }
        s
    }

    /// 시스템 프롬프트에 Harness surface 를 주입한다 (engine=self 전용).
    pub fn decorate_system(&self, system: &mut String) {
        let eff = self.effective();
        let trial_mark = self
            .trial
            .as_ref()
            .map(|t| format!(" · trial #{} ({})", t.candidate_id, t.surface))
            .unwrap_or_default();
        system.push_str(&format!(
            "\n\n[Self-Harness v{}{}]\n\
             [작업 시작] {}\n\
             [실행 원칙] {}\n\
             [검증 원칙] {}\n\
             [실패 복구] {}",
            self.version,
            trial_mark,
            eff.surfaces.bootstrap_instruction,
            eff.surfaces.execution_instruction,
            eff.surfaces.verification_instruction,
            eff.surfaces.failure_recovery_instruction,
        ));
        if eff.runtime_policy.enabled
            && let Some(instr) = &eff.runtime_policy.loop_break_instruction
            && !instr.trim().is_empty()
        {
            system.push_str(&format!("\n[루프 개입] {}", instr.trim()));
        }
    }

    /// 계획 호출 전용 지침 (trial 포함 유효값). decorate_system 에는 들어가지 않는다 —
    /// 계획 프롬프트에서만 `[Self-Harness 계획 지침]` 으로 붙는다.
    pub fn plan_instruction(&self) -> String {
        self.effective()
            .surfaces
            .plan_instruction
            .trim()
            .to_string()
    }

    /// runtime_policy 의 반복 상한 재정의 (trial 포함 유효값).
    pub fn effective_iter_cap(&self) -> Option<u32> {
        let eff = self.effective();
        if eff.runtime_policy.enabled {
            eff.runtime_policy
                .max_iterations_override
                .filter(|n| *n > 0)
        } else {
            None
        }
    }

    /// trial 승격 — 수정을 surfaces 에 병합하고 버전을 올린다 (MergeAccepted).
    fn promote_trial(
        &mut self,
        trial: &TrialEdit,
        summary: &str,
        baseline_success: f64,
        trial_success: f64,
    ) -> Result<()> {
        self.set_surface(&trial.surface, &trial.new_value)?;
        self.version += 1;
        self.lineage.push(LineageEntry {
            version: self.version,
            accepted_at: now_secs(),
            surface: trial.surface.clone(),
            target_signature: trial.target_signature.clone(),
            summary: summary.chars().take(300).collect(),
            baseline_success,
            trial_success,
        });
        self.trial = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Weakness Mining — verifier-grounded 실패 시그니처
// ---------------------------------------------------------------------------

/// 독립 검증자 게이트 미통과 — status 는 ok 로 남고 사유만 error 에 실린다(설계 §5).
/// 상태가 아니라 error 접두로 판정하는 이유: 게이트는 산출물을 되돌리지 않고
/// "완료 기준 미충족"만 기록하기 때문이다.
fn gate_failed(outcome: &AgentOutcome) -> bool {
    outcome
        .error
        .as_deref()
        .is_some_and(|e| e.starts_with(GATE_FAIL_PREFIX))
}

/// harness::run_review_gate 가 outcome.error 에 남기는 접두어.
const GATE_FAIL_PREFIX: &str = "검증자 미통과";

/// c: 터미널 verifier-레벨 원인. AgentOutcome 에서 결정적으로 추출한다.
fn terminal_cause(outcome: &AgentOutcome) -> Option<&'static str> {
    if outcome.verify_fail.is_some() {
        return Some("verify_fail");
    }
    if gate_failed(outcome) {
        return Some("gate_fail");
    }
    match outcome.status.as_str() {
        "limit" => {
            if outcome
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("동일 도구"))
            {
                Some("tool_loop")
            } else {
                Some("iteration_limit")
            }
        }
        "incomplete" => Some("todo_incomplete"),
        "fail" => Some("run_fail"),
        _ => None,
    }
}

fn is_success(outcome: &AgentOutcome) -> bool {
    outcome.status == "ok" && outcome.verify_fail.is_none() && !gate_failed(outcome)
}

/// 트레이스 요약 — 도구 호출 시퀀스와 오류를 (q, m) 추론 입력으로 압축한다.
fn trace_summary(outcome: &AgentOutcome) -> String {
    let mut tools: Vec<String> = Vec::new();
    let mut error_results = 0usize;
    for m in &outcome.messages {
        for b in &m.content {
            match b {
                ContentBlock::ToolUse { name, .. } => tools.push(name.clone()),
                ContentBlock::ToolResult { is_error, .. } if *is_error => error_results += 1,
                _ => {}
            }
        }
    }
    if tools.len() > 25 {
        let skipped = tools.len() - 25;
        tools = tools.split_off(skipped);
        tools.insert(0, format!("(앞 {skipped}개 생략)"));
    }
    let mut s = format!(
        "도구 시퀀스: {}\n반복: {} · 도구 오류 결과: {}",
        if tools.is_empty() {
            "(없음)".into()
        } else {
            tools.join(" → ")
        },
        outcome.iterations,
        error_results
    );
    for e in outcome.tool_errors.iter().take(3) {
        s.push_str(&format!(
            "\n오류: {}",
            e.chars().take(200).collect::<String>()
        ));
    }
    if let Some(v) = &outcome.verify_fail {
        s.push_str(&format!(
            "\n검증 실패: {}",
            v.chars().take(300).collect::<String>()
        ));
    }
    if let Some(e) = &outcome.error {
        s.push_str(&format!(
            "\n종료 오류: {}",
            e.chars().take(200).collect::<String>()
        ));
    }
    s
}

/// (q, m) 추론 — 소형 모델이 고정 어휘에서 고른다. 실패 시 보수적 기본값.
async fn infer_mechanism(
    cfg: &Config,
    task: &str,
    cause: &str,
    trace: &str,
) -> (String, String, String) {
    let default = (
        "contributing".to_string(),
        "other".to_string(),
        String::new(),
    );
    let order = harness::fallback_order(cfg, &cfg.file.general.default_provider, None);
    let system = format!(
        "너는 에이전트 실행 실패 분석가다. 실패 트레이스를 보고 에이전트 측 행동 메커니즘을 분류하라.\n\
         mechanism 은 반드시 다음 중 하나: {}\n\
         causal 은 반드시 다음 중 하나: {} (터미널 실패 '{cause}' 에 그 행동이 기여한 정도)\n\
         JSON {{\"mechanism\":\"...\",\"causal\":\"...\",\"note\":\"근거 1문장\"}} 형식으로만 출력하라.",
        MECHANISMS.join(" | "),
        CAUSALS.join(" | ")
    );
    let req = ChatRequest {
        model: String::new(),
        system,
        messages: vec![Message::user_text(format!(
            "[작업]\n{task}\n\n[터미널 실패 원인]\n{cause}\n\n[트레이스 요약]\n{trace}"
        ))],
        tools: vec![],
        max_tokens: 256,
        stream: false,
    };
    harness::set_fallback_quiet(true);
    let call = harness::chat_with_fallback(cfg, &order, "small", req).await;
    harness::set_fallback_quiet(false);
    let Ok((_n, resp)) = call else {
        return default;
    };
    let text = first_text(&resp.content);
    let Some(v) = extract_json_object(&text) else {
        return default;
    };
    let mechanism = v
        .get("mechanism")
        .and_then(|x| x.as_str())
        .filter(|m| MECHANISMS.contains(m))
        .unwrap_or("other")
        .to_string();
    let causal = v
        .get("causal")
        .and_then(|x| x.as_str())
        .filter(|q| CAUSALS.contains(q))
        .unwrap_or("contributing")
        .to_string();
    let note = v
        .get("note")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .chars()
        .take(300)
        .collect();
    (causal, mechanism, note)
}

// ---------------------------------------------------------------------------
// Harness Proposal — 다양하되 최소인 후보 수정
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct Proposal {
    surface: String,
    new_value: serde_json::Value,
    #[serde(default)]
    expected_effect: String,
    #[serde(default)]
    regression_risk: String,
}

impl Proposal {
    /// new_value 를 surface 별 문자열 표현으로 정규화한다.
    fn value_string(&self) -> String {
        match &self.new_value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }

    /// 논문 3.4: editable surface 를 실제로 바꾸지 않는 제안은 거부.
    fn validate(&self, state: &SelfHarnessState) -> Result<String> {
        if !SURFACES.contains(&self.surface.as_str()) {
            anyhow::bail!("알 수 없는 surface '{}'", self.surface);
        }
        let value = self.value_string();
        if value.trim().is_empty() {
            anyhow::bail!("빈 수정값");
        }
        if self.surface == "runtime_policy" {
            let _: RuntimePolicy = serde_json::from_str(&value)
                .map_err(|e| anyhow!("runtime_policy JSON 오류: {e}"))?;
        }
        if state.surface_value(&self.surface).trim() == value.trim() {
            anyhow::bail!("현재 값과 동일 — surface 를 바꾸지 않음");
        }
        Ok(value)
    }
}

/// proposer 호출 — 논문 3.3 의 bounded proposal context 를 그대로 제공한다:
/// 편집 가능 surface 현재값, 실패 패턴 증거, 보존할 성공 행동, 과거 시도 요약.
async fn propose_candidates(
    cfg: &Config,
    state: &SelfHarnessState,
    ev: &ShEvidenceRow,
    baseline_success: f64,
    baseline_n: u32,
    attempts: &str,
    k: u32,
) -> Result<Vec<Proposal>> {
    let order = harness::fallback_order(cfg, &cfg.file.general.default_provider, None);
    let surfaces_ctx = SURFACES
        .iter()
        .map(|s| format!("- {s}: {}", state.surface_value(s)))
        .collect::<Vec<_>>()
        .join("\n");
    let system = format!(
        "너는 이 에이전트 자신의 Harness를 개선하는 proposer 다. 아래 증거에 근거해 \
         Harness 수정 후보를 {k}개 생성하라.\n\
         규칙 (모두 필수):\n\
         1. 각 후보는 editable surface 중 정확히 하나만 수정한다 (최소성).\n\
         2. 후보끼리는 서로 실질적으로 달라야 한다 — 같은 문구의 재표현 금지 (다양성).\n\
         3. 실패 메커니즘을 직접 겨냥한 좁은 수정만 한다. 전면 재작성 금지.\n\
         4. 통과하던 행동은 보존한다. 무관한 지시를 추가하지 않는다.\n\
         5. runtime_policy 를 고칠 때 new_value 는 \
         {{\"enabled\":true,\"max_iterations_override\":숫자또는null,\"loop_break_instruction\":\"문장또는null\"}} \
         형태의 JSON 객체다. 나머지 surface 의 new_value 는 해당 지시문의 전체 새 텍스트(한국어)다.\n\
         출력은 JSON 배열만: \
         [{{\"surface\":\"...\",\"new_value\":...,\"expected_effect\":\"...\",\"regression_risk\":\"...\"}}]"
    );
    let user = format!(
        "[편집 가능 surface — 현재 Harness v{}]\n{surfaces_ctx}\n\n\
         [타깃 실패 패턴 (verifier-grounded, 지지도 {})]\n\
         시그니처: {}\n터미널 원인: {} · 인과: {} · 메커니즘: {}\n\
         대표 작업: {}\n증상 노트: {}\n\n\
         [보존할 행동]\n최근 {}개 에피소드 성공률 {:.0}% — 이 수준을 떨어뜨리면 안 된다.\n\n\
         [과거 시도한 수정]\n{}",
        state.version,
        ev.support,
        ev.signature,
        ev.cause,
        ev.causal,
        ev.mechanism,
        ev.sample_task,
        ev.sample_detail,
        baseline_n,
        baseline_success * 100.0,
        if attempts.is_empty() {
            "(없음)"
        } else {
            attempts
        },
    );
    let req = ChatRequest {
        model: String::new(),
        system,
        messages: vec![Message::user_text(user)],
        tools: vec![],
        max_tokens: 2048,
        stream: false,
    };
    harness::set_fallback_quiet(true);
    let call = harness::chat_with_fallback(cfg, &order, "main", req).await;
    harness::set_fallback_quiet(false);
    let (_n, resp) = call?;
    let text = first_text(&resp.content);
    let arr =
        extract_json_array(&text).ok_or_else(|| anyhow!("proposer 응답에 JSON 배열이 없습니다"))?;
    let proposals: Vec<Proposal> = serde_json::from_value(arr)?;
    Ok(proposals)
}

// ---------------------------------------------------------------------------
// 에피소드 관찰 훅 — Mining → Validation → Proposal 순서로 한 스텝 진행
// ---------------------------------------------------------------------------

/// 진행 중인 백그라운드 관찰 태스크 — 단발 CLI(rafikx agent)가 프로세스 종료
/// 전에 flush_observations 로 완료를 기다릴 수 있게 핸들을 보관한다.
/// (tokio 런타임은 main 반환 시 미완료 spawn 을 abort 하므로, 실패 채굴처럼
/// 모델 호출이 낀 관찰은 대기 없이는 유실된다 — 실측으로 확인된 동작.)
static OBSERVE_TASKS: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>> =
    std::sync::Mutex::new(Vec::new());

/// 자기개선 메타 레이어가 켜져 있는가 — legacy `engine = "self"` 또는
/// `[self_harness] meta = true`. run_pipeline_inner 의 게이트와 같은 조건이라
/// 관찰(observe)이 메타 레이어에서 빠지지 않는다.
pub fn meta_active(cfg: &Config) -> bool {
    let (_, legacy_self) = crate::engine::normalize(&cfg.file.general.engine);
    cfg.file.self_harness.enabled && (legacy_self || cfg.file.self_harness.meta)
}

/// run_pipeline 종료 시 호출. 메타 레이어가 꺼져 있으면 아무것도 하지 않는다.
/// 메인 흐름을 막지 않도록 백그라운드로 돈다 (lessons::maybe_spawn 과 동일 패턴).
pub fn maybe_observe(cfg: &Config, task: &str, outcome: &AgentOutcome) {
    if !meta_active(cfg) {
        return;
    }
    // denied 는 사용자 판단이지 Harness 실패가 아니다 — 논문의 addressability
    // 기준(Harness 수정으로 해결 가능한 패턴만)에 따라 관찰에서 제외한다.
    if outcome.status == "denied" {
        return;
    }
    let cfg = cfg.clone();
    let task: String = task.chars().take(400).collect();
    let cause = terminal_cause(outcome).map(str::to_string);
    let success = is_success(outcome);
    let trace = trace_summary(outcome);
    let handle = tokio::spawn(async move {
        if let Err(e) = observe_async(&cfg, &task, cause.as_deref(), success, &trace).await {
            applog::error(&format!("self-harness observe skip: {e:#}"));
        }
    });
    if let Ok(mut tasks) = OBSERVE_TASKS.lock() {
        tasks.retain(|h| !h.is_finished());
        tasks.push(handle);
    }
}

/// 남은 관찰 태스크가 끝날 때까지 기다린다 (최대 max_wait). 단발 CLI 경로
/// (rafikx agent/ask)의 종료 직전에 호출한다 — 장수 프로세스(TUI·텔레그램)는
/// 다음 턴 대기 중에 자연히 완료되므로 부를 필요가 없다.
pub async fn flush_observations(max_wait: std::time::Duration) {
    let handles: Vec<tokio::task::JoinHandle<()>> = match OBSERVE_TASKS.lock() {
        Ok(mut tasks) => tasks.drain(..).collect(),
        Err(_) => return,
    };
    let pending: Vec<_> = handles.into_iter().filter(|h| !h.is_finished()).collect();
    if pending.is_empty() {
        return;
    }
    crate::ui::note("self-harness 관찰 기록 중…");
    let join_all = async {
        for h in pending {
            let _ = h.await;
        }
    };
    let _ = tokio::time::timeout(max_wait, join_all).await;
}

/// rusqlite Connection 은 Send 가 아니므로, DB 접근(동기)과 모델 호출(await)을
/// 분리해 await 지점을 가로질러 Db 를 보유하지 않는다 (lessons.rs 와 동일 패턴).
async fn observe_async(
    cfg: &Config,
    task: &str,
    cause: Option<&str>,
    success: bool,
    trace: &str,
) -> Result<()> {
    let mut state = SelfHarnessState::load();

    // 1) Weakness Mining 의 (q, m) 추론 — 모델 호출, DB 없음.
    let mined = match cause {
        Some(c) => Some((c, infer_mechanism(cfg, task, c, trace).await)),
        None => None,
    };

    // 2) 동기 DB 단계: 클러스터 누적 → 에피소드 기록 → trial 판정 → 제안 준비.
    let prep = {
        let db = Db::open(&Db::db_path()?)?;
        let trial_id = state.trial.as_ref().map(|t| t.candidate_id);
        let mut signature = String::new();
        if let Some((c, (causal, mechanism, note))) = &mined {
            signature = format!("{c}|{causal}|{mechanism}");
            // 소형 모델이 note 를 비워 보내면 트레이스의 오류 라인으로 대체한다 —
            // 증상 노트가 비면 proposer 가 받는 증거가 얇아져 제안 품질이 떨어진다.
            let detail = if note.trim().is_empty() {
                trace
                    .lines()
                    .filter(|l| {
                        l.starts_with("오류:")
                            || l.starts_with("검증 실패:")
                            || l.starts_with("종료 오류:")
                    })
                    .collect::<Vec<_>>()
                    .join(" / ")
            } else {
                note.clone()
            };
            db.sh_upsert_evidence(&signature, c, causal, mechanism, task, &detail)?;
            applog::info(&format!("self-harness mined: {signature}"));
        }
        db.sh_add_episode(state.version as i64, trial_id, success, &signature)?;

        if let Some(trial) = state.trial.clone() {
            // Proposal Validation — 진행 중 trial 이 있으면 판정만 하고 끝낸다.
            run_validation(cfg, &db, &mut state, &trial)?;
            return Ok(());
        }
        prepare_proposal(cfg, &db, &mut state)?
    };

    // 3) Harness Proposal — proposer 모델 호출 (await, DB 없음).
    let Some(prep) = prep else {
        return Ok(());
    };
    let proposals = propose_candidates(
        cfg,
        &state,
        &prep.evidence,
        prep.baseline_success,
        prep.baseline_n,
        &prep.attempts,
        cfg.file.self_harness.proposal_width.max(1),
    )
    .await?;

    // 4) 다시 동기 DB 단계: 후보 저장 후 첫 후보의 trial 시작.
    let db = Db::open(&Db::db_path()?)?;
    store_proposals(cfg, &db, &mut state, &prep, proposals)
}

/// 판정에 필요한 최소 기준선 표본. 이보다 얇으면 성공률 비교가 잡음이다.
/// 주의: 이 게이트는 통계 검정이 아니라 보수적 휴리스틱이다 (설계 §15.4).
const MIN_BASELINE_N: i64 = 5;

/// 기준선 표본이 얇을 때 판정을 미룰지 (순수 함수).
/// trial 중에는 비-trial 에피소드가 늘지 않으므로 무기한 보류는 교착이다 —
/// trial 관측이 최소치의 3배까지 쌓이면 얇은 기준선 그대로 판정한다.
fn defer_for_thin_baseline(baseline_n: u32, episodes: i64, min: i64) -> bool {
    (baseline_n as i64) < MIN_BASELINE_N && episodes < min.saturating_mul(3)
}

/// 논문 3.4 수락 규칙의 온라인 번역:
/// Δ_in ≥ 0 ← trial 중 타깃 원인(cause) 재발 0 (기준선에서 반복되던 패턴 소멸)
/// Δ_ho ≥ 0 ← trial 성공률이 기준선 성공률 이상 (다른 행동의 회귀 없음)
fn run_validation(
    cfg: &Config,
    db: &Db,
    state: &mut SelfHarnessState,
    trial: &TrialEdit,
) -> Result<()> {
    let stats = db.sh_trial_stats(trial.candidate_id, &trial.target_signature)?;
    let min = cfg.file.self_harness.trial_min_episodes.max(1) as i64;
    if stats.episodes < min {
        return Ok(());
    }
    // 기준선 표본이 너무 얇으면 승격도 기각도 근거가 없다 — 판정을 미루고 trial 을 이어간다.
    let (_, _, baseline_n) = db.sh_baseline(
        &trial.target_signature,
        cfg.file.self_harness.baseline_window as i64,
    )?;
    if defer_for_thin_baseline(baseline_n, stats.episodes, min) {
        applog::info(&format!(
            "self-harness 판정 보류: 기준선 표본 {baseline_n}건 (필요 {MIN_BASELINE_N}건), trial {}회",
            stats.episodes
        ));
        return Ok(());
    }
    let Some(cand) = db.sh_candidate(trial.candidate_id)? else {
        // 후보 기록이 사라졌으면 trial 을 해제만 한다.
        state.trial = None;
        return state.save();
    };
    let trial_success = stats.successes as f64 / stats.episodes as f64;
    let delta_in_ok = stats.target_recurrences == 0;
    let delta_ho_ok = trial_success + 1e-9 >= cand.baseline_success;

    if delta_in_ok && delta_ho_ok {
        let note = format!(
            "accepted: 재발 0/{}에피소드, 성공률 {:.0}%→{:.0}%",
            stats.episodes,
            cand.baseline_success * 100.0,
            trial_success * 100.0
        );
        state.promote_trial(
            trial,
            &cand_summary(&cand),
            cand.baseline_success,
            trial_success,
        )?;
        state.save()?;
        db.sh_decide_candidate(trial.candidate_id, "accepted", &note)?;
        db.sh_mark_addressed(cand.evidence_id)?;
        // 승격으로 base_version 이 지난 나머지 후보는 폐기 (병렬 후보의 순차 번역).
        db.sh_stale_proposed(state.version as i64)?;
        applog::info(&format!(
            "self-harness promote: v{} {} ({})",
            state.version, trial.surface, note
        ));
    } else {
        let note = format!(
            "rejected: 재발 {}회, 성공률 {:.0}%→{:.0}% (기준 미달)",
            stats.target_recurrences,
            cand.baseline_success * 100.0,
            trial_success * 100.0
        );
        state.trial = None;
        state.save()?;
        db.sh_decide_candidate(trial.candidate_id, "rejected", &note)?;
        applog::info(&format!(
            "self-harness reject: #{} {note}",
            trial.candidate_id
        ));
        // 같은 세대의 다음 후보가 있으면 이어서 trial (순차 평가).
        if let Some(next) = db.sh_next_proposed(state.version as i64)? {
            start_trial(db, state, &next)?;
        }
    }
    Ok(())
}

/// proposer 호출에 필요한 준비물 — DB 단계와 모델 호출 단계를 잇는 스냅샷.
struct ProposalPrep {
    evidence: ShEvidenceRow,
    baseline_success: f64,
    baseline_target: i64,
    baseline_n: u32,
    attempts: String,
}

/// 제안이 필요한지 동기적으로 판단한다. 이번 세대의 대기 후보가 있으면
/// 그 trial 을 시작하고 None, 임계 미달이거나 이미 시도한 패턴이면 None.
fn prepare_proposal(
    cfg: &Config,
    db: &Db,
    state: &mut SelfHarnessState,
) -> Result<Option<ProposalPrep>> {
    let sh_cfg = &cfg.file.self_harness;
    if let Some(next) = db.sh_next_proposed(state.version as i64)? {
        start_trial(db, state, &next)?;
        return Ok(None);
    }
    let Some(ev) = db.sh_top_unaddressed(sh_cfg.proposal_threshold as i64)? else {
        return Ok(None);
    };
    if db.sh_has_candidates_for(ev.id, state.version as i64)? {
        // 이 패턴은 이번 세대에서 이미 시도했다 — 같은 증거로 재제안하지 않는다.
        return Ok(None);
    }
    let (baseline_success, baseline_target, baseline_n) =
        db.sh_baseline(&ev.signature, sh_cfg.baseline_window as i64)?;
    let attempts = db.sh_attempts_summary(ev.id, 5)?;
    Ok(Some(ProposalPrep {
        evidence: ev,
        baseline_success,
        baseline_target,
        baseline_n,
        attempts,
    }))
}

/// 검증을 통과한 제안을 저장하고 첫 후보의 trial 을 시작한다.
fn store_proposals(
    cfg: &Config,
    db: &Db,
    state: &mut SelfHarnessState,
    prep: &ProposalPrep,
    proposals: Vec<Proposal>,
) -> Result<()> {
    let ev = &prep.evidence;
    let mut first: Option<ShCandidateRow> = None;
    let mut kept = 0u32;
    for p in proposals {
        let value = match p.validate(state) {
            Ok(v) => v,
            Err(e) => {
                applog::info(&format!("self-harness proposal 거부: {e}"));
                continue;
            }
        };
        let audit = serde_json::json!({
            "expected_effect": p.expected_effect,
            "regression_risk": p.regression_risk,
        })
        .to_string();
        let id = db.sh_add_candidate(
            ev.id,
            &p.surface,
            &value,
            &audit,
            state.version as i64,
            &ev.signature,
            prep.baseline_success,
            prep.baseline_target,
        )?;
        kept += 1;
        if first.is_none() {
            first = db.sh_candidate(id)?;
        }
        if kept >= cfg.file.self_harness.proposal_width {
            break;
        }
    }
    applog::info(&format!(
        "self-harness proposed {kept} candidates for {} (support {})",
        ev.signature, ev.support
    ));
    if let Some(cand) = first {
        start_trial(db, state, &cand)?;
    }
    Ok(())
}

fn start_trial(db: &Db, state: &mut SelfHarnessState, cand: &ShCandidateRow) -> Result<()> {
    state.trial = Some(TrialEdit {
        candidate_id: cand.id,
        surface: cand.surface.clone(),
        new_value: cand.new_value.clone(),
        target_signature: cand.target_signature.clone(),
        started_at: now_secs(),
    });
    state.save()?;
    db.sh_start_trial(cand.id)?;
    applog::info(&format!(
        "self-harness trial start: #{} {} (target {})",
        cand.id, cand.surface, cand.target_signature
    ));
    Ok(())
}

fn cand_summary(cand: &ShCandidateRow) -> String {
    let effect = serde_json::from_str::<serde_json::Value>(&cand.audit_json)
        .ok()
        .and_then(|v| {
            v.get("expected_effect")
                .and_then(|x| x.as_str())
                .map(String::from)
        })
        .unwrap_or_default();
    if effect.is_empty() {
        format!("{} 수정", cand.surface)
    } else {
        effect
    }
}

// ---------------------------------------------------------------------------
// 상태 표시 (/engine 명령용)
// ---------------------------------------------------------------------------

pub fn status_lines(db: &Db) -> Vec<String> {
    let state = SelfHarnessState::load();
    let mut out = Vec::new();
    let (episodes, failures) = db.sh_episode_counts(state.version as i64).unwrap_or((0, 0));
    let clusters = db.sh_open_cluster_count().unwrap_or(0);
    out.push(format!(
        "Self-Harness v{} · 이번 버전 에피소드 {episodes}회 (실패 {failures}) · 미해결 실패 클러스터 {clusters}개",
        state.version
    ));
    if let Some(t) = &state.trial {
        out.push(format!(
            "  trial #{} 진행 중 — {} 수정 검증 (타깃 {})",
            t.candidate_id, t.surface, t.target_signature
        ));
    }
    for l in state.lineage.iter().rev().take(3) {
        out.push(format!(
            "  v{} 승격: {} ← {} ({:.0}%→{:.0}%)",
            l.version,
            l.surface,
            l.target_signature,
            l.baseline_success * 100.0,
            l.trial_success * 100.0
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// 헬퍼
// ---------------------------------------------------------------------------

fn first_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn extract_json_object(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(&text[start..=end]).ok()
}

fn extract_json_array(text: &str) -> Option<serde_json::Value> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    let v: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    v.is_array().then_some(v)
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

    fn failed_outcome(status: &str, error: Option<&str>) -> AgentOutcome {
        AgentOutcome {
            status: status.into(),
            error: error.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn thin_baseline_defers_until_trial_evidence_piles_up() {
        // 기준선이 얇으면 판정을 미룬다 (설계 §15.4).
        assert!(defer_for_thin_baseline(0, 5, 5));
        assert!(defer_for_thin_baseline(4, 5, 5));
        // 표본이 충분하면 곧바로 판정한다.
        assert!(!defer_for_thin_baseline(5, 5, 5));
        assert!(!defer_for_thin_baseline(20, 5, 5));
        // trial 중에는 비-trial 에피소드가 늘지 않는다 — 관측이 3배 쌓이면 교착을 푼다.
        assert!(!defer_for_thin_baseline(0, 15, 5));
        assert!(defer_for_thin_baseline(0, 14, 5));
    }

    #[test]
    fn verify_recovery_is_not_counted_as_failure() {
        // 재시도로 통과한 실행은 성공이다 — verify_fail 이 비어 있으면 원인도 없다.
        let mut recovered = failed_outcome("ok", None);
        recovered.verify_recovered = Some("cargo check 실패".into());
        assert_eq!(terminal_cause(&recovered), None);
        assert!(is_success(&recovered));
    }

    #[test]
    fn terminal_cause_is_deterministic_and_verifier_grounded() {
        assert_eq!(terminal_cause(&failed_outcome("ok", None)), None);
        assert_eq!(
            terminal_cause(&failed_outcome("limit", Some("반복 상한"))),
            Some("iteration_limit")
        );
        assert_eq!(
            terminal_cause(&failed_outcome("limit", Some("동일 도구 3회 연속 반복"))),
            Some("tool_loop")
        );
        assert_eq!(
            terminal_cause(&failed_outcome("incomplete", None)),
            Some("todo_incomplete")
        );
        let mut o = failed_outcome("ok", None);
        o.verify_fail = Some("cargo check 실패".into());
        assert_eq!(terminal_cause(&o), Some("verify_fail"));
        assert!(!is_success(&o));
    }

    #[test]
    fn review_gate_failure_is_mined_as_gate_fail_and_not_a_success() {
        // 게이트 미통과는 status 를 ok 로 남기고 error 에만 사유를 적는다 —
        // status 만 보면 성공으로 오집계되므로 error 접두로 판정한다.
        let gated = failed_outcome("ok", Some("검증자 미통과: [미충족 항목] 3번 미구현"));
        assert_eq!(terminal_cause(&gated), Some("gate_fail"));
        assert!(!is_success(&gated));

        // 일반 성공·다른 사유의 error 는 영향을 받지 않는다.
        assert!(is_success(&failed_outcome("ok", None)));
        let other = failed_outcome("ok", Some("검증 생략: 빌드 없음"));
        assert_eq!(terminal_cause(&other), None);
        assert!(is_success(&other));

        // 검증 명령 실패가 함께 있으면 verify_fail 이 우선한다 (더 구체적인 원인).
        let mut both = failed_outcome("ok", Some("검증자 미통과: 회귀"));
        both.verify_fail = Some("cargo test 실패".into());
        assert_eq!(terminal_cause(&both), Some("verify_fail"));
        assert!(!is_success(&both));
    }

    #[test]
    fn state_roundtrip_and_surface_edit() {
        let mut s = SelfHarnessState::default();
        assert_eq!(s.version, 0);
        s.set_surface("verification_instruction", "새 검증 규칙")
            .unwrap();
        assert_eq!(s.surfaces.verification_instruction, "새 검증 규칙");
        s.set_surface(
            "runtime_policy",
            r#"{"enabled":true,"max_iterations_override":12,"loop_break_instruction":"루프를 멈추고 요약하라"}"#,
        )
        .unwrap();
        assert_eq!(s.effective_iter_cap(), Some(12));
        assert!(s.set_surface("nope", "x").is_err());

        let json = serde_json::to_string(&s).unwrap();
        let back: SelfHarnessState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.surfaces.verification_instruction, "새 검증 규칙");
        assert!(back.runtime_policy.enabled);
    }

    #[test]
    fn plan_instruction_surface_loads_from_v2_files_and_stays_out_of_system() {
        // 기존 self_harness.json(v2)에는 plan_instruction 이 없다 — serde default 로 채워야 한다.
        let v2 = r#"{
            "version": 2,
            "surfaces": {
                "bootstrap_instruction": "b",
                "execution_instruction": "e",
                "verification_instruction": "v",
                "failure_recovery_instruction": "f"
            },
            "runtime_policy": {"enabled": false},
            "trial": null,
            "lineage": []
        }"#;
        let mut s: SelfHarnessState = serde_json::from_str(v2).expect("v2 파일 로드");
        assert_eq!(s.version, 2);
        assert_eq!(s.surfaces.plan_instruction, default_plan_instruction());
        assert!(s.plan_instruction().contains("완료 기준"));

        // 계획 전용 면이므로 시스템 프롬프트 장식에는 들어가지 않는다.
        let mut sys = String::from("base");
        s.decorate_system(&mut sys);
        assert!(!sys.contains(&s.surfaces.plan_instruction));

        // 자기개선 루프의 편집 대상이다 (SURFACES 등록 + set/get 왕복).
        assert!(SURFACES.contains(&"plan_instruction"));
        s.set_surface("plan_instruction", "계획에 위험을 한 줄씩 적는다")
            .unwrap();
        assert_eq!(
            s.surface_value("plan_instruction"),
            "계획에 위험을 한 줄씩 적는다"
        );
        // trial 겹침도 계획 면에 적용된다.
        s.trial = Some(TrialEdit {
            candidate_id: 7,
            surface: "plan_instruction".into(),
            new_value: "trial 계획 지시".into(),
            target_signature: "todo_incomplete|direct|other".into(),
            started_at: 0,
        });
        assert_eq!(s.plan_instruction(), "trial 계획 지시");
    }

    #[test]
    fn trial_overlay_applies_without_mutating_base() {
        let s = SelfHarnessState {
            trial: Some(TrialEdit {
                candidate_id: 1,
                surface: "bootstrap_instruction".into(),
                new_value: "trial 지시문".into(),
                target_signature: "verify_fail|direct|missing_artifact".into(),
                started_at: 0,
            }),
            ..SelfHarnessState::default()
        };
        let mut sys = String::from("base");
        s.decorate_system(&mut sys);
        assert!(sys.contains("trial 지시문"));
        assert!(sys.contains("trial #1"));
        // 원본 surface 는 그대로 — 승격 전에는 병합되지 않는다.
        assert!(s.surfaces.bootstrap_instruction.contains("워크스페이스"));
    }

    #[test]
    fn promote_merges_edit_and_records_lineage() {
        let mut s = SelfHarnessState::default();
        let trial = TrialEdit {
            candidate_id: 7,
            surface: "failure_recovery_instruction".into(),
            new_value: "오류 파일을 먼저 복구하라".into(),
            target_signature: "tool_loop|direct|blind_retry".into(),
            started_at: 0,
        };
        s.trial = Some(trial.clone());
        s.promote_trial(&trial, "복구 지시 강화", 0.5, 0.8).unwrap();
        assert_eq!(s.version, 1);
        assert!(s.trial.is_none());
        assert_eq!(
            s.surfaces.failure_recovery_instruction,
            "오류 파일을 먼저 복구하라"
        );
        assert_eq!(s.lineage.len(), 1);
        assert_eq!(
            s.lineage[0].target_signature,
            "tool_loop|direct|blind_retry"
        );
    }

    #[test]
    fn proposal_validation_rejects_no_op_and_unknown_surface() {
        let state = SelfHarnessState::default();
        let noop = Proposal {
            surface: "bootstrap_instruction".into(),
            new_value: serde_json::Value::String(state.surfaces.bootstrap_instruction.clone()),
            expected_effect: String::new(),
            regression_risk: String::new(),
        };
        assert!(noop.validate(&state).is_err());
        let unknown = Proposal {
            surface: "system_prompt".into(),
            new_value: serde_json::Value::String("x".into()),
            expected_effect: String::new(),
            regression_risk: String::new(),
        };
        assert!(unknown.validate(&state).is_err());
        let bad_policy = Proposal {
            surface: "runtime_policy".into(),
            new_value: serde_json::Value::String("이건 JSON 이 아님".into()),
            expected_effect: String::new(),
            regression_risk: String::new(),
        };
        assert!(bad_policy.validate(&state).is_err());
        let ok = Proposal {
            surface: "verification_instruction".into(),
            new_value: serde_json::Value::String("최종 답 전에 반드시 검증 명령을 실행한다".into()),
            expected_effect: "검증 없는 완료 주장 방지".into(),
            regression_risk: "낮음".into(),
        };
        assert!(ok.validate(&state).is_ok());
    }

    #[test]
    fn json_extractors_tolerate_prose() {
        let obj =
            extract_json_object("답: {\"mechanism\":\"blind_retry\",\"causal\":\"direct\"} 끝")
                .unwrap();
        assert_eq!(obj.get("mechanism").unwrap().as_str(), Some("blind_retry"));
        let arr = extract_json_array(
            "후보는 다음과 같다 [{\"surface\":\"runtime_policy\",\"new_value\":{}}] .",
        )
        .unwrap();
        assert!(arr.is_array());
        assert!(extract_json_array("배열 없음 {}").is_none());
    }
}
