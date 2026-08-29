//! Harness 엔진 카탈로그 — 엔진 차이를 제어 흐름 분기가 아니라 데이터로 표현한다.
//!
//! 각 엔진은 해당 Harness의 검증된 품질 장치를 시스템 프롬프트와 런타임 플래그로
//! 옮겨 담은 것이다(이름 차용이 아님). 근거: dsh pre/post 훅 파이프라인,
//! Claude Code TodoWrite·사이드체인·verify-work, Qwen Code ReAct 루프,
//! Kimi K2 기술 리포트(arXiv:2507.20534)의 루브릭·도구 연쇄.

use std::borrow::Cow;
use std::collections::HashMap;

use serde::Deserialize;

/// 계획 단계 깊이. Contract 는 dev/advanced 클래스에서만 활성된다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDepth {
    /// 계획 호출을 건너뛴다.
    Off,
    /// 3~7개 항목 목록 (기존 동작).
    Brief,
    /// [해석]/[완료 기준]/[작업 분해] 3부 산출물.
    Contract,
}

impl PlanDepth {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Some(Self::Off),
            "brief" => Some(Self::Brief),
            "contract" => Some(Self::Contract),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Brief => "brief",
            Self::Contract => "contract",
        }
    }
}

/// 검증 강도. Phase 1 에서는 선언만 하고 파이프라인은 아직 참조하지 않는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyPolicy {
    /// 프로파일의 verify 설정을 그대로 따른다.
    Inherit,
    /// verify 를 강제로 켠다 (명령 자동 감지).
    Auto,
    /// Auto + 독립 검증자 게이트.
    Strict,
}

impl VerifyPolicy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "inherit" => Some(Self::Inherit),
            "auto" => Some(Self::Auto),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Auto => "auto",
            Self::Strict => "strict",
        }
    }
}

/// 엔진 하나의 전체 사양. 내장 카탈로그는 정적 문자열을, config 오버라이드가
/// 적용된 사본은 소유 문자열을 담는다(Cow).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSpec {
    pub name: Cow<'static, str>,
    /// /engine 목록 한 줄 설명.
    pub summary: Cow<'static, str>,
    /// 시스템 프롬프트 증강 블록. 비어 있으면 주입하지 않는다.
    pub prompt_block: Cow<'static, str>,
    /// true 면 도구가 있는 모든 작업에 todo 단계화를 강제한다.
    pub force_staged: bool,
    pub plan_depth: PlanDepth,
    pub verify_policy: VerifyPolicy,
    /// goal continuation 한도.
    pub max_continuations: u8,
    /// Some 이면 실행 경로(계획·에이전트 루프·검증·검증자 게이트)의 프로바이더를 이 값으로
    /// 고정한다. 자동 선택(manual_*·sticky·ranks·fallback)을 전부 이기고, 사용자의 명시
    /// 오버라이드(--provider/--model)에만 진다. 일반 메커니즘이므로 config `[engines.*]`
    /// 로 어떤 엔진에든 다른 프로바이더를 고정할 수 있다.
    pub pin_provider: Option<Cow<'static, str>>,
    /// true 면 고정 프로바이더가 전면 장애일 때도 다른 프로바이더로 넘어가지 않는다.
    /// 기본은 false — 고정은 선호이지 가용성 희생이 아니다(설계 §15.2). 폴백이 일어나면
    /// 기존 폴백 경고가 그대로 알린다.
    pub pin_strict: bool,
}

impl EngineSpec {
    /// 고정 프로바이더 — 공백뿐인 값은 고정 없음으로 본다.
    pub fn pin(&self) -> Option<&str> {
        self.pin_provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

/// config `[engines.<name>]` 필드 단위 오버라이드 — 문구·플래그를 코드 수정 없이 튜닝.
/// 알 수 없는 문자열 값은 조용히 무시하고 내장값을 유지한다(설정 오타로 로드 전체가
/// 실패하지 않게).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EngineOverride {
    #[serde(default)]
    pub prompt_block: Option<String>,
    #[serde(default)]
    pub force_staged: Option<bool>,
    /// off | brief | contract
    #[serde(default)]
    pub plan_depth: Option<String>,
    /// inherit | auto | strict
    #[serde(default)]
    pub verify_policy: Option<String>,
    #[serde(default)]
    pub max_continuations: Option<u8>,
    /// 프로바이더 고정. 빈 문자열이면 내장 고정을 해제한다.
    #[serde(default)]
    pub pin_provider: Option<String>,
    /// true 면 고정 프로바이더 전면 장애에도 폴백을 금지한다 (기본 false).
    #[serde(default)]
    pub pin_strict: Option<bool>,
}

impl EngineOverride {
    fn apply(&self, spec: &mut EngineSpec) {
        if let Some(v) = &self.prompt_block {
            spec.prompt_block = Cow::Owned(v.clone());
        }
        if let Some(v) = self.force_staged {
            spec.force_staged = v;
        }
        if let Some(v) = self.plan_depth.as_deref().and_then(PlanDepth::parse) {
            spec.plan_depth = v;
        }
        if let Some(v) = self.verify_policy.as_deref().and_then(VerifyPolicy::parse) {
            spec.verify_policy = v;
        }
        if let Some(v) = self.max_continuations {
            spec.max_continuations = v.max(1);
        }
        if let Some(v) = &self.pin_provider {
            let t = v.trim();
            // 빈 문자열은 고정 해제 — 내장 pin 을 config 로 끌 수 있어야 한다.
            spec.pin_provider = if t.is_empty() {
                None
            } else {
                Some(Cow::Owned(t.to_string()))
            };
        }
        if let Some(v) = self.pin_strict {
            spec.pin_strict = v;
        }
    }
}

/// 기본 엔진 이름 — 미설정·미지원 값이 떨어지는 자리.
pub const DEFAULT_ENGINE: &str = "rafikx";

static CATALOG: &[EngineSpec] = &[
    EngineSpec {
        name: Cow::Borrowed("rafikx"),
        summary: Cow::Borrowed("기본 파이프라인 — 프롬프트 증강 없음"),
        prompt_block: Cow::Borrowed(""),
        force_staged: false,
        plan_depth: PlanDepth::Brief,
        verify_policy: VerifyPolicy::Auto,
        max_continuations: 8,
        pin_provider: None,
        pin_strict: false,
    },
    EngineSpec {
        name: Cow::Borrowed("claude"),
        summary: Cow::Borrowed("계획 가시화 + 탐색 위임 + 검증 우선 (계약형 계획)"),
        prompt_block: Cow::Borrowed(
            "\n\n[claude harness]\n\
             - 착수 즉시 todo_write 로 계획을 가시화하고, 항목 단위로 진행한다. 한 항목을 마칠 때마다 그 자리에서 상태를 갱신한다(MUST).\n\
             - 넓은 탐색(파일 전수 조사·후보 나열·원인 좁히기)은 task 도구로 위임하고, 부모 대화에는 결론과 근거가 되는 파일 경로만 되돌린다. 탐색 로그로 부모 컨텍스트를 채우지 않는다(NEVER).\n\
             - 여러 분야에 걸친 대형 작업은 task 도구의 role 인자로 planner → frontend/backend 순으로 위임한다(role=planner|frontend|backend|reviewer 는 전문가 프로파일로 실행된다).\n\
             - 단계 사이에는 자유 대화가 아니라 구조화 산출물만 주고받는다: planner 는 [완료 기준]/[작업 분해]를, 구현 역할은 [변경 요약]을 돌려준다.\n\
             - 완료를 선언하기 전에 반드시 검증을 실행한다: 빌드·테스트·실제 실행 중 해당하는 것을 돌리고 결과를 관찰한 뒤 보고한다. 검증하지 않은 완료 선언은 금지(NEVER).",
        ),
        force_staged: false,
        plan_depth: PlanDepth::Contract,
        verify_policy: VerifyPolicy::Strict,
        max_continuations: 8,
        pin_provider: None,
        pin_strict: false,
    },
    EngineSpec {
        name: Cow::Borrowed("deepseek"),
        summary: Cow::Borrowed("도구 작업을 항상 단계별(todo)로 · 도구 pre/post 선언"),
        prompt_block: Cow::Borrowed(
            "\n\n[deepseek harness]\n\
             - 각 단계 시작 시 `[N/총M] 단계명` 형식의 한 줄 상태를 출력한다.\n\
             - 도구를 실행하기 전에 무엇을 왜 실행하는지 한 줄로 선언한다(pre). 결과를 받은 뒤에는 무엇을 확인했고 다음이 무엇인지 한 줄로 요약한다(post).\n\
             - 선언한 의도와 실제 결과가 어긋나면 덮어쓰지 말고 그 사실을 먼저 밝힌 뒤 계획을 고친다.\n\
             - 검증 가능한 작업(빌드·테스트·파일 수정)은 마지막에 검증 방법과 결과를 함께 남긴다.",
        ),
        force_staged: true,
        plan_depth: PlanDepth::Brief,
        verify_policy: VerifyPolicy::Auto,
        max_continuations: 8,
        pin_provider: None,
        pin_strict: false,
    },
    EngineSpec {
        name: Cow::Borrowed("qwen"),
        summary: Cow::Borrowed("ReAct 사이클(생각→행동→관찰) 명시 · 검증 자동"),
        prompt_block: Cow::Borrowed(
            "\n\n[qwen harness]\n\
             - 생각 → 행동 → 관찰 사이클을 유지한다. 행동 전에 한 줄로 의도를, 관찰 뒤에 한 줄로 결과 해석을 남긴다.\n\
             - 관찰마다 계획과의 차이를 확인한다. 예상과 다르면 추측으로 덮지 말고 그 자리에서 계획을 조정한다.\n\
             - 반복 작업(여러 파일에 같은 수정)은 첫 건에서 패턴을 확정한 뒤 나머지를 동일 패턴으로 일관 처리한다.\n\
             - 관찰 근거 없이 다음 행동으로 넘어가지 않는다(NEVER).",
        ),
        force_staged: false,
        plan_depth: PlanDepth::Brief,
        verify_policy: VerifyPolicy::Auto,
        max_continuations: 8,
        pin_provider: None,
        pin_strict: false,
    },
    EngineSpec {
        name: Cow::Borrowed("kimi"),
        summary: Cow::Borrowed("성공 루브릭 선언 + 도구 연쇄 유지 (계약형 계획)"),
        prompt_block: Cow::Borrowed(
            "\n\n[kimi harness]\n\
             - 작업 시작 시 성공 루브릭을 명시한다: 무엇이 충족되어야 완료인가(기준), 어떤 도구를 어떤 순서로 쓸 것인가(예상 패턴), 중간 확인 지점은 어디인가(체크포인트).\n\
             - 도구 호출 연쇄를 끊지 않는다. 한 도구의 결과가 다음 호출을 결정하면 사용자에게 되묻지 말고 같은 턴에서 이어 실행한다.\n\
             - 관찰마다 루브릭 대비 현재 위치를 한 줄로 확인한다. 기준에서 벗어났으면 되돌린다.\n\
             - 여러 분야에 걸친 대형 작업은 task 도구의 role 인자로 planner → frontend/backend 순 위임한다(role=planner|frontend|backend|reviewer 는 전문가 프로파일로 실행된다).\n\
             - 단계 사이에는 자유 대화가 아니라 구조화 산출물만 주고받는다: planner 는 [완료 기준]/[작업 분해]를, 구현 역할은 [변경 요약]을 돌려준다.\n\
             - 루브릭의 모든 기준을 충족하기 전에는 완료를 선언하지 않는다(NEVER).",
        ),
        force_staged: false,
        plan_depth: PlanDepth::Contract,
        verify_policy: VerifyPolicy::Strict,
        max_continuations: 8,
        pin_provider: None,
        pin_strict: false,
    },
    EngineSpec {
        name: Cow::Borrowed("pi"),
        summary: Cow::Borrowed("저소음 진행 보고 (oh-my-pi 스타일)"),
        prompt_block: Cow::Borrowed(
            "\n\n[pi harness]\n\
             작업 중에는 지금 수행하는 단계와 도구 결과를 짧고 사실적으로 알리고, \
             최종 답변과 분리하라. 공개 응답에 포함된 추론·진행 텍스트는 숨기지 않는다.",
        ),
        force_staged: false,
        plan_depth: PlanDepth::Brief,
        verify_policy: VerifyPolicy::Auto,
        max_continuations: 8,
        pin_provider: None,
        pin_strict: false,
    },
    EngineSpec {
        name: Cow::Borrowed("minimax"),
        summary: Cow::Borrowed("MiniMax 전용 — 프로바이더 고정 + 약점 보정 + 계약형 계획·검증"),
        prompt_block: Cow::Borrowed(
            "\n\n[minimax harness]\n\
             - 도구를 호출할 때 필수 인자(path 등)를 절대 누락하지 마라(NEVER). 호출 직전에 인자 완전성을 한 번 확인한다.\n\
             - 큰 파일은 한 번에 통째로 쓰지 않는다. 처음부터 골격을 먼저 만들고 분할해서 덧붙여 완성한다.\n\
             - 구현 후 문법 검사에서 멈추지 마라. 핵심 상호작용과 엣지케이스를 실행 관점에서 자체 시뮬레이션으로 점검한다(호출 흐름을 따라가며 상태 변화를 검증).\n\
             - 같은 실패를 같은 방식으로 반복하지 마라(NEVER). 2회 실패하면 접근을 바꾼다.",
        ),
        force_staged: false,
        plan_depth: PlanDepth::Contract,
        verify_policy: VerifyPolicy::Strict,
        max_continuations: 10,
        pin_provider: Some(Cow::Borrowed("minimax")),
        pin_strict: false,
    },
];

/// 내장 엔진 7종.
pub fn catalog() -> &'static [EngineSpec] {
    CATALOG
}

/// 카탈로그 조회 (대소문자 무시). 별칭은 처리하지 않는다 — normalize 를 먼저 통과시킬 것.
pub fn resolve(name: &str) -> Option<&'static EngineSpec> {
    let n = name.trim();
    CATALOG.iter().find(|s| s.name.eq_ignore_ascii_case(n))
}

/// 미지원 이름이 들어와도 항상 사양을 돌려주는 조회 (기본 엔진 폴백).
pub fn resolve_or_default(name: &str) -> &'static EngineSpec {
    resolve(name).unwrap_or_else(|| resolve(DEFAULT_ENGINE).expect("내장 기본 엔진"))
}

/// config `[engines.<name>]` 오버라이드를 병합한 사양.
pub fn resolve_with(overrides: &HashMap<String, EngineOverride>, name: &str) -> EngineSpec {
    let mut spec = resolve_or_default(name).clone();
    if let Some(o) = overrides.get(spec.name.as_ref()) {
        o.apply(&mut spec);
    }
    spec
}

/// 실행 분야 — 같은 엔진 위에서 실행 제어 전략만 바꾼다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discipline {
    /// 기본 파이프라인 (계획 → 목표 continuation → 검증 → 게이트).
    Harness,
    /// 루프 엔지니어링 — continuation 한도 가산 + 정체 시 전략 전환 지시.
    Loop,
    /// 그래프 엔지니어링 — 계획이 낳은 DAG 를 위상순으로 노드마다 독립 실행.
    Graph,
}

impl Discipline {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Harness => "harness",
            Self::Loop => "loop",
            Self::Graph => "graph",
        }
    }

    /// /discipline 목록 한 줄 설명.
    pub fn summary(self) -> &'static str {
        match self {
            Self::Harness => "기본 — 계획·목표 continuation·검증 파이프라인",
            Self::Loop => "루프 강화 — 계속 실행 한도 +4, 정체 시 전략 전환 지시",
            Self::Graph => "그래프 — 계획이 낳은 노드 DAG 를 위상순으로 독립 실행",
        }
    }
}

/// 선택 가능한 분야 3종 (표시 순서).
pub const DISCIPLINES: &[Discipline] = &[Discipline::Harness, Discipline::Loop, Discipline::Graph];

/// loop 분야의 continuation 한도 상한 — 무한 재개를 막는 안전핀.
const LOOP_MAX_CONTINUATIONS: u8 = 12;
/// loop 분야가 엔진 값에 얹는 가산분.
const LOOP_CONTINUATION_BONUS: u8 = 4;

/// config 값 정규화 — 미설정·오타는 기본 분야로 떨어진다.
pub fn normalize_discipline(raw: &str) -> Discipline {
    match raw.trim().to_ascii_lowercase().as_str() {
        "loop" => Discipline::Loop,
        "graph" => Discipline::Graph,
        _ => Discipline::Harness,
    }
}

/// /discipline 표시용 — `harness|loop|graph`.
pub fn discipline_names_joined() -> String {
    DISCIPLINES
        .iter()
        .map(|d| d.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

/// 분야를 반영한 goal continuation 한도. loop 만 엔진 값에 +4 하고 12 로 자른다.
pub fn max_continuations_for(discipline: Discipline, spec_value: u8) -> u8 {
    match discipline {
        Discipline::Loop => spec_value
            .saturating_add(LOOP_CONTINUATION_BONUS)
            .min(LOOP_MAX_CONTINUATIONS),
        _ => spec_value,
    }
}

/// 팀 모드 — 엔진·분야와 직교하는 축. 한 바인딩이 전 과정을 수행할지(single),
/// 계획이 확정된 뒤 독립 단계를 역할 서브에이전트에게 위임할지(multi)를 정한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamMode {
    /// 현행 — 한 바인딩 모델이 전 과정을 수행한다.
    Single,
    /// 계획 확정 후 독립 단계를 task 도구(role 프로파일)로 위임한다. 독립 갈래는 병렬 실행.
    Multi,
}

impl TeamMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multi => "multi",
        }
    }

    /// /team 목록 한 줄 설명.
    pub fn summary(self) -> &'static str {
        match self {
            Self::Single => "기본 — 한 모델이 전 과정을 수행",
            Self::Multi => "팀 — 독립 단계를 역할별 서브에이전트로 위임(연속 위임은 병렬)",
        }
    }
}

/// 선택 가능한 팀 모드 2종 (표시 순서).
pub const TEAM_MODES: &[TeamMode] = &[TeamMode::Single, TeamMode::Multi];

/// config 값 정규화 — 미설정·오타는 single 로 떨어진다.
pub fn normalize_team(raw: &str) -> TeamMode {
    match raw.trim().to_ascii_lowercase().as_str() {
        "multi" => TeamMode::Multi,
        _ => TeamMode::Single,
    }
}

/// /team 표시용 — `single|multi`.
pub fn team_names_joined() -> String {
    TEAM_MODES
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

/// config 값 정규화 → (엔진 이름, legacy self 플래그).
/// - 빈 값·미지원 값 → 기본 엔진
/// - `dk` → `deepseek` (제거된 옛 값 흡수)
/// - `self` → 기본 엔진 + Self-Harness 메타 레이어 on (하위호환)
pub fn normalize(raw: &str) -> (String, bool) {
    let e = raw.trim().to_ascii_lowercase();
    match e.as_str() {
        "" => (DEFAULT_ENGINE.to_string(), false),
        "dk" => ("deepseek".to_string(), false),
        "self" => (DEFAULT_ENGINE.to_string(), true),
        other => match resolve(other) {
            Some(spec) => (spec.name.to_string(), false),
            None => (DEFAULT_ENGINE.to_string(), false),
        },
    }
}

/// /engine 목록 표시용 — `rafikx|claude|deepseek|qwen|kimi|pi|minimax`.
pub fn names_joined() -> String {
    CATALOG
        .iter()
        .map(|s| s.name.as_ref())
        .collect::<Vec<_>>()
        .join("|")
}

/// /engine 입력으로 받아들일 값인지 — 카탈로그 7종 + legacy `self`.
/// `dk`·`dkharness` 같은 제거된 값은 거부한다(normalize 만 흡수).
pub fn is_selectable(name: &str) -> bool {
    let n = name.trim();
    n.eq_ignore_ascii_case("self") || resolve(n).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_maps_legacy_values() {
        assert_eq!(normalize("dk"), ("deepseek".into(), false));
        assert_eq!(normalize("DK"), ("deepseek".into(), false));
        assert_eq!(normalize("self"), ("rafikx".into(), true));
        assert_eq!(normalize("SELF"), ("rafikx".into(), true));
        assert_eq!(normalize(""), ("rafikx".into(), false));
        assert_eq!(normalize("   "), ("rafikx".into(), false));
        // 미지원 값은 기본 엔진으로 떨어진다.
        assert_eq!(normalize("dkharness"), ("rafikx".into(), false));
        assert_eq!(normalize("gpt"), ("rafikx".into(), false));
        // 카탈로그 7종은 이름 그대로 (대소문자 무시).
        for name in [
            "rafikx", "claude", "deepseek", "qwen", "kimi", "pi", "minimax",
        ] {
            assert_eq!(normalize(name), (name.to_string(), false));
            assert_eq!(normalize(&name.to_uppercase()), (name.to_string(), false));
        }
    }

    #[test]
    fn catalog_specs_match_design_table() {
        assert_eq!(catalog().len(), 7);
        let deep = resolve("deepseek").expect("deepseek");
        assert!(deep.force_staged);
        assert_eq!(deep.plan_depth, PlanDepth::Brief);
        let claude = resolve("claude").expect("claude");
        assert_eq!(claude.plan_depth, PlanDepth::Contract);
        assert_eq!(claude.verify_policy, VerifyPolicy::Strict);
        assert!(!claude.force_staged);
        let kimi = resolve("kimi").expect("kimi");
        assert_eq!(kimi.plan_depth, PlanDepth::Contract);
        assert_eq!(kimi.verify_policy, VerifyPolicy::Strict);
        let qwen = resolve("qwen").expect("qwen");
        assert_eq!(qwen.verify_policy, VerifyPolicy::Auto);
        // 기본 엔진은 증강하지 않는다.
        assert!(resolve("rafikx").expect("rafikx").prompt_block.is_empty());
        // 나머지는 모두 프롬프트 블록을 갖는다.
        for spec in catalog().iter().filter(|s| s.name != "rafikx") {
            assert!(!spec.prompt_block.is_empty(), "{} 블록 없음", spec.name);
        }
        // 계약형 계획 엔진은 전문가 멀티롤 위임을 지시한다 (MetaGPT 산출물 계약).
        for spec in [claude, kimi] {
            assert!(
                spec.prompt_block.contains("task 도구의 role"),
                "{} 에 위임 지침 없음",
                spec.name
            );
            assert!(spec.prompt_block.contains("planner"));
            assert!(spec.prompt_block.contains("[변경 요약]"));
        }
        assert!(resolve("없는엔진").is_none());
    }

    #[test]
    fn minimax_engine_pins_provider_and_corrects_weaknesses() {
        let mm = resolve("minimax").expect("minimax");
        assert_eq!(mm.plan_depth, PlanDepth::Contract);
        assert_eq!(mm.verify_policy, VerifyPolicy::Strict);
        assert!(
            !mm.force_staged,
            "Contract 의 todo 시드가 단계화를 담당한다"
        );
        assert_eq!(mm.max_continuations, 10);
        assert_eq!(mm.pin(), Some("minimax"));
        assert!(is_selectable("minimax"));
        // 관찰된 약점(인자 누락·통짜 쓰기·문법 검사에서 멈춤·같은 실패 반복)의 정면 보정.
        for key in ["필수 인자", "골격", "시뮬레이션", "2회 실패"] {
            assert!(mm.prompt_block.contains(key), "{key} 지시 없음");
        }
        // 고정은 minimax 만 — 나머지 내장 엔진은 자동 선택을 그대로 쓴다.
        for spec in catalog().iter().filter(|s| s.name != "minimax") {
            assert_eq!(spec.pin(), None, "{} 에 고정이 걸려 있다", spec.name);
        }
    }

    #[test]
    fn config_override_sets_and_clears_pin_provider() {
        // 어떤 엔진에든 프로바이더를 고정할 수 있는 일반 메커니즘이다.
        let mut set: HashMap<String, EngineOverride> = HashMap::new();
        set.insert(
            "claude".into(),
            EngineOverride {
                pin_provider: Some("  glm  ".into()),
                ..EngineOverride::default()
            },
        );
        assert_eq!(resolve_with(&set, "claude").pin(), Some("glm"));

        // 빈 문자열은 내장 고정을 해제한다.
        let mut clear: HashMap<String, EngineOverride> = HashMap::new();
        clear.insert(
            "minimax".into(),
            EngineOverride {
                pin_provider: Some("".into()),
                ..EngineOverride::default()
            },
        );
        let spec = resolve_with(&clear, "minimax");
        assert_eq!(spec.pin(), None);
        // 다른 필드는 그대로.
        assert_eq!(spec.plan_depth, PlanDepth::Contract);
        assert_eq!(spec.max_continuations, 10);

        // 지정하지 않으면 내장 고정 유지.
        let untouched = resolve_with(&HashMap::new(), "minimax");
        assert_eq!(untouched.pin(), Some("minimax"));
        // 고정은 기본적으로 가용성에 양보한다 — 폴백 금지는 명시해야 켜진다 (§15.2).
        assert!(!untouched.pin_strict);

        let mut strict: HashMap<String, EngineOverride> = HashMap::new();
        strict.insert(
            "minimax".into(),
            EngineOverride {
                pin_strict: Some(true),
                ..EngineOverride::default()
            },
        );
        let spec = resolve_with(&strict, "minimax");
        assert!(spec.pin_strict);
        assert_eq!(spec.pin(), Some("minimax"));
    }

    #[test]
    fn config_override_merges_field_by_field() {
        let mut overrides: HashMap<String, EngineOverride> = HashMap::new();
        overrides.insert(
            "claude".into(),
            EngineOverride {
                prompt_block: Some("사내 지침만 따른다.".into()),
                plan_depth: Some("brief".into()),
                max_continuations: Some(3),
                ..EngineOverride::default()
            },
        );
        let spec = resolve_with(&overrides, "claude");
        assert_eq!(spec.prompt_block, "사내 지침만 따른다.");
        assert_eq!(spec.plan_depth, PlanDepth::Brief);
        assert_eq!(spec.max_continuations, 3);
        // 지정하지 않은 필드는 내장값 유지.
        assert_eq!(spec.verify_policy, VerifyPolicy::Strict);
        assert!(!spec.force_staged);

        // 오버라이드가 없는 엔진은 내장값 그대로.
        let untouched = resolve_with(&overrides, "deepseek");
        assert_eq!(&untouched, resolve("deepseek").expect("deepseek"));

        // 알 수 없는 문자열 값은 무시하고 내장값을 지킨다.
        let mut bad: HashMap<String, EngineOverride> = HashMap::new();
        bad.insert(
            "kimi".into(),
            EngineOverride {
                plan_depth: Some("깊게".into()),
                verify_policy: Some("느슨".into()),
                max_continuations: Some(0),
                ..EngineOverride::default()
            },
        );
        let spec = resolve_with(&bad, "kimi");
        assert_eq!(spec.plan_depth, PlanDepth::Contract);
        assert_eq!(spec.verify_policy, VerifyPolicy::Strict);
        assert_eq!(spec.max_continuations, 1);

        // 미지원 이름은 기본 엔진으로 폴백.
        assert_eq!(resolve_with(&overrides, "없는엔진").name, "rafikx");
    }

    #[test]
    fn discipline_normalizes_and_scales_continuations() {
        assert_eq!(normalize_discipline(""), Discipline::Harness);
        assert_eq!(normalize_discipline("  "), Discipline::Harness);
        assert_eq!(normalize_discipline("harness"), Discipline::Harness);
        assert_eq!(normalize_discipline("LOOP"), Discipline::Loop);
        assert_eq!(normalize_discipline(" Graph "), Discipline::Graph);
        // 오타·미지원 값은 기본 분야로 떨어진다 (설정 오류로 실행이 막히지 않게).
        assert_eq!(normalize_discipline("dag"), Discipline::Harness);
        assert_eq!(discipline_names_joined(), "harness|loop|graph");
        for d in DISCIPLINES {
            assert!(!d.summary().is_empty());
        }
    }

    #[test]
    fn loop_discipline_adds_four_continuations_up_to_twelve() {
        // 내장 엔진 값은 전부 8 — loop 는 +4 로 12.
        for spec in catalog() {
            assert_eq!(
                max_continuations_for(Discipline::Loop, spec.max_continuations),
                12
            );
            assert_eq!(
                max_continuations_for(Discipline::Harness, spec.max_continuations),
                spec.max_continuations
            );
            assert_eq!(
                max_continuations_for(Discipline::Graph, spec.max_continuations),
                spec.max_continuations
            );
        }
        assert_eq!(max_continuations_for(Discipline::Loop, 3), 7);
        // 상한 12 를 넘지 않는다 (오버라이드가 큰 값을 줘도).
        assert_eq!(max_continuations_for(Discipline::Loop, 10), 12);
        assert_eq!(max_continuations_for(Discipline::Loop, 250), 12);
        // harness 는 오버라이드 값을 그대로 쓴다.
        assert_eq!(max_continuations_for(Discipline::Harness, 250), 250);
    }

    #[test]
    fn team_normalizes_to_single_unless_multi() {
        assert_eq!(normalize_team(""), TeamMode::Single);
        assert_eq!(normalize_team("   "), TeamMode::Single);
        assert_eq!(normalize_team("single"), TeamMode::Single);
        assert_eq!(normalize_team("MULTI"), TeamMode::Multi);
        assert_eq!(normalize_team("  multi  "), TeamMode::Multi);
        // 오타·미지원 값은 조용히 single — 설정 오류로 실행이 막히지 않게.
        assert_eq!(normalize_team("team"), TeamMode::Single);
        assert_eq!(normalize_team("multi-agent"), TeamMode::Single);
        assert_eq!(team_names_joined(), "single|multi");
        for t in TEAM_MODES {
            assert!(!t.summary().is_empty());
        }
    }

    #[test]
    fn selectable_accepts_catalog_and_legacy_self_only() {
        for name in [
            "rafikx", "claude", "deepseek", "qwen", "kimi", "pi", "minimax", "self",
        ] {
            assert!(is_selectable(name), "{name} 가 거부됨");
        }
        assert!(!is_selectable("dk"));
        assert!(!is_selectable("dkharness"));
        assert!(!is_selectable(""));
        assert_eq!(
            names_joined(),
            "rafikx|claude|deepseek|qwen|kimi|pi|minimax"
        );
    }
}
