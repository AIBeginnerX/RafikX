use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::agent::{self, AgentOutcome, AgentRun};
use crate::config::{Config, ProviderConfig};
use crate::db::Db;
use crate::provider::{
    AnthropicProvider, ChatRequest, ChatResponse, ContentBlock, DynProvider, Message,
    OpenAiCompatProvider, StopReason, is_rate_limited, is_retryable,
};
use crate::run::{RunContext, RunId, TerminalState};
use crate::tools::{self, ToolCtx, ToolRegistry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskClass {
    Simple,
    Medium,
    Advanced,
    Dev,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessStrategy {
    Single,
    Multi,
}

impl HarnessStrategy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "single" => Some(Self::Single),
            "multi" => Some(Self::Multi),
            _ => None,
        }
    }
}

impl TaskClass {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskClass::Simple => "simple",
            TaskClass::Medium => "medium",
            TaskClass::Advanced => "advanced",
            TaskClass::Dev => "dev",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "simple" => Some(TaskClass::Simple),
            "medium" => Some(TaskClass::Medium),
            "advanced" => Some(TaskClass::Advanced),
            "dev" => Some(TaskClass::Dev),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct Binding {
    pub class: TaskClass,
    pub profile_name: String,
    pub provider_name: String,
    pub model: String,
    pub kind: String,
    pub tools: Vec<String>,
    pub max_iterations: u32,
    pub plan_first: bool,
    pub verify: bool,
    pub verify_command: String,
    pub system_extra: String,
    pub context_window: u32,
    pub verify_model: Option<String>,
}

pub fn classify_rules(text: &str, obsidian: bool) -> TaskClass {
    if looks_like_dev(text) {
        return TaskClass::Dev;
    }
    if looks_like_advanced(text) {
        return TaskClass::Advanced;
    }
    // obsidian 플래그는 컨텍스트 주입 여부일 뿐 — 인사말까지 medium 으로 올리지 않는다.
    let _ = obsidian;
    let n = text.chars().count();
    if (150..=600).contains(&n) {
        return TaskClass::Medium;
    }
    if contains_any(
        text,
        &[
            "요약",
            "정리",
            "번역",
            "초안",
            "검색",
            "찾아",
            "노트",
            "문서",
            "파일",
            "마크다운",
            "폴더",
            "디렉토리",
            "워크스페이스",
        ],
    ) {
        return TaskClass::Medium;
    }
    TaskClass::Simple
}

fn looks_like_dev(text: &str) -> bool {
    if text.contains("```") {
        return true;
    }
    let exts = [
        ".rs", ".py", ".js", ".ts", ".tsx", ".jsx", ".toml", ".json", ".go", ".java", ".c", ".cpp",
        ".h", ".cs", ".rb", ".php", ".kt", ".swift", ".sh", ".ps1", ".md", ".yml", ".yaml",
    ];
    if exts.iter().any(|e| text.contains(e)) {
        return true;
    }
    contains_any(
        text,
        &[
            "코드",
            "구현",
            "수정해",
            "고쳐",
            "버그",
            "디버그",
            "디버깅",
            "검증",
            "컴파일",
            "빌드",
            "리팩터",
            "테스트 작성",
            "스크립트",
            "함수",
            "에러 잡아",
            "업그레이드",
            "만들어",
            "생성해",
            "작성해",
            "적용해",
        ],
    )
}

fn looks_like_advanced(text: &str) -> bool {
    if text.chars().count() > 600 {
        return true;
    }
    if list_item_count(text) >= 3 {
        return true;
    }
    contains_any(
        text,
        &[
            "설계",
            "아키텍처",
            "분석",
            "전략",
            "비교 평가",
            "보고서",
            "계획 수립",
            "검토",
            "구성",
        ],
    )
}

fn list_item_count(text: &str) -> usize {
    text.lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("- ")
                || t.starts_with("* ")
                || t.starts_with("• ")
                || t.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() && t.contains('.'))
        })
        .count()
}

fn contains_any(text: &str, kws: &[&str]) -> bool {
    kws.iter().any(|k| text.contains(k))
}

pub fn profile_name_for(cfg: &Config, class: TaskClass) -> &str {
    match class {
        TaskClass::Simple => cfg.file.harness.simple.as_str(),
        TaskClass::Medium => cfg.file.harness.medium.as_str(),
        TaskClass::Advanced => cfg.file.harness.advanced.as_str(),
        TaskClass::Dev => cfg.file.harness.dev.as_str(),
    }
}

/// 프로파일 사양 조회 — config `[subagents.<name>]` 이 내장 전문가 프리셋을 이긴다.
/// (기존 사용자 config 에 planner/reviewer 가 없어도 동작하게 하는 폴백.)
pub fn resolve_profile(cfg: &Config, name: &str) -> Option<crate::config::SubAgentConfig> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    cfg.file
        .subagents
        .get(n)
        .cloned()
        .or_else(|| crate::config::builtin_profile(n))
}

/// config `[subagents]` 또는 내장 프리셋에 있는 프로파일 이름인지.
pub fn profile_exists(cfg: &Config, name: &str) -> bool {
    resolve_profile(cfg, name).is_some()
}

pub fn bind(
    cfg: &Config,
    class: TaskClass,
    provider_override: Option<&str>,
    model_override: Option<&str>,
) -> Result<Binding> {
    bind_profile(cfg, class, None, provider_override, model_override)
}

/// 프로파일을 직접 지정하는 bind — 전문가 역할 위임(task role)과 독립 검증자 게이트가 쓴다.
/// `profile_override` 가 없으면 분류가 가리키는 config 프로파일을 쓴다.
/// config 에 이름이 없으면 내장 프리셋(planner/frontend/backend/reviewer)으로 폴백한다.
pub fn bind_profile(
    cfg: &Config,
    class: TaskClass,
    profile_override: Option<&str>,
    provider_override: Option<&str>,
    model_override: Option<&str>,
) -> Result<Binding> {
    let profile_name = profile_override
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| profile_name_for(cfg, class).to_string());
    let sub = resolve_profile(cfg, &profile_name)
        .ok_or_else(|| anyhow!("서브에이전트 '{profile_name}' 이(가) config에 없습니다"))?;
    let sub = &sub;

    let needs_tools = !sub.tools.is_empty();
    let selection = cfg.file.harness.selection.trim().to_ascii_lowercase();
    let manual = selection == "manual";
    let strategy =
        HarnessStrategy::parse(&cfg.file.harness.strategy).unwrap_or(HarnessStrategy::Single);

    let (provider_name, model, verify_model) = if let (Some(p), Some(m)) =
        (provider_override, model_override)
    {
        // 직접 지정 조합도 도구 지원 여부를 검사한다 — 미지원 모델로 코딩 프로필을
        // 돌리면 답변이 비정상(도구 호출 불가·빈 응답)으로 나온다.
        ensure_connected(cfg, p)?;
        let pc = cfg.provider(p)?;
        let needs_tools = !sub.tools.is_empty();
        if needs_tools && !pc.supports_tools {
            anyhow::bail!(
                "'{p}' 연결은 도구를 지원하지 않습니다. 선택한 모델 '{m}' 은(는) 코딩 작업에 쓸 수 없습니다."
            );
        }
        crate::applog::debug(&format!(
            "bind: direct pair {p}/{m} tools_ok={}",
            pc.supports_tools
        ));
        (p.to_string(), m.to_string(), None)
    } else if let Some(m) = model_override {
        let p = provider_override
            .map(|s| s.to_string())
            .or_else(|| provider_for_model(cfg, m))
            .unwrap_or_else(|| sub.provider.clone());
        (p, m.to_string(), None)
    } else if let Some(p) = provider_override {
        ensure_connected(cfg, p)?;
        let pc = cfg.provider(p)?;
        if needs_tools && !pc.supports_tools {
            anyhow::bail!("'{p}' 는 도구를 지원하지 않습니다. 다른 연결을 고르세요.");
        }
        let model = pick_for_provider(cfg, p, class, &sub.model_role, needs_tools)
            .unwrap_or_else(|| model_for_role(pc, &sub.model_role));
        (p.to_string(), model, None)
    } else if manual {
        pick_manual(cfg, class, sub, needs_tools)?
    } else if strategy == HarnessStrategy::Single {
        pick_single(cfg, needs_tools)?
    } else {
        pick_auto(cfg, class, sub, needs_tools)?
    };

    let p = cfg.provider(&provider_name)?;
    if !crate::auth::is_usable(cfg, &provider_name)
        && crate::auth::auth_mode(&provider_name, p) != "none"
    {
        if crate::auth::is_connected(cfg, &provider_name)
            && !crate::auth::is_enabled(cfg, &provider_name)
        {
            anyhow::bail!(
                "'{provider_name}' 는 사용 중지입니다. rafikx settings 에서 다시 켜세요."
            );
        }
        anyhow::bail!("'{provider_name}' 연결이 없습니다. rafikx settings 에서 번호로 연결하세요.");
    }

    let window = crate::packer::context_window_for(&provider_name, &model, Some(p));
    Ok(Binding {
        class,
        profile_name,
        provider_name,
        model,
        kind: p.kind.clone(),
        tools: sub.tools.clone(),
        max_iterations: {
            let n = if sub.max_iterations == 0 {
                agent::AGENT_MAX_ITER
            } else {
                sub.max_iterations
            };
            n.min(agent::HARD_CAP)
        },
        plan_first: sub.plan_first,
        verify: sub.verify,
        verify_command: sub.verify_command.clone(),
        system_extra: sub.system_extra.clone(),
        context_window: window,
        verify_model,
    })
}

// ---------------------------------------------------------------------------
// 엔진 프로바이더 고정 (EngineSpec::pin_provider) — 배선은 이 구역 한 곳에만 둔다.
// 진입점(chat·cli·telegram·task)은 bind 직후 apply_engine_pin 만 부르면 된다.
// ---------------------------------------------------------------------------

/// 현재 엔진이 고정한 프로바이더 — `[general] engine` + `[engines.*]` 오버라이드 결과.
pub fn engine_pin(cfg: &Config) -> Option<String> {
    let (engine, _) = crate::engine::normalize(&cfg.file.general.engine);
    let spec = crate::engine::resolve_with(&cfg.file.engines, &engine);
    spec.pin().map(str::to_string)
}

/// 고정 판정 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinDecision {
    /// 고정할 것이 없다 — 엔진에 고정이 없거나 이미 그 프로바이더로 묶여 있다.
    Keep,
    /// 이 프로바이더로 다시 묶는다.
    Apply(String),
    /// 사용자가 직접 지정했으므로 고정을 양보한다 (경고 한 줄).
    Yield { pin: String, explicit: String },
}

/// 고정 우선순위 판정 (순수 함수).
/// 자동 선택(sticky 재사용·manual_*·ranks·프로파일 기본)은 고정에 지고,
/// 사용자의 명시 오버라이드(--provider / --model)는 고정을 이긴다.
pub fn decide_pin(
    pin: Option<&str>,
    current_provider: &str,
    explicit_provider: Option<&str>,
    explicit_model: Option<&str>,
) -> PinDecision {
    let Some(pin) = pin.map(str::trim).filter(|p| !p.is_empty()) else {
        return PinDecision::Keep;
    };
    // 이미 고정 프로바이더로 묶여 있으면 다툴 것이 없다 (sticky 가 고정과 같은 경우 포함).
    if current_provider.eq_ignore_ascii_case(pin) {
        return PinDecision::Keep;
    }
    let explicit = explicit_provider
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|p| format!("provider={p}"))
        .or_else(|| {
            // 모델만 직접 고른 경우도 사용자 의지다 — 그 모델이 없는 프로바이더로 끌고 가지 않는다.
            explicit_model
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|m| format!("model={m}"))
        });
    match explicit {
        Some(explicit) => PinDecision::Yield {
            pin: pin.to_string(),
            explicit,
        },
        None => PinDecision::Apply(pin.to_string()),
    }
}

/// 고정 프로바이더를 쓸 수 없는 이유 — 쓸 수 있으면 None.
/// 고정이 가용성을 해치면 안 되므로(기존 capability binding 철학) 여기서 걸리면 고정을 포기한다.
fn pin_unavailable(cfg: &Config, pin: &str, needs_tools: bool) -> Option<&'static str> {
    let Ok(p) = cfg.provider(pin) else {
        return Some("config 에 연결이 없음");
    };
    if !crate::auth::is_usable(cfg, pin) {
        return Some("연결되지 않음");
    }
    if needs_tools && !p.supports_tools {
        return Some("도구 미지원");
    }
    None
}

/// 엔진 고정을 바인딩에 적용한다 — 실행 경로 진입점이 bind 직후 한 번 호출한다.
/// 반환값은 사용자에게 보여줄 경고 한 줄(없으면 None).
pub fn apply_engine_pin(
    cfg: &Config,
    binding: &mut Binding,
    explicit_provider: Option<&str>,
    explicit_model: Option<&str>,
) -> Option<String> {
    let pin = engine_pin(cfg)?;
    let (engine, _) = crate::engine::normalize(&cfg.file.general.engine);
    match decide_pin(
        Some(&pin),
        &binding.provider_name,
        explicit_provider,
        explicit_model,
    ) {
        PinDecision::Keep => None,
        PinDecision::Yield { pin, explicit } => Some(format!(
            "엔진 {engine} 은 {pin} 고정이지만 직접 지정한 {explicit} 을(를) 따릅니다."
        )),
        PinDecision::Apply(pin) => {
            if let Some(reason) = pin_unavailable(cfg, &pin, !binding.tools.is_empty()) {
                return Some(format!(
                    "엔진 {engine} 의 {pin} 고정을 건너뜁니다({reason}). {} 로 실행합니다.",
                    binding.provider_name
                ));
            }
            // 모델은 고정 프로바이더의 model_role 규칙으로 다시 해석한다.
            match bind_profile(
                cfg,
                binding.class,
                Some(&binding.profile_name),
                Some(&pin),
                None,
            ) {
                Ok(pinned) => {
                    crate::applog::debug(&format!(
                        "engine pin: {engine} → {}/{}",
                        pinned.provider_name, pinned.model
                    ));
                    binding.provider_name = pinned.provider_name;
                    binding.model = pinned.model;
                    binding.kind = pinned.kind;
                    binding.context_window = pinned.context_window;
                    // 고정 실행은 manual_verify 를 따르지 않는다 (§11.2).
                    binding.verify_model = pinned.verify_model;
                    None
                }
                Err(e) => Some(format!("엔진 {engine} 의 {pin} 고정 실패({e}).")),
            }
        }
    }
}

fn pick_single(cfg: &Config, needs_tools: bool) -> Result<(String, String, Option<String>)> {
    // Single 모드 계약: 사용자가 지정한 기본 연결(default_provider) 하나만 쓴다.
    // (/engine single <연결> 이 이 값을 저장한다.)
    let default = cfg.file.general.default_provider.clone();
    if crate::auth::is_usable(cfg, &default)
        && let Ok(provider) = cfg.provider(&default)
        && (!needs_tools || provider.supports_tools)
    {
        return Ok((default, provider.model.clone(), None));
    }
    let registered = crate::auth::registered_models(cfg);
    if let Some(model) = registered.iter().find(|item| {
        crate::ranks::normalize_id(&item.id) == "minimax-m3"
            && (!needs_tools
                || cfg
                    .provider(&item.provider)
                    .map(|provider| provider.supports_tools)
                    .unwrap_or(false))
    }) {
        return Ok((model.provider.clone(), model.id.clone(), None));
    }
    let fallback = registered.into_iter().find(|item| {
        !needs_tools
            || cfg
                .provider(&item.provider)
                .map(|provider| provider.supports_tools)
                .unwrap_or(false)
    });
    fallback
        .map(|model| (model.provider, model.id, None))
        .ok_or_else(|| {
            anyhow!("사용 가능한 단일 모델 연결이 없습니다. /connect 로 모델을 연결하세요.")
        })
}

pub fn set_strategy(cfg: &Config, strategy: HarnessStrategy) -> Result<()> {
    let value = match strategy {
        HarnessStrategy::Single => "single",
        HarnessStrategy::Multi => "multi",
    };
    crate::config::write_toml_key(
        &cfg.path,
        "[harness]",
        "strategy",
        &crate::config::toml_string(value),
    )
}

fn ensure_connected(cfg: &Config, name: &str) -> Result<()> {
    if crate::auth::is_usable(cfg, name) {
        Ok(())
    } else if crate::auth::is_connected(cfg, name) {
        Err(anyhow!(
            "'{name}' 는 사용 중지입니다. rafikx settings 에서 다시 켜세요."
        ))
    } else {
        Err(anyhow!(
            "'{name}' 연결이 없습니다. rafikx settings 에서 번호로 연결하세요."
        ))
    }
}

fn provider_for_model(cfg: &Config, model: &str) -> Option<String> {
    crate::auth::registered_models(cfg)
        .into_iter()
        .find(|r| r.id == model)
        .map(|r| r.provider)
}

fn parse_manual_spec(spec: &str) -> (Option<String>, String) {
    let t = spec.trim();
    if let Some((p, m)) = t.split_once(':') {
        if !p.is_empty() && !m.is_empty() && !p.contains('/') {
            return (Some(p.to_string()), m.to_string());
        }
    }
    (None, t.to_string())
}

fn resolve_spec(cfg: &Config, spec: &str, needs_tools: bool) -> Result<(String, String)> {
    let (p, m) = parse_manual_spec(spec);
    if let Some(p) = p {
        ensure_connected(cfg, &p)?;
        let pc = cfg.provider(&p)?;
        if needs_tools && !pc.supports_tools {
            anyhow::bail!("'{p}' 는 도구를 지원하지 않습니다.");
        }
        return Ok((p, m));
    }
    if let Some(r) = crate::auth::registered_models(cfg)
        .into_iter()
        .find(|r| r.id == m)
    {
        return Ok((r.provider, r.id));
    }
    Err(anyhow!(
        "수동 모델 '{spec}' 을(를) 등록된 연결에서 찾지 못했습니다. rafikx settings 에서 다시 고르세요."
    ))
}

fn pick_manual(
    cfg: &Config,
    class: TaskClass,
    sub: &crate::config::SubAgentConfig,
    needs_tools: bool,
) -> Result<(String, String, Option<String>)> {
    let h = &cfg.file.harness;
    // 분류별 수동 지정 — 비어 있으면 자동으로 폴백 (사용자 요구: 기본 자동, 수동 선택 시 수동).
    let spec = match class {
        TaskClass::Simple => h.manual_simple.as_deref().filter(|s| !s.is_empty()),
        TaskClass::Medium => h.manual_medium.as_deref().filter(|s| !s.is_empty()),
        TaskClass::Advanced => h.manual_design.as_deref().filter(|s| !s.is_empty()),
        TaskClass::Dev => h.manual_debug.as_deref().filter(|s| !s.is_empty()),
    }
    .or(h.manual_model.as_deref().filter(|s| !s.is_empty()));
    let verify_spec = h.manual_verify.as_deref().filter(|s| !s.is_empty());
    let verify_model = if let Some(vs) = verify_spec {
        resolve_spec(cfg, vs, needs_tools).ok().map(|(_, m)| m)
    } else {
        None
    };
    if let Some(spec) = spec {
        let (p, m) = resolve_spec(cfg, spec, needs_tools)?;
        return Ok((p, m, verify_model));
    }
    pick_auto(cfg, class, sub, needs_tools).map(|(p, m, _)| (p, m, verify_model))
}

/// 분류 → 수동 모델 설정 키. 관리자 UI/CLI 가 이 이름으로 config 에 쓴다.
pub fn manual_key_for(class: TaskClass) -> &'static str {
    match class {
        TaskClass::Simple => "manual_simple",
        TaskClass::Medium => "manual_medium",
        TaskClass::Advanced => "manual_design",
        TaskClass::Dev => "manual_debug",
    }
}

/// 하네스 선정 모드 저장 ("auto" | "manual").
pub fn set_selection_mode(cfg: &Config, mode: &str) -> Result<()> {
    let m = if mode.eq_ignore_ascii_case("manual") {
        "manual"
    } else {
        "auto"
    };
    crate::config::write_toml_key(
        &cfg.path,
        "[harness]",
        "selection",
        &crate::config::toml_string(m),
    )
}

/// 분류별 수동 모델 지정. 빈 spec 이면 해당 분류의 수동 지정을 지운다(자동 폴백).
pub fn set_manual_model(cfg: &Config, class: TaskClass, spec: &str) -> Result<String> {
    let key = manual_key_for(class);
    if spec.trim().is_empty() {
        // 값 제거: auto 로 덮어쓰고 주석 처리된 형태가 되지 않게 빈 문자열로 둔다.
        crate::config::write_toml_key(
            &cfg.path,
            "[harness]",
            key,
            &crate::config::toml_string(""),
        )?;
        return Ok(format!("{} 수동 지정 해제 (자동 사용)", class.as_str()));
    }
    // 지정 형식 검증 — 연결된 프로바이더에서 찾을 수 있어야 한다.
    let needs_tools = cfg
        .file
        .subagents
        .get(profile_name_for(cfg, class))
        .map(|s| !s.tools.is_empty())
        .unwrap_or(false);
    let (p, m) = resolve_spec(cfg, spec.trim(), needs_tools)?;
    crate::config::write_toml_key(
        &cfg.path,
        "[harness]",
        key,
        &crate::config::toml_string(spec.trim()),
    )?;
    Ok(format!("{} 수동 모델: {} / {}", class.as_str(), p, m))
}

/// /engine single <연결> — 지정한 하나의 연결만 쓰도록 저장한다.
/// default_provider 를 바꾸고 selection=auto, strategy=single 로 되돌린다.
pub fn set_single_provider(cfg: &Config, provider: &str) -> Result<String> {
    ensure_connected(cfg, provider)?;
    crate::accounts_ui::set_default_provider(cfg, provider)?;
    set_selection_mode(cfg, "auto")?;
    set_strategy(cfg, HarnessStrategy::Single)?;
    let model = cfg
        .provider(provider)
        .map(|p| p.model.clone())
        .unwrap_or_default();
    Ok(format!(
        "Provider mode: single — 모든 작업을 {provider} / {model} 하나로 처리합니다."
    ))
}

/// /engine multi — 역할별 모델 자동 배정 후보.
struct RoleCandidate {
    provider: String,
    id: String,
    score: i32,
    cheap: bool,
    tools: bool,
}

/// /engine multi — 등록된 연결마다 사용 가능한 모델을 원격 조회해 가져오고,
/// 모델 순위표(ranks)로 점수화한 뒤 각 역할에 가장 알맞은 모델을 미리 배정한다.
/// 배정 결과는 [harness] manual_simple/medium/design/debug/verify 에
/// "provider:model" 로 고정되고 selection=manual, strategy=multi 로 저장된다.
pub async fn auto_assign_roles(cfg: &Config) -> Result<Vec<String>> {
    let names = crate::auth::usable_names(cfg);
    if names.is_empty() {
        anyhow::bail!("연결된 서비스가 없습니다. rafikx settings 에서 먼저 연결하세요.");
    }
    let table = crate::ranks::load();
    let mut pool: Vec<RoleCandidate> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    notes.push(format!("Provider {}곳의 모델을 조회합니다…", names.len()));

    for name in &names {
        let Ok(p) = cfg.provider(name) else { continue };
        let tools = p.supports_tools;
        let remote = tokio::time::timeout(
            Duration::from_secs(12),
            crate::auth::list_remote_models(cfg, name),
        )
        .await;
        let (ids, source) = match remote {
            Ok(Ok(list)) if !list.is_empty() => (list, "원격"),
            _ => {
                // 원격 조회 실패·빈 목록 → 그 연결의 등록/기본 모델로 폴백.
                let mut ids: Vec<String> = crate::auth::registered_models(cfg)
                    .into_iter()
                    .filter(|r| &r.provider == name)
                    .map(|r| r.id)
                    .collect();
                if ids.is_empty() {
                    ids.push(p.model.clone());
                    if let Some(small) = &p.small_model {
                        ids.push(small.clone());
                    }
                }
                (ids, "등록")
            }
        };
        let mut added = 0usize;
        for id in ids {
            if id.trim().is_empty() {
                continue;
            }
            // ':batch'·':free' 같은 라우터 변형은 대화형 코딩에 부적합(지연·제한).
            // 기본 변형만 후보로 쓴다. (수동 지정 형식 'provider:model' 과의
            // 혼동도 함께 피한다.)
            if id.contains(':') {
                continue;
            }
            let norm = crate::ranks::normalize_id(&id);
            if pool
                .iter()
                .any(|c| &c.provider == name && crate::ranks::normalize_id(&c.id) == norm)
            {
                continue;
            }
            // 순위표 미등재 모델은 잡음(embedding·tts 등)일 수 있어 제외하되,
            // 그 연결의 기본 모델은 점수 0 후보로 유지한다.
            let score = match crate::ranks::score_of(&table, &id) {
                Some(s) => s,
                None if id == p.model || Some(&id) == p.small_model.as_ref() => 0,
                None => continue,
            };
            pool.push(RoleCandidate {
                provider: name.clone(),
                id: id.clone(),
                score,
                cheap: crate::ranks::is_cheap_id(&id),
                tools,
            });
            added += 1;
        }
        notes.push(format!("  {name}: {source} 목록에서 후보 {added}개"));
    }
    if pool.is_empty() {
        anyhow::bail!("배정할 후보 모델이 없습니다. 연결과 모델 순위표(ranks)를 확인하세요.");
    }

    // 역할 배정 — simple 외의 프로파일은 전부 도구를 쓰므로 tools 지원을 요구한다.
    let strongest = |exclude: Option<&RoleCandidate>| -> Option<&RoleCandidate> {
        pool.iter()
            .filter(|c| c.tools)
            .filter(|c| exclude.is_none_or(|e| !(c.provider == e.provider && c.id == e.id)))
            .max_by_key(|c| c.score)
    };
    let dev = strongest(None)
        .ok_or_else(|| anyhow!("도구를 지원하는 연결이 없어 dev 역할을 배정할 수 없습니다."))?;
    let advanced = dev;
    let verify = strongest(Some(dev)).unwrap_or(dev);
    let medium = pool
        .iter()
        .filter(|c| c.tools && c.cheap)
        .max_by_key(|c| c.score)
        .unwrap_or(dev);
    let simple = pool
        .iter()
        .filter(|c| c.cheap)
        .min_by_key(|c| c.score)
        .unwrap_or(medium);

    let spec = |c: &RoleCandidate| format!("{}:{}", c.provider, c.id);
    set_manual_model(cfg, TaskClass::Simple, &spec(simple))?;
    set_manual_model(cfg, TaskClass::Medium, &spec(medium))?;
    set_manual_model(cfg, TaskClass::Advanced, &spec(advanced))?;
    set_manual_model(cfg, TaskClass::Dev, &spec(dev))?;
    crate::config::write_toml_key(
        &cfg.path,
        "[harness]",
        "manual_verify",
        &crate::config::toml_string(&spec(verify)),
    )?;
    set_selection_mode(cfg, "manual")?;
    set_strategy(cfg, HarnessStrategy::Multi)?;

    notes.push("Provider mode: multi — 역할별 자동 배정 완료 (selection=manual)".into());
    for (role, c) in [
        ("simple", simple),
        ("medium", medium),
        ("advanced", advanced),
        ("dev", dev),
        ("verify", verify),
    ] {
        notes.push(format!(
            "  {role:8} → {} / {}  (score {})",
            c.provider, c.id, c.score
        ));
    }
    notes.push("되돌리기: /engine single <연결이름>".into());
    Ok(notes)
}

/// 연결된(등록된) 모델만. 미연결 프로바이더는 절대 고르지 않는다.
/// 사용자가 기본 연결(default_provider)을 지정했다면 그 안에서 자동 선택한다 —
/// 기본 연결·기본 모델 설정이 자동 하네스보다 우선한다.
pub fn pick_auto(
    cfg: &Config,
    class: TaskClass,
    sub: &crate::config::SubAgentConfig,
    needs_tools: bool,
) -> Result<(String, String, Option<String>)> {
    let all = crate::auth::registered_models(cfg);
    if all.is_empty() {
        anyhow::bail!(
            "연결된 서비스가 없습니다. rafikx settings 또는 rafikx doctor 에서 번호로 연결하세요."
        );
    }

    // multi 전략은 기본 연결에 고정하지 않고 등록 모델 전체에서 비용·능력 기준으로 고른다.
    let table = crate::ranks::load();
    let mut regs: Vec<crate::auth::RegisteredModel> = all
        .into_iter()
        .filter(|r| {
            if !needs_tools {
                return true;
            }
            cfg.provider(&r.provider)
                .map(|p| p.supports_tools)
                .unwrap_or(false)
        })
        .collect();
    if regs.is_empty() {
        anyhow::bail!(
            "이 작업에 필요한 도구를 지원하는 연결이 없습니다. rafikx settings 에서 Anthropic 등을 연결하세요."
        );
    }

    let prefer_strong = matches!(class, TaskClass::Advanced | TaskClass::Dev);
    let prefer_cheap = matches!(class, TaskClass::Simple | TaskClass::Medium);

    if prefer_cheap {
        if let Some(hit) = pick_cheap(&regs, &table, &sub.provider) {
            return Ok((hit.provider, hit.id, None));
        }
    }

    if prefer_strong {
        if let Some(hit) = pick_strongest(&regs, &table) {
            return Ok((hit.provider, hit.id.clone(), Some(hit.id)));
        }
    }

    // 순위 모르면 프로파일 기본 (그 프로바이더가 연결된 경우만)
    if crate::auth::is_usable(cfg, &sub.provider) {
        if let Ok(p) = cfg.provider(&sub.provider) {
            if !needs_tools || p.supports_tools {
                let model = model_for_role(p, &sub.model_role);
                return Ok((sub.provider.clone(), model, None));
            }
        }
    }
    let first = regs.remove(0);
    Ok((first.provider, first.id, None))
}

fn pick_cheap(
    regs: &[crate::auth::RegisteredModel],
    table: &crate::ranks::RankTable,
    preferred_provider: &str,
) -> Option<crate::auth::RegisteredModel> {
    let mut cheap: Vec<&crate::auth::RegisteredModel> = regs
        .iter()
        .filter(|r| r.small || crate::ranks::is_cheap_id(&r.id))
        .collect();
    if cheap.is_empty() {
        // 등록분이 전부 플래그십이면 그대로 써도 됨 — 그 중 가장 낮은 점수(저렴 쪽) 선호
        let mut all = regs.to_vec();
        all.sort_by_key(|r| crate::ranks::score_of(table, &r.id).unwrap_or(999));
        return all.first().cloned();
    }
    cheap.sort_by_key(|r| {
        let pref = if r.provider == preferred_provider {
            0
        } else {
            1
        };
        (pref, crate::ranks::score_of(table, &r.id).unwrap_or(50))
    });
    cheap.first().cloned().cloned()
}

fn pick_strongest(
    regs: &[crate::auth::RegisteredModel],
    table: &crate::ranks::RankTable,
) -> Option<crate::auth::RegisteredModel> {
    let mut ranked: Vec<(i32, bool, &crate::auth::RegisteredModel)> = regs
        .iter()
        .filter_map(|r| {
            let e = crate::ranks::match_entry(table, &r.id)?;
            Some((
                e.score,
                crate::ranks::Tier::parse(&e.tier) == crate::ranks::Tier::Top5,
                r,
            ))
        })
        .collect();
    if !ranked.is_empty() {
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        // top5 가 등록되어 있으면 그걸, 없으면 등록분 중 최고점
        if let Some(hit) = ranked.iter().find(|(_, top, _)| *top) {
            return Some((*hit.2).clone());
        }
        return Some(ranked[0].2.clone());
    }
    None
}

fn pick_for_provider(
    cfg: &Config,
    name: &str,
    class: TaskClass,
    role: &str,
    _needs_tools: bool,
) -> Option<String> {
    let Ok(p) = cfg.provider(name) else {
        return None;
    };
    // 프로바이더 자체 자동(model_auto)일 때만 그 안에서 순위 기반 선택.
    // 사용자가 저장한 model 은 설정값이므로 존중한다 (selection=auto 라도 덮어쓰지 않음).
    if p.model_auto {
        let regs: Vec<_> = crate::auth::registered_models(cfg)
            .into_iter()
            .filter(|r| r.provider == name)
            .collect();
        let table = crate::ranks::load();
        if matches!(class, TaskClass::Simple | TaskClass::Medium) {
            if let Some(h) = pick_cheap(&regs, &table, name) {
                return Some(h.id);
            }
        }
        if let Some(h) = pick_strongest(&regs, &table) {
            return Some(h.id);
        }
    }
    Some(model_for_role(p, role))
}

fn model_for_role(p: &ProviderConfig, role: &str) -> String {
    if role == "small" {
        p.small_model.clone().unwrap_or_else(|| p.model.clone())
    } else {
        p.model.clone()
    }
}

pub fn build_provider(cfg: &Config, name: &str) -> Result<DynProvider> {
    let accs = crate::accounts::for_provider(name);
    let id = crate::usage::select_account(&accs).unwrap_or_else(|| name.to_string());
    build_provider_account(cfg, name, &id)
}

pub fn build_provider_account(cfg: &Config, name: &str, account_id: &str) -> Result<DynProvider> {
    let p = cfg.provider(name)?;
    let cred = crate::auth::resolve_account_credential(cfg, name, account_id)?;
    match p.kind.as_str() {
        "anthropic" => {
            let c = cred.ok_or_else(|| {
                anyhow!("'{name}' 연결이 없습니다. rafikx settings 에서 번호로 연결하세요")
            })?;
            if c.oauth {
                Ok(DynProvider::Anthropic(AnthropicProvider::with_oauth(
                    c.token,
                )?))
            } else {
                Ok(DynProvider::Anthropic(AnthropicProvider::new(c.token)?))
            }
        }
        "openai_compat" => {
            if name == "openai" {
                if let Some(c) = &cred {
                    if c.oauth {
                        return Ok(DynProvider::OpenAi(OpenAiCompatProvider::with_codex_oauth(
                            c.token.clone(),
                            c.account_id.clone(),
                        )?));
                    }
                }
            }
            let base = p
                .base_url
                .clone()
                .ok_or_else(|| anyhow!("프로바이더 '{name}' 에 base_url 이 없습니다"))?;
            let key = cred.map(|c| c.token);
            if crate::auth::auth_mode(name, p) != "none" && key.is_none() {
                return Err(anyhow!(
                    "'{name}' 연결이 없습니다. rafikx settings 에서 번호로 연결하세요"
                ));
            }
            Ok(DynProvider::OpenAi(OpenAiCompatProvider::new(base, key)?))
        }
        other => Err(anyhow!("알 수 없는 프로바이더 kind: {other}")),
    }
}

fn account_ids_for(name: &str) -> Vec<String> {
    let accs = crate::accounts::for_provider(name);
    if accs.is_empty() {
        vec![name.to_string()]
    } else {
        crate::usage::order_ids(&accs)
    }
}

async fn try_accounts<F, Fut>(cfg: &Config, name: &str, mut call: F) -> Result<ChatResponse>
where
    F: FnMut(DynProvider) -> Fut,
    Fut: std::future::Future<Output = Result<ChatResponse>>,
{
    let ids = account_ids_for(name);
    let mut last_err = None;
    for (i, id) in ids.iter().enumerate() {
        let wait = crate::usage::seconds_left(id);
        if wait > 0 && wait <= 20 {
            crate::ui::note(&format!("계정 대기 {wait}초 후 재시도…"));
            tokio::time::sleep(Duration::from_secs(wait as u64)).await;
        } else if wait > 20 {
            // retry_after 존중: 리밋이 긴 계정은 마지막 계정이어도 두드리지
            // 않는다 — 이 연결을 건너뛰면 폴백 체인이 다른 연결을 쓴다.
            // (예전에는 단일 계정이면 대기 없이 재호출해 429 폭풍이 났다.)
            crate::ui::note(&format!(
                "{} 리밋 {}분 → 건너뜀",
                crate::accounts::get(id)
                    .map(|a| a.label)
                    .unwrap_or_else(|| id.clone()),
                (wait + 59) / 60
            ));
            let _ = i;
            continue;
        }
        let Ok(client) = build_provider_account(cfg, name, id) else {
            continue;
        };
        match call(client).await {
            Ok(resp) => {
                crate::usage::record_success(id, &resp);
                crate::usage::apply_hint(id, &resp.limit);
                return Ok(resp);
            }
            Err(e) if is_rate_limited(&e) => {
                let secs = crate::usage::parse_retry_after(&format!("{e:#}"));
                crate::usage::mark_limited(id, secs);
                crate::ui::warn(&format!(
                    "리밋 → 다음 계정으로 전환 ({})",
                    crate::accounts::get(id)
                        .map(|a| a.label)
                        .unwrap_or_else(|| id.clone())
                ));
                last_err = Some(e);
            }
            Err(e) if is_auth_failure(&e) => {
                fallback_warn(&format!(
                    "{name} 키 인증 실패(401/403) — rafikx login 에서 키를 갱신하세요"
                ));
                last_err = Some(e);
            }
            Err(e) if is_retryable(&e) => {
                last_err = Some(e);
            }
            Err(e) => {
                last_err = Some(e);
                break;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("'{name}' 사용 가능한 계정이 없습니다")))
}

/// 401/403 등 키 문제 — 재시도보다 재연결이 답이다.
fn is_auth_failure(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}");
    s.contains("401") || s.contains("403")
}

pub fn fallback_order(cfg: &Config, primary: &str, cli_provider: Option<&str>) -> Vec<String> {
    let mut order = Vec::new();
    if let Some(p) = cli_provider {
        if crate::auth::is_usable(cfg, p) {
            order.push(p.to_string());
        }
    }
    if crate::auth::is_usable(cfg, primary) && !order.iter().any(|x| x == primary) {
        order.push(primary.to_string());
    }
    for f in &cfg.file.harness.fallback {
        if crate::auth::is_usable(cfg, f) && !order.iter().any(|x| x == f) {
            order.push(f.clone());
        }
    }
    order
}

/// 실행 경로(에이전트 루프·검증·검증자 게이트)용 폴백 순서.
/// 엔진 고정이 있으면 그 프로바이더 하나로 제한한다 — 계정 다중 순회는 안쪽
/// (chat_with_fallback)이 담당하므로 리밋 시 같은 프로바이더의 다른 계정으로만 넘어간다.
/// 백그라운드 보조 호출(교훈 반성·LLM 분류·self-harness 제안)은 고정 대상이 아니므로
/// 기존 `fallback_order` 를 그대로 쓴다 (설계 §11.2).
pub fn fallback_order_pinned(
    cfg: &Config,
    primary: &str,
    cli_provider: Option<&str>,
) -> Vec<String> {
    let order = fallback_order(cfg, primary, cli_provider);
    limit_order_to_pin(engine_pin(cfg).as_deref(), cli_provider, order)
}

/// 고정이 걸린 실행의 폴백 순서 계산 (순수 함수).
/// 사용자가 --provider 로 직접 지정했으면 고정을 양보하고, 고정 프로바이더가 순서에
/// 아예 없으면(연결 없음) 원래 순서를 지킨다 — 가용성 우선.
fn limit_order_to_pin(
    pin: Option<&str>,
    cli_provider: Option<&str>,
    order: Vec<String>,
) -> Vec<String> {
    let Some(pin) = pin.map(str::trim).filter(|p| !p.is_empty()) else {
        return order;
    };
    if cli_provider.map(str::trim).is_some_and(|p| !p.is_empty()) {
        return order;
    }
    if order.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
        return vec![pin.to_string()];
    }
    order
}

fn model_for_fallback(
    cfg: &Config,
    name: &str,
    model_role: &str,
    original_model: &str,
    primary: Option<&str>,
) -> Option<String> {
    if !crate::auth::is_usable(cfg, name) {
        return None;
    }
    let Ok(p) = cfg.provider(name) else {
        return None;
    };
    if Some(name) == primary && !original_model.is_empty() {
        Some(original_model.to_string())
    } else {
        Some(model_for_role(p, model_role))
    }
}

/// 진행 중 턴의 컨텍스트 창 — TUI 실시간 사용량 표시가 읽는다.
pub static CURRENT_CTX_WINDOW: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_current_ctx_window(v: u32) {
    CURRENT_CTX_WINDOW.store(v, std::sync::atomic::Ordering::Relaxed);
}

pub fn current_ctx_window() -> u32 {
    CURRENT_CTX_WINDOW.load(std::sync::atomic::Ordering::Relaxed)
}

/// 백그라운드 작업(교훈 반성 등)이 폴백 실패를 화면에 띄우지 않게 하는 스위치.
static FALLBACK_QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_fallback_quiet(quiet: bool) {
    FALLBACK_QUIET.store(quiet, std::sync::atomic::Ordering::Relaxed);
}

fn fallback_warn(msg: &str) {
    // 폴백 과정의 개별 실패(503·429·401 등)는 화면에 노출하지 않는다.
    // 폴백이 성공하면 사용자는 최종 답변만 보고, 전부 실패했을 때만 오류가 전달된다.
    crate::applog::debug(&format!("fallback: {msg}"));
}

pub async fn chat_with_fallback(
    cfg: &Config,
    order: &[String],
    model_role: &str,
    mut req: ChatRequest,
) -> Result<(String, ChatResponse)> {
    let original_model = req.model.clone();
    let primary = order.first().map(|s| s.as_str());
    // 첫 번째(주 연결) 오류를 끝까지 보존한다 — 마지막 폴백의 오류가 원인을 가리지 않게.
    let mut primary_err: Option<anyhow::Error> = None;
    let mut last_err = None;
    for name in order {
        let Some(model) = model_for_fallback(cfg, name, model_role, &original_model, primary)
        else {
            continue;
        };
        req.model = model;
        match try_accounts(cfg, name, |client| {
            let req = req.clone();
            async move { client.chat(&req).await }
        })
        .await
        {
            Ok(resp) => return Ok((name.clone(), resp)),
            Err(e) => {
                fallback_warn(&format!("{name} 호출 실패 ({}) → 다음 연결", short_err(&e)));
                if Some(name.as_str()) == primary && primary_err.is_none() {
                    primary_err = Some(e);
                } else {
                    last_err = Some(e);
                }
            }
        }
    }
    Err(primary_err
        .or(last_err)
        .unwrap_or_else(|| anyhow!("사용 가능한 프로바이더가 없습니다")))
}

fn short_err(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    s.chars().take(120).collect()
}

pub async fn stream_with_fallback<F>(
    cfg: &Config,
    order: &[String],
    model_role: &str,
    mut req: ChatRequest,
    mut on_text: F,
) -> Result<(String, ChatResponse)>
where
    F: FnMut(&str),
{
    use std::sync::atomic::{AtomicUsize, Ordering};
    let original_model = req.model.clone();
    let primary = order.first().map(|s| s.as_str());
    let mut primary_err: Option<anyhow::Error> = None;
    let mut last_err: Option<anyhow::Error> = None;
    let emitted = AtomicUsize::new(0);

    for name in order {
        let Some(model) = model_for_fallback(cfg, name, model_role, &original_model, primary)
        else {
            continue;
        };
        req.model = model;
        let ids = account_ids_for(name);
        for (i, id) in ids.iter().enumerate() {
            let wait = crate::usage::seconds_left(id);
            if wait > 20 {
                // retry_after 존중 — 마지막 계정이어도 리밋 중이면 건너뛰고
                // 다음 연결로 폴백한다 (429 재시도 폭풍 방지).
                let _ = i;
                continue;
            }
            if wait > 0 && wait <= 20 {
                tokio::time::sleep(Duration::from_secs(wait as u64)).await;
            }
            let Ok(client) = build_provider_account(cfg, name, id) else {
                continue;
            };

            // 스트림 실패(네트워크·5xx·미완료 EOF)는 짧은 백오프 뒤 같은 계정으로 재시도한다.
            // 화면에 이미 텍스트가 흘러나간 뒤의 재시도·폴백은 중복 출력을 만드므로 금지한다.
            let mut attempt = 0u32;
            loop {
                let mut track = |piece: &str| {
                    emitted.fetch_add(piece.chars().count(), Ordering::Relaxed);
                    on_text(piece);
                };
                match client.chat_stream(&req, &mut track).await {
                    Ok(resp) => {
                        crate::usage::record_success(id, &resp);
                        return Ok((name.clone(), resp));
                    }
                    Err(e) if is_rate_limited(&e) => {
                        crate::usage::mark_limited(
                            id,
                            crate::usage::parse_retry_after(&format!("{e:#}")),
                        );
                        crate::ui::warn("리밋 → 다음 계정으로 전환");
                        last_err = Some(e);
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e);
                        if is_retryable(last_err.as_ref().unwrap()) && attempt < 2 {
                            attempt += 1;
                            if emitted.load(Ordering::Relaxed) > 0 {
                                // 출력이 이미 나간 뒤의 절단 — 같은 연결로만 다시 시도하고,
                                // 재출력이 중복으로 보이지 않게 경계 표시를 남긴다.
                                on_text("\n[연결 끊김 — 같은 연결로 재시도]\n");
                            }
                            tokio::time::sleep(Duration::from_millis(800 * u64::from(attempt)))
                                .await;
                            continue;
                        }
                        break;
                    }
                }
            }
            if emitted.load(Ordering::Relaxed) > 0 {
                return Err(last_err.unwrap_or_else(|| anyhow!("응답 도중 스트림이 끊겼습니다")));
            }
            break;
        }
        if last_err.is_some() && Some(name.as_str()) == primary && primary_err.is_none() {
            primary_err = last_err.take();
            fallback_warn(&format!(
                "{name} 실패 ({}) → 다음 연결",
                primary_err.as_ref().map(short_err).unwrap_or_default()
            ));
        }
    }
    Err(primary_err
        .or(last_err)
        .unwrap_or_else(|| anyhow!("사용 가능한 프로바이더가 없습니다")))
}

pub async fn chat_accounts(cfg: &Config, provider: &str, req: ChatRequest) -> Result<ChatResponse> {
    try_accounts(cfg, provider, |client| {
        let req = req.clone();
        async move { client.chat(&req).await }
    })
    .await
}

pub async fn classify(
    cfg: &Config,
    text: &str,
    obsidian: bool,
    forced: Option<&str>,
) -> Result<TaskClass> {
    if let Some(s) = forced {
        return TaskClass::parse(s)
            .ok_or_else(|| anyhow!("--class 값은 simple|medium|advanced|dev 여야 합니다"));
    }
    if cfg.file.general.classifier == "llm" {
        match classify_llm(cfg, text).await {
            Ok(c) => return Ok(c),
            Err(_) => {}
        }
    }
    Ok(classify_rules(text, obsidian))
}

async fn classify_llm(cfg: &Config, text: &str) -> Result<TaskClass> {
    let default = cfg.file.general.default_provider.clone();
    let order = fallback_order(cfg, &default, None);
    let req = ChatRequest {
        model: String::new(),
        system: "다음 지시를 simple/medium/advanced/dev 중 한 단어로만 분류하라.".into(),
        messages: vec![Message::user_text(text)],
        tools: vec![],
        max_tokens: 8,
        stream: false,
    };
    let (_name, resp) = chat_with_fallback(cfg, &order, "small", req).await?;
    let word = resp
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("")
        .trim();
    TaskClass::parse(word).ok_or_else(|| anyhow!("llm 분류 모호: {word}"))
}

pub fn print_binding(cfg: &Config, b: &Binding) {
    crate::ui::note(&format!(
        "하네스  {} → {}  ·  {}/{}{}",
        b.class.as_str(),
        b.profile_name,
        b.provider_name,
        crate::ui::bold(&b.model),
        engine_suffix(cfg)
    ));
}

/// 기본값이 아닐 때만 붙는 실행 축 표시 — ` · engine=claude · graph`.
pub fn engine_suffix(cfg: &Config) -> String {
    let mut s = String::new();
    let (engine, _) = crate::engine::normalize(&cfg.file.general.engine);
    if engine != crate::engine::DEFAULT_ENGINE {
        s.push_str(&format!("  ·  engine={engine}"));
    }
    let discipline = crate::engine::normalize_discipline(&cfg.file.general.discipline);
    if discipline != crate::engine::Discipline::Harness {
        s.push_str(&format!("  ·  {}", discipline.as_str()));
    }
    s
}

pub fn print_binding_table(cfg: &Config) {
    println!();
    println!("분류 → 프로파일 → 프로바이더(kind) → 모델");
    for class in [
        TaskClass::Simple,
        TaskClass::Medium,
        TaskClass::Advanced,
        TaskClass::Dev,
    ] {
        match bind(cfg, class, None, None) {
            // 표는 실제 실행에 붙는 조합을 보여야 하므로 엔진 고정을 반영한다
            // (경고는 실행 경로에서만 낸다).
            Ok(mut b) => {
                let _ = apply_engine_pin(cfg, &mut b, None, None);
                println!(
                    "  {} → {} → {} ({}) → {}",
                    b.class.as_str(),
                    b.profile_name,
                    b.provider_name,
                    b.kind,
                    b.model
                );
            }
            Err(e) => println!("  {} → (실패: {e})", class.as_str()),
        }
    }
}

pub async fn ping_provider(cfg: &Config, name: &str) -> String {
    let Ok(p) = cfg.provider(name) else {
        return format!("{name}: config 없음");
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build();
    let Ok(client) = client else {
        return format!("{name}: HTTP 클라이언트 실패");
    };
    let cred = crate::auth::resolve_credential(cfg, name).ok().flatten();
    match p.kind.as_str() {
        "anthropic" => {
            let Some(c) = cred else {
                return format!("{name}: 미연결 (ping 생략)");
            };
            let req = crate::auth::apply_anthropic_cred(
                client
                    .get("https://api.anthropic.com/v1/models")
                    .header("anthropic-version", "2023-06-01"),
                &c,
            );
            match req.send().await {
                Ok(r) if r.status().is_success() => format!("{name}: ping OK"),
                Ok(r) => format!("{name}: ping HTTP {}", r.status().as_u16()),
                Err(e) => format!("{name}: ping 실패 ({e})"),
            }
        }
        "openai_compat" => {
            let oauth_openai = name == "openai" && cred.as_ref().is_some_and(|c| c.oauth);
            let url = if oauth_openai {
                "https://chatgpt.com/backend-api/codex/models".to_string()
            } else {
                let Some(base) = &p.base_url else {
                    return format!("{name}: base_url 없음");
                };
                format!("{}/models", base.trim_end_matches('/'))
            };
            let mut req = client.get(url);
            if let Some(c) = &cred {
                req = req.header("Authorization", format!("Bearer {}", c.token));
                if oauth_openai {
                    req = req
                        .header("originator", "codex_cli_rs")
                        .header("User-Agent", "codex_cli_rs");
                    if !c.account_id.is_empty() {
                        req = req.header("chatgpt-account-id", &c.account_id);
                    }
                }
            }
            match req.send().await {
                Ok(r) if r.status().is_success() => format!("{name}: ping OK"),
                Ok(r) => format!("{name}: ping HTTP {}", r.status().as_u16()),
                Err(e) => format!("{name}: ping 실패 ({e})"),
            }
        }
        other => format!("{name}: kind={other} ping 생략"),
    }
}

/// oh-my-pi (github.com/can1357/oh-my-pi) 의 시스템 프롬프트 구성을 RafikX 에
/// 이식한 것: RFC 2119 규약 → 엔지니어링 원칙 → 증거 우선 간결 페르소나 →
/// 6단계 워크플로 → 전달 계약 → Critical. 답변 끝 행동 선택 메뉴는 금지한다.
pub fn system_prompt(cfg: &Config, extra: &str, lessons: &str) -> String {
    let mut s = format!(
        "You are RafikX, a coding agent for the terminal.\n\
         Workspace: {}\n\
         If the user writes in Korean, reply in Korean.\n\
         \n\
         [규약]\n\
         RFC 2119 키워드를 쓴다: MUST(반드시), NEVER(절대 금지), SHOULD(권장), AVOID(지양), MAY(허용).\n\
         \n\
         [엔지니어링]\n\
         - 정확성이 먼저다. 그다음이 6개월 뒤의 유지보수성이다.\n\
         - 취향을 적용한다: 무게 없는 코드는 지우고, 불필요한 추상화는 거부하고, 지루한(boring) 해법을 선호한다. 설계는 철저하고 우아하게.\n\
         - 예상 밖의 저장소 변경은 사용자의 작업이다. 적응한다.\n\
         - 사용자의 말이 최우선이다: 사용자가 보고한 상태(오류·실패·관찰)는 ground truth 다. 그대로 근거로 행동하고, 이미 보고된 사실을 재확인하려고 검사를 다시 돌리지 않는다(NEVER).\n\
         \n\
         [말투 — 증거 우선 간결 엔지니어]\n\
         - 모든 문장은 사실·결정·리스크 중 하나다. 의례·헤징·자기요약·필러·과장은 금지(NEVER).\n\
         - 명확하다면 완전한 문장 대신 조각 표현도 허용(MAY). 기술 독자를 가정하고, 뻔한 단계를 서술하거나 기초를 과잉 설명하지 않는다.\n\
         - 구체적으로: 정확한 파일·심볼·API·상태 필드·엣지케이스·검증 방법을 적는다.\n\
         - 결론 먼저, 증거 다음. 추론은 사실→제약→트레이드오프→결정→검증 순으로 압축한다.\n\
         - 불확실하면 주장하는 그 자리에서 밝히고, 트레이드오프에 이름을 붙이고, 안전한(boring) 쪽을 고른다.\n\
         - 형식은 요청에 맞춘다(MUST). 산문은 짧게, 증거·검증·차단 사유는 완전하게.\n\
         - 관측하지 않은 주장에는 [INFERENCE] 를 붙인다.\n\
         - 답변 끝에 다음 행동 선택지 목록이나 번호 메뉴를 붙이지 않는다(NEVER). 후속 제안이 정말 필요하면 한 문장으로 끝낸다.\n\
         - 리스크를 숨긴 계획이나 틀린 주장에는 반박한다: 리스크를 명명하고 증거를 보이고 대안을 제시한다. 사용자가 기각하면 그 결정을 실행하고 재논쟁하지 않는다.\n\
         \n\
         [워크플로]\n\
         1 Scope — 요청을 파악한다. 다중 파일 작업은 파일보다 계획이 먼저다.\n\
         2 Research — 편집 전에 조각이 아니라 구획을 읽는다. 기존 패턴을 재사용한다(MUST); 기존 관례 옆에 두 번째 관례를 만드는 것은 금지다. 도구 실패나 읽은 뒤 바뀐 파일은 다시 읽고 행동한다.\n\
         3 Decompose — todo 를 갱신한다. 하네스가 단계별 실행(todo)을 지시하면 반드시 따른다(MUST); 그 지시가 없는 사소한 요청만 건너뛴다.\n\
         4 Implement — 원인을 고친다. 요청 없이 증상 억제·입력 특수케이스 처리 금지(NEVER). 이관은 깨끗하게: 모든 호출부를 옮기고 낡은 코드·별칭·재수출·주석을 제거한다. 새 파일보다 기존 파일 갱신을 선호한다.\n\
         5 Verify — 검증 없는 산출물 전달 금지(NEVER). 버그 수정은 재현→수정→재현 소멸 확인. 스모크는 테스트 파일이 아니라 실제 실행이다: 실행하고, 바뀐 경로를 통과시키고, 결과를 관찰한다.\n\
         6 Cleanup — 스모크가 작업을 증명한 뒤의 마지막 단계다. 실험·일회성 조사에는 테스트·문서 정리를 만들지 않는다.\n\
         \n\
         [전달 계약]\n\
         - 완전한 산출물 전에 멈추지 않는다(NEVER). 단계 경계·todo 전환·중간 단계는 멈출 이유가 아니다: 같은 턴에서 계속한다.\n\
         - 출력을 지어내지 않는다(NEVER): 코드·도구·테스트·문서에 대한 주장은 근거가 있어야 한다.\n\
         - 더 쉬운 문제로 바꿔치기하지 않는다(NEVER): 요청에 없는 재시도·검증·텔레메트리·추상화를 멋대로 더하지 않고, 증상만 가리지 않는다. 실제 요청만 푼다.\n\
         - '완료' = 명세된 end-to-end 동작 + 모든 수락 기준. 스텁·플레이스홀더·mock·no-op·'TODO: implement'·'일단 MVP' 는 미완이다(NEVER). 범위 축소는 이 대화에서 사용자의 명시적 승인이 있을 때만.\n\
         - 도구·저장소·파일로 알 수 있는 정보를 사용자에게 묻지 않는다(NEVER). 반쯤 푼 작업을 떠넘기지 않는다.\n\
         - blocked 선언 전에 도구와 컨텍스트로 정말 확인 불가한지 먼저 확인한다. 검사 1회 실패는 blocked 가 아니다. 도달 가능한 작업은 끝내고, 정확히 무엇이 없고 무엇을 시도했는지 밝힌다.\n\
         \n\
         [안전]\n\
         - 커밋과 배포는 사용자가 명시적으로 요청하기 전에는 실행하지 않는다(NEVER).\n\
         - 내가 만들지 않은 무관한 코드 삭제·파괴적 git 명령 전에는 확인을 받는다. 이관이 낡게 만든 코드는 범위 안이다.\n\
         - task 위임은 사용자가 병렬을 요청했거나 진짜 독립 슬라이스가 있을 때만 쓴다. 최상위 계획을 위임하지 않는다.\n\
         \n\
         [하네스 표기]\n\
         - 수치 비교는 ```chart 블록(한 줄에 '라벨: 수치')으로 — 터미널이 실제 막대그래프로 렌더링한다. ASCII 아트 도표(+---+, ->, 문자 박스)는 금지.\n\
         - 항목 나열은 마크다운 표를 쓴다.\n\
         {extra}",
        cfg.workspace.display()
    );
    // OMO 의 Rules Injection 수용: 워크스페이스 규칙 파일을 자동 주입 (경량 상한 8K).
    for fname in ["AGENTS.md", "RAFIKX.md"] {
        let p = cfg.workspace.join(fname);
        if let Ok(body) = std::fs::read_to_string(&p) {
            let trimmed: String = body.trim().chars().take(8000).collect();
            if !trimmed.is_empty() {
                s.push_str(&format!("\n\n[프로젝트 규칙 — {fname}]\n{trimmed}"));
            }
            break;
        }
    }
    if !lessons.trim().is_empty() {
        s.push('\n');
        s.push_str(lessons.trim_end());
    }
    s
}

pub async fn run_pipeline(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    yes: bool,
    cli_provider: Option<&str>,
    resume: Option<Vec<Message>>,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
) -> Result<AgentOutcome> {
    let run_id =
        crate::graph::current_run().unwrap_or_else(|| format!("run-{}", crate::db::Db::new_id()));
    let context = RunContext::for_config(RunId::new(run_id), Arc::new(cfg.clone()))
        .with_live_sink(crate::ui::current_live_sink());
    run_pipeline_with_context(
        cfg,
        binding,
        task,
        yes,
        cli_provider,
        resume,
        remote,
        local_ask,
        context,
    )
    .await
}

pub async fn run_pipeline_with_context(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    yes: bool,
    cli_provider: Option<&str>,
    resume: Option<Vec<Message>>,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    let start_event = if binding.plan_first {
        crate::lifecycle::LifecycleEventData::PlanningStarted
    } else {
        crate::lifecycle::LifecycleEventData::RunStarted {
            model: Some(binding.model.clone()),
        }
    };
    let _ = run_context.transition_lifecycle(start_event);
    let result = run_pipeline_inner(
        cfg,
        binding,
        task,
        yes,
        cli_provider,
        resume,
        remote,
        local_ask,
        run_context.clone(),
    )
    .await;
    match &result {
        Ok(outcome) => {
            let state = match outcome.status.as_str() {
                "ok" => TerminalState::Succeeded,
                "cancelled" => TerminalState::Cancelled,
                "limit" | "incomplete" => TerminalState::Limited,
                _ => TerminalState::Failed,
            };
            run_context.finish_with_error(state, outcome.error.clone());
        }
        Err(error) => {
            run_context.finish_with_error(TerminalState::Failed, Some(error.to_string()));
        }
    }
    result
}

/// 계획 호출은 메인 system 을 그대로 이어받고 이 머리말만 덧붙인다.
const PLAN_MODE_HEADER: &str = "\n\n[계획 모드] 지금은 계획만 세운다. 도구는 쓰지 마라.\n";

const PLAN_BRIEF_INSTRUCTION: &str = "작업 계획을 3~7개 항목으로만 출력하라.";

/// PlanDepth::Contract — 산출물을 3부로 강제한다 (dev/advanced 클래스에서만 활성).
const PLAN_CONTRACT_INSTRUCTION: &str = "\
    20년 경력 시니어가 착수 전 검토하듯 계획하라: 기존 코드·파일 구조에 대한 가정을 명시하고, \
    위험(호환성·회귀·엣지케이스)을 한 줄씩 짚는다.\n\
    출력은 반드시 아래 세 부분으로만 구성한다. 머리표는 대괄호 그대로 쓴다.\n\
    [해석] 요구사항을 한 문단으로 재진술하고, 모호한 점과 그중 채택한 해석을 밝힌다.\n\
    [완료 기준] 검증 가능한 체크리스트 3~10항목. 각 항목은 '무엇이 충족되어야 하는가'와 \
    '어떻게 확인하는가(명령·파일·관찰 대상)'를 함께 적는다.\n\
    [작업 분해] 실행 순서 3~9단계. 각 단계는 한 줄로 적고 결과물을 명시한다.";

/// discipline = loop — 종료 조건을 시스템 프롬프트에 못 박는다.
const LOOP_DISCIPLINE_RULE: &str =
    "\n\n[루프 규율] 모든 todo 완료 + 검증 통과 전에는 완료를 선언하지 마라.";

/// discipline = loop — 정체를 감지한 사이클의 continuation 에 덧붙이는 전략 전환 지시.
const LOOP_STALE_SWITCH: &str = "\n직전 사이클에서 진전이 없었다. 현재 todo를 더 작은 단위로 \
     쪼개거나 다른 도구/경로로 전환하라. 같은 접근의 반복을 금지한다.";

/// discipline = graph — 계획이 완료 기준과 노드 DAG 를 함께 산출한다.
/// JSON 은 산문 뒤에 와야 한다: [완료 기준] 추출이 첫 `{` 앞까지만 훑기 때문이다.
const PLAN_GRAPH_INSTRUCTION: &str = "\
    작업을 상태 그래프로 분해하라. 출력은 아래 두 부분으로만, 이 순서로 구성한다.\n\
    먼저 [완료 기준] 절: 검증 가능한 체크리스트 3~10항목. 각 항목에 '어떻게 확인하는가'를 함께 적는다.\n\
    그다음 노드 DAG 를 JSON 한 덩어리로 적는다. JSON 뒤에는 아무 설명도 붙이지 않는다.\n\
    {\"nodes\":[{\"id\":\"n1\",\"goal\":\"이 노드에서 끝낼 일\",\"deps\":[],\"produces\":\"산출물 한 줄\"}]}\n\
    - 노드는 3~7개. id 는 짧게, goal 과 produces 는 한국어로 쓴다.\n\
    - deps 에는 먼저 끝나야 하는 노드의 id 만 넣는다. 순환은 금지(NEVER)다.\n\
    - 각 노드는 신선한 컨텍스트에서 따로 실행된다. goal 만 읽고도 무엇을 할지 알 수 있게 쓴다.";

/// 계획 호출용 시스템 프롬프트 — 메인 system 조립 결과를 그대로 이어받고 계획 모드
/// 지시만 덧붙인다. lessons·system_extra·프로젝트 규칙(AGENTS.md)·엔진 블록이
/// 계획에도 반영되어야 하므로 절대 통째로 교체하지 않는다.
/// `plan_extra` 는 Self-Harness 의 계획 전용 면(plan_instruction) — 메타 레이어가
/// 켜졌을 때만 채워지고, decorate_system 이 아니라 여기서만 붙는다.
fn plan_system_prompt(system: &str, depth: crate::engine::PlanDepth, plan_extra: &str) -> String {
    plan_system_prompt_with(
        system,
        if depth == crate::engine::PlanDepth::Contract {
            PLAN_CONTRACT_INSTRUCTION
        } else {
            PLAN_BRIEF_INSTRUCTION
        },
        plan_extra,
    )
}

/// 계획 지시만 갈아끼우는 공통 조립 — depth 별 지시와 graph 분야 지시가 함께 쓴다.
fn plan_system_prompt_with(system: &str, instruction: &str, plan_extra: &str) -> String {
    let mut s = String::with_capacity(system.len() + instruction.len() + 64);
    s.push_str(system);
    s.push_str(PLAN_MODE_HEADER);
    s.push_str(instruction);
    let extra = plan_extra.trim();
    if !extra.is_empty() {
        s.push_str("\n\n[Self-Harness 계획 지침] ");
        s.push_str(extra);
    }
    s
}

/// 계획 텍스트에서 `[머리표]` 절만 잘라낸다 — 다음 `[`로 시작하는 줄 직전까지.
fn extract_plan_section(plan: &str, header: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in plan.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(header) {
            inside = true;
            let rest = rest.trim();
            if !rest.is_empty() {
                out.push(rest);
            }
            continue;
        }
        if inside {
            if trimmed.starts_with('[') {
                break;
            }
            out.push(line);
        }
    }
    out.join("\n").trim().to_string()
}

async fn run_pipeline_inner(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    yes: bool,
    cli_provider: Option<&str>,
    resume: Option<Vec<Message>>,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    run_context
        .metrics()
        .set_context_window(binding.context_window);
    if run_context.is_cancelled() {
        return Ok(cancelled_outcome());
    }
    let role = cfg
        .file
        .subagents
        .get(&binding.profile_name)
        .map(|s| s.model_role.as_str())
        .unwrap_or("main");
    let order = fallback_order_pinned(cfg, &binding.provider_name, cli_provider);
    let lessons_block = if cfg.file.memory.enabled {
        Db::open(&Db::db_path()?)
            .ok()
            .map(|db| {
                crate::lessons::inject_block_for_project(
                    &db,
                    &cfg.workspace,
                    task,
                    cfg.file.memory.inject_limit_chars as usize,
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    if !lessons_block.is_empty() {
        crate::applog::info(&format!("lessons inject:\n{lessons_block}"));
        crate::graph::node_in(
            &run_context,
            "pre_step",
            "lessons",
            "injected",
            Some("bind"),
        );
    } else {
        crate::graph::node_in(&run_context, "pre_step", "lessons", "none", Some("bind"));
    }
    let mut system = system_prompt(cfg, &binding.system_extra, &lessons_block);
    crate::context::record_system_sources(&run_context, cfg, &system, &lessons_block);
    system.push_str(&format!(
        "\n\n[현재 실행 정보]\nProvider: {}\nModel: {}\nContext window: {} tokens\n\
         사용자가 현재 provider, model, context window를 물으면 이 값을 그대로 답한다.",
        binding.provider_name, binding.model, binding.context_window
    ));

    // 난이도 기반 단계별 실행 (dsh ctx.goals 영향 수용):
    // 단순 업무는 즉답, medium 이상은 todo 스테이징. force_staged 엔진(deepseek)은
    // 모든 도구 작업에 적용. 엔진 차이는 EngineSpec 데이터로만 표현한다.
    let (engine_name, _legacy_self) = crate::engine::normalize(&cfg.file.general.engine);
    let spec = crate::engine::resolve_with(&cfg.file.engines, &engine_name);
    // Self-Harness 는 엔진 위에 겹치는 메타 레이어다 — legacy engine="self" 또는
    // [self_harness] meta = true. 관찰 경로와 같은 판정을 쓴다.
    let self_meta_on = crate::self_harness::meta_active(cfg);
    // 실행 분야 — 엔진(품질 장치)과 직교하는 축이다. 제어 전략만 바꾼다.
    let discipline = crate::engine::normalize_discipline(&cfg.file.general.discipline);
    // 그래프 분야는 도구를 쓰는 설계·개발 작업에서만 발동한다. 그 밖은 harness 와 동일.
    let graph_mode = discipline == crate::engine::Discipline::Graph
        && matches!(binding.class, TaskClass::Dev | TaskClass::Advanced)
        && !binding.tools.is_empty();
    // 그래프는 노드가 단계 역할을 하므로 전역 todo 스테이징을 강제하지 않는다.
    let staged = !binding.tools.is_empty()
        && (spec.force_staged || binding.class != crate::harness::TaskClass::Simple)
        && !graph_mode;
    let mut system = system;
    if staged {
        // 배너 없이 조용히 — 단계 진행은 Todo 패널이 보여준다 (pi 저소음).
        crate::tools_more::clear_todos_in(&run_context);
        crate::graph::node_in(
            &run_context,
            "pre_step",
            "staging",
            if spec.force_staged {
                spec.name.as_ref()
            } else {
                "auto"
            },
            Some("bind"),
        );
        system.push_str(
            "\n\n[실행 방식 — 단계별 처리]\n\
             이 작업은 여러 단계가 필요하다. 다른 도구를 쓰기 전에 먼저 todo_write 로 2~6개의 실행 단계를 등록하고, \
             한 단계를 마칠 때마다 todo_write 로 상태를 갱신하라. \
             모든 단계가 끝나면 단계별 핵심 결과를 짧게 요약해 답을 마친다.",
        );
    }
    // 엔진 프롬프트 블록 — 각 하네스의 품질 장치를 한 지점에서 주입한다.
    if !spec.prompt_block.is_empty() {
        system.push_str(&spec.prompt_block);
    }
    // loop 분야 — 종료 조건을 명시해 조기 완료 선언을 막는다 (Ralph 루프 계열).
    if discipline == crate::engine::Discipline::Loop {
        system.push_str(LOOP_DISCIPLINE_RULE);
    }
    // Self-Harness (arXiv:2606.09498) — 자기개선 루프가 유지하는 하네스 상태를
    // 시스템 프롬프트와 런타임 제어에 반영한다. 상태는 에피소드 관찰이 갱신한다.
    // 엔진 지시 뒤에 붙어 학습된 지시가 우선하게 한다.
    let mut effective_max_iter = binding.max_iterations;
    // 계획 호출 전용 면 — 메타 레이어가 켜졌을 때만 채워져 plan_system_prompt 로 넘어간다.
    let mut sh_plan_instruction = String::new();
    if self_meta_on {
        let sh = crate::self_harness::SelfHarnessState::load();
        crate::ui::live_line_in(
            &run_context,
            &format!(
                "[하네스] self-harness v{} — 자기개선 루프 활성{}",
                sh.version,
                sh.trial
                    .as_ref()
                    .map(|t| format!(" · trial #{} 검증 중", t.candidate_id))
                    .unwrap_or_default()
            ),
        );
        crate::graph::node_in(
            &run_context,
            "pre_step",
            "self_harness",
            &format!("v{}", sh.version),
            Some("bind"),
        );
        sh.decorate_system(&mut system);
        sh_plan_instruction = sh.plan_instruction();
        if let Some(cap) = sh.effective_iter_cap() {
            effective_max_iter = effective_max_iter.min(cap).max(1);
        }
    }

    // 계획 단계 — 메인 system 을 그대로 이어받아 lessons·system_extra·프로젝트 규칙·
    // 엔진 지시가 계획에도 반영되게 한다 (system 을 통째로 교체하던 결함 수정).
    // Contract 깊이는 dev/advanced 클래스에서만 활성하고 그 밖은 Brief 로 낮춘다.
    let plan_depth = match spec.plan_depth {
        crate::engine::PlanDepth::Contract
            if !matches!(binding.class, TaskClass::Dev | TaskClass::Advanced) =>
        {
            crate::engine::PlanDepth::Brief
        }
        other => other,
    };
    let contract_plan = plan_depth == crate::engine::PlanDepth::Contract;
    // 계획이 산출한 DoD 체크리스트 — 독립 검증자 게이트(§5)의 입력.
    let mut dod_checklist = String::new();
    // Contract 계획이 실제로 나왔을 때만 첫 사용자 메시지에 todo 시드 지시를 붙인다.
    let mut seed_todo_from_plan = false;
    // graph 분야가 실행할 노드 DAG — 계획이 형식을 지켰을 때만 채워진다.
    let mut dag: Option<(Vec<DagNode>, Vec<usize>)> = None;
    // 그래프는 계획 산출물(DAG) 없이는 성립하지 않으므로 계획 호출을 반드시 지난다.
    if (binding.plan_first || graph_mode) && plan_depth != crate::engine::PlanDepth::Off {
        let plan_budget: u32 = if contract_plan || graph_mode {
            2048
        } else {
            1024
        };
        let req = ChatRequest {
            model: binding.model.clone(),
            system: if graph_mode {
                plan_system_prompt_with(&system, PLAN_GRAPH_INSTRUCTION, &sh_plan_instruction)
            } else {
                plan_system_prompt(&system, plan_depth, &sh_plan_instruction)
            },
            messages: vec![Message::user_text(task)],
            tools: vec![],
            max_tokens: plan_budget,
            stream: false,
        };
        match chat_with_fallback(cfg, &order, role, req).await {
            Ok((_n, resp)) => {
                crate::graph::node_in(&run_context, "plan", "plan_first", "", Some("pre_step"));
                crate::ui::live_line_in(&run_context, "[계획]");
                for b in &resp.content {
                    if let ContentBlock::Text { text } = b {
                        crate::ui::live_assistant_in(
                            &run_context,
                            &format!("[모델 작업]\n{text}\n[/모델 작업]"),
                        );
                    }
                }
                let plan = resp
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.trim()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !plan.is_empty() {
                    crate::context::record_plan(&run_context, &plan, plan_budget);
                    run_context.emit(
                        crate::run::RunEventKind::Plan,
                        serde_json::json!({"plan": plan}),
                    );
                    system.push_str("\n\n[실행 계획]\n");
                    system.push_str(&plan);
                    system.push_str(
                        "\n이 계획을 실행 상태의 기준으로 사용하되, 새 증거가 생기면 안전하게 조정하라.",
                    );
                    if graph_mode {
                        // DAG JSON 과 별개로 [완료 기준] 절도 요구한다 — 게이트의 입력.
                        dod_checklist = extract_plan_section(plan_prose(&plan), "[완료 기준]");
                        dag = match parse_dag(&plan) {
                            Some(nodes) => match topo_order(&nodes) {
                                Ok(order) => Some((nodes, order)),
                                Err(cycle) => {
                                    crate::ui::live_warn_in(
                                        &run_context,
                                        &format!(
                                            "그래프 폴백: {cycle} — 기본 파이프라인으로 진행합니다."
                                        ),
                                    );
                                    None
                                }
                            },
                            None => {
                                crate::ui::live_warn_in(
                                    &run_context,
                                    "그래프 폴백: 계획에서 노드 DAG 를 읽지 못했습니다 — 기본 파이프라인으로 진행합니다.",
                                );
                                None
                            }
                        };
                    } else if contract_plan {
                        dod_checklist = extract_plan_section(&plan, "[완료 기준]");
                        seed_todo_from_plan =
                            !extract_plan_section(&plan, "[작업 분해]").is_empty();
                    }
                }
            }
            Err(e) => {
                crate::ui::live_warn_in(&run_context, &format!("계획 단계 실패(계속 진행): {e}"))
            }
        }
        let _ =
            run_context.transition_lifecycle(crate::lifecycle::LifecycleEventData::RunStarted {
                model: Some(binding.model.clone()),
            });
    }

    let use_tools = !binding.tools.is_empty();
    if !use_tools {
        let mut messages = resume.unwrap_or_else(|| vec![Message::user_text(task)]);
        messages = crate::packer::pack_messages(
            &messages,
            &system,
            &[],
            binding.context_window,
            cfg.file.general.max_tokens,
            cfg.file.general.max_context_chars,
        );
        let req = ChatRequest {
            model: binding.model.clone(),
            system,
            messages: messages.clone(),
            tools: vec![],
            max_tokens: cfg.file.general.max_tokens,
            stream: true,
        };
        let response = stream_with_fallback(cfg, &order, role, req, |piece| {
            crate::ui::live_chunk_in(&run_context, piece);
        });
        tokio::pin!(response);
        let (_name, resp) = tokio::select! {
            result = &mut response => result?,
            _ = run_context.cancelled_reason() => return Ok(cancelled_outcome()),
        };
        let _ = run_context.transition_lifecycle(crate::lifecycle::LifecycleEventData::Tokens {
            input: resp.input_tokens,
            output: resp.output_tokens,
            cached: resp.cached_tokens,
        });
        crate::graph::node_in(
            &run_context,
            "request",
            &binding.model,
            &format!("in={} out={}", resp.input_tokens, resp.output_tokens),
            Some("pre_step"),
        );
        crate::ui::live_chunk_in(&run_context, "\n");
        crate::ui::live_status_in(
            &run_context,
            &format!(
                "[tokens] in={} out={} stop={:?}",
                resp.input_tokens, resp.output_tokens, resp.stop_reason
            ),
        );
        messages.push(Message {
            role: crate::provider::Role::Assistant,
            content: resp.content.clone(),
        });
        let _ =
            run_context.transition_lifecycle(crate::lifecycle::LifecycleEventData::AnswerStarted);
        let hit_token_limit = resp.stop_reason == StopReason::MaxTokens;
        return Ok(AgentOutcome {
            status: if hit_token_limit {
                "incomplete".into()
            } else {
                "ok".into()
            },
            iterations: 1,
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            context_tokens: resp.input_tokens,
            cached_tokens: resp.cached_tokens,
            cache_reported: resp.cache_reported,
            error: hit_token_limit.then(|| "모델 출력 토큰 상한에 도달했습니다.".into()),
            messages,
            changed_files: vec![],
            tool_errors: vec![],
            deny_reasons: vec![],
            verify_fail: None,
        });
    }

    if !cfg.workspace.exists() {
        std::fs::create_dir_all(&cfg.workspace)?;
        crate::ui::live_line_in(
            &run_context,
            &format!(
                "워크스페이스 폴더를 만들었습니다: {}",
                cfg.workspace.display()
            ),
        );
    }

    // Contract 계획의 [작업 분해]를 todo 로 옮겨 staged goal continuation 과 결합한다.
    // 첫 사용자 메시지에만 붙는다 (이어하기에는 resume 이 우선).
    let agent_task = if seed_todo_from_plan {
        format!(
            "{task}\n\n[착수 지시] 위 [작업 분해]의 단계들을 먼저 todo_write 로 등록한 뒤 \
             첫 항목부터 실행하라. 항목을 마칠 때마다 상태를 갱신한다."
        )
    } else {
        task.to_string()
    };

    // 그래프 분야 — 위상순 노드 실행이 전역 goal continuation 루프를 대체한다.
    // 검증·게이트는 그래프 전체가 끝난 뒤 합산 outcome 위에서 1회만 돈다.
    if let Some((nodes, node_order)) = dag {
        crate::graph::node_in(
            &run_context,
            "plan",
            "graph",
            &format!("{}개 노드", nodes.len()),
            Some("plan_first"),
        );
        let mut outcome = run_graph_discipline(
            cfg,
            binding,
            task,
            &nodes,
            &node_order,
            &system,
            yes,
            remote.clone(),
            local_ask.clone(),
            run_context.clone(),
        )
        .await?;
        outcome = finish_verification(
            cfg,
            binding,
            &spec,
            task,
            &dod_checklist,
            yes,
            &system,
            outcome,
            remote,
            local_ask,
            run_context,
        )
        .await?;
        if self_meta_on {
            crate::self_harness::maybe_observe(cfg, task, &outcome);
        }
        return Ok(outcome);
    }

    // loop 분야는 계속 실행 한도를 엔진 값 +4(상한 12)로 늘린다.
    let max_continuations =
        crate::engine::max_continuations_for(discipline, spec.max_continuations);
    let mut next_resume = resume;
    let mut continuations = 0u8;
    let mut stale_rounds = 0u8;
    let mut previous_progress: Option<(usize, usize)> = None;
    let mut total_input = 0u32;
    let mut total_output = 0u32;
    let mut total_iterations = 0u32;
    let mut all_changed = Vec::new();
    let mut all_tool_errors = Vec::new();
    let mut all_denials = Vec::new();
    if staged {
        persist_goal_state(
            &run_context,
            task,
            "active",
            0,
            0,
            0,
            next_resume.as_deref().unwrap_or(&[]),
        );
    }

    let mut outcome = loop {
        let registry = ToolRegistry::with_names(&binding.tools);
        let resume_for_failure = next_resume.clone().unwrap_or_default();
        let run = agent::run_agent_with_context(
            AgentRun {
                cfg,
                provider_name: &binding.provider_name,
                model: &binding.model,
                task: &agent_task,
                yes,
                max_iterations: effective_max_iter,
                system: system.clone(),
                registry,
                resume: next_resume.take(),
                remote: remote.clone(),
                local_ask: local_ask.clone(),
                context_window: binding.context_window,
            },
            run_context.clone(),
        )
        .await;
        let mut current = match run {
            Ok(outcome) => outcome,
            Err(error) => {
                if staged {
                    let progress = crate::tools_more::todo_progress(
                        &crate::tools_more::current_todos_in(&run_context),
                    );
                    persist_goal_state(
                        &run_context,
                        task,
                        "failed",
                        progress.completed,
                        progress.total,
                        continuations,
                        &resume_for_failure,
                    );
                }
                return Err(error);
            }
        };

        total_input = total_input.saturating_add(current.input_tokens);
        total_output = total_output.saturating_add(current.output_tokens);
        total_iterations = total_iterations.saturating_add(current.iterations);
        for path in &current.changed_files {
            if !all_changed.contains(path) {
                all_changed.push(path.clone());
            }
        }
        all_tool_errors.extend(current.tool_errors.clone());
        all_denials.extend(current.deny_reasons.clone());

        let todos = crate::tools_more::current_todos_in(&run_context);
        let progress = crate::tools_more::todo_progress(&todos);
        let signature = (progress.completed, progress.total);
        if previous_progress == Some(signature) {
            stale_rounds = stale_rounds.saturating_add(1);
        } else {
            stale_rounds = 0;
            previous_progress = Some(signature);
        }
        crate::graph::node_in(
            &run_context,
            "goal",
            &format!("{}/{}", progress.completed, progress.total),
            &format!("continuations={continuations} stale={stale_rounds}"),
            Some("request"),
        );
        if staged
            && progress.total > 0
            && progress.completed == progress.total
            && current.status == "limit"
        {
            current.status = "ok".into();
            current.error = None;
        }

        let seeded_missing = staged && progress.total == 0 && continuations == 0;
        let continuation_eligible = matches!(current.status.as_str(), "ok" | "limit");
        let should_continue = continuation_eligible
            && (seeded_missing
                || goal_should_continue(
                    progress.completed,
                    progress.total,
                    stale_rounds,
                    continuations,
                    max_continuations,
                ));
        if !should_continue {
            // todo 를 등록하고도 못 끝낸 경우만 미완료다. todo 자체를 만들지 않고
            // ok 로 끝난 턴은 모델이 단계화가 불필요한 작업으로 판단한 것 —
            // 검증된 산출물이 있으면 완료로 인정한다 (성공을 실패로 오표시 금지).
            if staged
                && progress.total > 0
                && progress.completed < progress.total
                && continuation_eligible
            {
                current.status = "incomplete".into();
                current.error = Some(format!(
                    "목표 미완료: Todo {}/{} · 연속 정체 {}회",
                    progress.completed, progress.total, stale_rounds
                ));
            }
            persist_goal_state(
                &run_context,
                task,
                if current.status == "ok" && progress.completed >= progress.total {
                    "complete"
                } else if current.status == "incomplete" {
                    "blocked"
                } else {
                    "failed"
                },
                progress.completed,
                progress.total,
                continuations,
                &current.messages,
            );
            current.input_tokens = total_input;
            current.output_tokens = total_output;
            current.iterations = total_iterations;
            current.changed_files = all_changed;
            current.tool_errors = all_tool_errors;
            current.deny_reasons = all_denials;
            break current;
        }

        continuations = continuations.saturating_add(1);
        persist_goal_state(
            &run_context,
            task,
            "active",
            progress.completed,
            progress.total,
            continuations,
            &current.messages,
        );
        crate::ui::live_line_in(
            &run_context,
            &format!(
                "[목표 계속] Todo {}/{} · 연속 실행 {continuations}/{max_continuations}",
                progress.completed, progress.total
            ),
        );
        crate::graph::node_in(
            &run_context,
            "goal_continue",
            &format!("cycle {continuations}"),
            &format!("{}/{}", progress.completed, progress.total),
            Some("goal"),
        );
        let mut messages = current.messages;
        let mut nudge = String::from(
            "목표가 아직 완료되지 않았다. 현재 Todo와 도구 결과를 확인하고, \
             완료되지 않은 다음 항목부터 즉시 계속 실행하라. 이미 끝낸 작업은 반복하지 말고, \
             항목을 마칠 때마다 todo_write 상태를 갱신하라. 모든 Todo가 완료된 뒤에만 최종 답변하라.",
        );
        // loop 분야는 정체를 감지한 그 사이클에서 바로 전략 전환을 지시한다
        // (기본 분야는 stale 2회에서 루프를 끊는 기존 동작 그대로).
        if discipline == crate::engine::Discipline::Loop && stale_rounds > 0 {
            nudge.push_str(LOOP_STALE_SWITCH);
        }
        messages.push(Message::user_text(nudge));
        next_resume = Some(messages);
    };

    outcome = finish_verification(
        cfg,
        binding,
        &spec,
        task,
        &dod_checklist,
        yes,
        &system,
        outcome,
        remote.clone(),
        local_ask.clone(),
        run_context.clone(),
    )
    .await?;
    // 도구 없는 프로파일(quick)의 응답에 tool-call 텍스트가 새어 있으면 —
    // 분류가 낮게 잡혔지만 실제로는 도구가 필요한 작업이라는 뜻이다. 모델이
    // 도구 문법을 텍스트로 흉내 낸 오염 응답을 걷어내고 coder 로 1회 승격해
    // 다시 실행한다. (승격된 바인딩은 tools 가 비지 않으므로 재귀는 1회로 끝.)
    if binding.tools.is_empty() && leaked_tool_call(&agent::assistant_text(&outcome.messages)) {
        crate::ui::live_line_in(
            &run_context,
            "도구가 필요한 작업으로 판단 — coder 로 승격해 다시 실행합니다.",
        );
        if let Ok(dev) = bind(cfg, TaskClass::Dev, cli_provider, None)
            && !dev.tools.is_empty()
        {
            let mut clean = outcome.messages.clone();
            while matches!(clean.last(), Some(m) if m.role == crate::provider::Role::Assistant) {
                clean.pop();
            }
            return Box::pin(run_pipeline_inner(
                cfg,
                &dev,
                task,
                yes,
                cli_provider,
                Some(clean),
                remote,
                local_ask,
                run_context,
            ))
            .await;
        }
    }
    // Self-Harness 에피소드 관찰 — TUI/CLI/텔레그램 세 진입 경로가 모두 여기를
    // 지나므로 이 지점 하나로 전 경로의 실행 증거가 수집된다. 백그라운드 실행.
    if self_meta_on {
        crate::self_harness::maybe_observe(cfg, task, &outcome);
    }
    Ok(outcome)
}

/// 종료 검증부 — 검증 실행 + 독립 검증자 게이트. harness 루프와 graph 실행이
/// 같은 지점을 지나도록 한 함수로 모았다 (그래프는 전체 종료 후 1회만 지난다).
#[allow(clippy::too_many_arguments)]
async fn finish_verification(
    cfg: &Config,
    binding: &Binding,
    spec: &crate::engine::EngineSpec,
    task: &str,
    dod: &str,
    yes: bool,
    system: &str,
    mut outcome: AgentOutcome,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    // 검증 강도 — Auto/Strict 는 프로파일의 verify 가 꺼져 있어도 자동 감지 명령으로 검증한다.
    let verify_forced = matches!(
        spec.verify_policy,
        crate::engine::VerifyPolicy::Auto | crate::engine::VerifyPolicy::Strict
    );
    if (binding.verify || verify_forced) && outcome.status != "incomplete" {
        crate::graph::node_in(&run_context, "verify", "start", "", Some("request"));
        crate::spinner::set_label_in(&run_context, "검증 중…");
        outcome = run_verify(
            cfg,
            binding,
            task,
            yes,
            system.to_string(),
            outcome,
            remote.clone(),
            local_ask.clone(),
            run_context.clone(),
        )
        .await?;
        crate::graph::node_in(&run_context, "verify", &outcome.status, "", Some("verify"));
    }
    // 독립 검증자 게이트 (§5) — 자기평가 편향을 막기 위해 신선한 컨텍스트의 리뷰어가
    // 완료 기준과 대조한다. 게이트가 가용성을 해치면 안 되므로 게이트 자체의 실패는
    // 경고 한 줄 후 통과로 취급한다.
    if spec.verify_policy == crate::engine::VerifyPolicy::Strict
        && cfg.file.harness.strict_gate
        && matches!(binding.class, TaskClass::Dev | TaskClass::Advanced)
        && outcome.status == "ok"
        && !run_context.is_cancelled()
    {
        outcome = run_review_gate(
            cfg,
            binding,
            task,
            dod,
            yes,
            system,
            outcome,
            remote,
            local_ask,
            run_context,
        )
        .await;
    }
    Ok(outcome)
}

fn cancelled_outcome() -> AgentOutcome {
    AgentOutcome {
        status: "cancelled".into(),
        error: Some("실행이 취소되었습니다.".into()),
        ..AgentOutcome::default()
    }
}

/// 모델이 도구 호출을 구조체가 아니라 텍스트로 흉내 낸 흔적 —
/// MiniMax 계열의 내부 마커(`]<]`)와 `<tool_call>` JSON 조각을 감지한다.
fn leaked_tool_call(text: &str) -> bool {
    if text.contains("<tool_call>") || text.contains("]<]") {
        return true;
    }
    text.contains("\"name\"") && text.contains("\"arguments\"")
}

fn persist_goal_state(
    run: &RunContext,
    objective: &str,
    status: &str,
    completed: usize,
    total: usize,
    continuations: u8,
    messages: &[Message],
) {
    let Ok(messages_json) = serde_json::to_string(messages) else {
        return;
    };
    if let Ok(path) = Db::db_path()
        && let Ok(db) = Db::open(&path)
    {
        let _ = db.save_goal(&crate::db::GoalRow {
            id: run.run_id().to_string(),
            objective: objective.to_string(),
            status: status.to_string(),
            completed,
            total,
            continuations,
            messages_json,
        });
    }
}

fn goal_should_continue(
    completed: usize,
    total: usize,
    stale_rounds: u8,
    continuations: u8,
    max_continuations: u8,
) -> bool {
    total > 0 && completed < total && stale_rounds < 2 && continuations < max_continuations
}

async fn run_verify(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    yes: bool,
    system: String,
    mut outcome: AgentOutcome,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    let mut cmd = binding.verify_command.clone();
    if cmd.trim().is_empty() {
        cmd = auto_verify_command(cfg, &outcome.changed_files);
    }
    if cmd.is_empty() {
        crate::ui::live_line_in(&run_context, "검증 생략: 자동 감지할 빌드가 없습니다.");
        return Ok(outcome);
    }

    let bash = ToolRegistry::all();
    let Some(tool) = bash.get("bash") else {
        crate::ui::live_line_in(&run_context, "검증 생략: bash 도구가 없습니다.");
        return Ok(outcome);
    };
    let mut ctx = ToolCtx::new(cfg.workspace.clone());
    ctx.vault = Some(crate::config::expand_tilde(&cfg.file.obsidian.vault_path));
    ctx.db_path = crate::config::expand_tilde(&cfg.file.obsidian.db_path);
    ctx.local_ask = local_ask.clone();
    ctx.remote = remote.clone();
    ctx.run = Some(run_context.clone());

    let yes = agent::effective_yes(yes, &remote);
    for round in 0..3 {
        crate::ui::live_line_in(&run_context, &format!("[검증] {cmd}"));
        crate::spinner::set_label_in(&run_context, &format!("검증 실행: {cmd}"));
        let input = serde_json::json!({"command": cmd});
        if tool.needs_approval(&input) && !yes {
            match tools::approval_preview("bash", &input, &ctx) {
                Ok(p) => {
                    crate::ui::live_line_in(&run_context, &p);
                    let denied = if let Some(ask) = &local_ask {
                        !matches!(
                            ask(p.clone()).await,
                            crate::agent::ApprovalChoice::Yes
                                | crate::agent::ApprovalChoice::Always
                        )
                    } else if let Some(r) = &remote {
                        let ask = r.ask.clone();
                        !tokio::time::timeout(r.timeout, (ask)(p.clone()))
                            .await
                            .unwrap_or(false)
                    } else {
                        print!("[y] 이번만  / [n] 거부  / [a] 이번 실행 모두 허용 : ");
                        let _ = io::stdout().flush();
                        let mut line = String::new();
                        io::stdin().read_line(&mut line)?;
                        let t = line.trim().to_lowercase();
                        t == "n" || t == "no"
                    };
                    if denied {
                        crate::ui::live_line_in(&run_context, "검증이 거부되었습니다.");
                        outcome.status = "denied".into();
                        return Ok(outcome);
                    }
                }
                Err(e) => {
                    crate::ui::live_line_in(
                        &run_context,
                        &format!("검증 명령을 실행할 수 없습니다: {e}"),
                    );
                    return Ok(outcome);
                }
            }
        }
        match tool.run(serde_json::json!({"command": cmd}), &ctx) {
            Ok(out) if !out.contains("[exit") => {
                // 성공 시 원문 대신 요약 한 줄 — 실패했을 때만 상세가 필요하다.
                let lines = out.trim().lines().count();
                crate::ui::live_line_in(&run_context, &format!("검증 성공 ({lines}줄 출력)"));
                return Ok(outcome);
            }
            other => {
                let err = match other {
                    Ok(o) => o,
                    Err(e) => e.to_string(),
                };
                if round >= 2 {
                    crate::ui::live_line_in(&run_context, "검증이 2회 재시도 후에도 실패했습니다.");
                    crate::ui::live_line_in(&run_context, &err);
                    outcome.status = "fail".into();
                    outcome.error = Some(err.chars().take(500).collect());
                    outcome.verify_fail = Some(err.chars().take(500).collect());
                    return Ok(outcome);
                }
                crate::ui::live_line_in(
                    &run_context,
                    &format!("검증 실패, 오류를 되먹여 재시도합니다 ({}/2)", round + 1),
                );
                let cause: String = err.chars().take(500).collect();
                let mut msgs = outcome.messages.clone();
                if msgs.is_empty() {
                    msgs.push(Message::user_text(task));
                }
                msgs.push(Message::user_text(format!(
                    "검증 명령이 실패했습니다. 오류를 고치세요.\n{err}"
                )));
                let mut next = agent::run_agent_with_context(
                    AgentRun {
                        cfg,
                        provider_name: &binding.provider_name,
                        model: binding.verify_model.as_deref().unwrap_or(&binding.model),
                        task,
                        yes,
                        max_iterations: binding.max_iterations,
                        system: system.clone(),
                        registry: ToolRegistry::with_names(&binding.tools),
                        resume: Some(msgs),
                        remote: remote.clone(),
                        local_ask: local_ask.clone(),
                        context_window: binding.context_window,
                    },
                    run_context.clone(),
                )
                .await?;
                if next.verify_fail.is_none() {
                    next.verify_fail = Some(cause);
                }
                outcome = next;
            }
        }
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// 독립 검증자 게이트 (VerifyPolicy::Strict) — 근거: K2 검증자 선택, fresh-context 리뷰어 노드
// ---------------------------------------------------------------------------

/// 게이트 미니 루프 상한 — 리뷰어는 읽고 판정만 한다.
const REVIEW_GATE_MAX_ITER: u32 = 6;
/// 리뷰 피드백 보존 상한 (재개 메시지에 그대로 실린다).
const REVIEW_SUMMARY_CAP: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReviewVerdict {
    Pass,
    Fail { summary: String },
}

/// 리뷰어 출력에서 판정을 뽑는 순수 함수.
/// `[판정]` 줄이 없으면 통과로 본다 — 게이트가 가용성을 해치면 안 된다.
fn parse_review_verdict(text: &str) -> ReviewVerdict {
    let mut failed = None;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("[판정]") else {
            continue;
        };
        let v = rest.trim().to_ascii_lowercase();
        // '미통과'가 '통과'를 포함하므로 실패 표지를 먼저 본다.
        failed = Some(
            v.contains("fail")
                || v.contains("미통과")
                || v.contains("불합격")
                || v.contains("실패"),
        );
        break;
    }
    if failed != Some(true) {
        return ReviewVerdict::Pass;
    }
    let mut summary = String::new();
    for header in ["[미충족 항목]", "[결함]"] {
        let section = extract_plan_section(text, header);
        let section = section.trim();
        if section.is_empty() || section == "없음" {
            continue;
        }
        if !summary.is_empty() {
            summary.push('\n');
        }
        summary.push_str(header);
        summary.push(' ');
        summary.push_str(section);
    }
    if summary.is_empty() {
        // 구조를 지키지 않은 출력 — 본문을 그대로 근거로 쓴다.
        summary = text.trim().to_string();
    }
    ReviewVerdict::Fail {
        summary: summary.chars().take(REVIEW_SUMMARY_CAP).collect(),
    }
}

/// 판정 뒤 게이트가 취할 동작 — 재개는 1회만, 2번째 미통과면 기록 후 종료한다.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GateAction {
    /// 통과 — outcome 그대로 완료.
    Accept,
    /// 지적을 되먹여 본 작업을 1회 재개한다.
    Resume(String),
    /// 재개 후에도 미통과 — status 는 유지하고 error 에만 사유를 남긴다.
    Report(String),
}

/// 게이트 상태 전이 (순수 함수). `attempt` 는 0-based 검증 회차.
fn gate_action(verdict: ReviewVerdict, attempt: u8) -> GateAction {
    match verdict {
        ReviewVerdict::Pass => GateAction::Accept,
        ReviewVerdict::Fail { summary } if attempt == 0 => GateAction::Resume(summary),
        ReviewVerdict::Fail { summary } => GateAction::Report(summary),
    }
}

/// 미통과 사유 한 줄 요약 — outcome.error 와 진행 표시에 쓴다.
fn verdict_headline(summary: &str) -> String {
    summary
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("사유 미상")
        .chars()
        .take(120)
        .collect()
}

/// 검증자 입력 — 원 작업 + 완료 기준 + 변경 파일 목록.
/// diff 전문은 넣지 않는다: 리뷰어가 도구로 직접 읽어야 신선한 시각이 유지된다.
fn review_prompt(task: &str, dod: &str, changed: &[String]) -> String {
    let mut s = String::from("아래 작업의 산출물을 완료 기준과 대조해 판정하라.\n\n[원 작업]\n");
    s.extend(task.trim().chars().take(4000));
    let dod = dod.trim();
    if !dod.is_empty() {
        s.push_str("\n\n[완료 기준]\n");
        s.extend(dod.chars().take(4000));
    }
    s.push_str("\n\n[변경된 파일]\n");
    if changed.is_empty() {
        s.push_str("(도구가 보고한 변경 파일 없음)\n");
    } else {
        for path in changed.iter().take(40) {
            s.push_str("- ");
            s.push_str(path);
            s.push('\n');
        }
    }
    s.push_str(
        "\n변경 내용은 첨부하지 않았다. read_file·grep 으로 직접 읽어 확인하고, 필요하면 \
         bash 로 빌드·테스트를 실행하라. 확인하지 않은 파일에 대해서는 판정하지 않는다(NEVER).",
    );
    s
}

/// 리뷰어 미니 루프 1회. 게이트 자체의 오류는 호출자가 통과로 처리한다.
async fn run_review_once(
    cfg: &Config,
    reviewer: &Binding,
    prompt: &str,
    yes: bool,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    agent::run_agent_with_context(
        AgentRun {
            cfg,
            provider_name: &reviewer.provider_name,
            model: &reviewer.model,
            task: prompt,
            yes,
            max_iterations: reviewer.max_iterations.min(REVIEW_GATE_MAX_ITER).max(1),
            // 신선한 컨텍스트: 본 작업의 대화·lessons 를 물려받지 않는다.
            system: system_prompt(cfg, &reviewer.system_extra, ""),
            registry: ToolRegistry::with_names(&reviewer.tools),
            resume: None,
            remote,
            local_ask,
            context_window: reviewer.context_window,
        },
        run_context,
    )
    .await
}

/// 독립 검증자 게이트 — pass 면 그대로, fail 이면 지적을 되먹여 1회 재개 후 재검증.
/// 2번째 fail 이면 status 는 유지하고 error 에만 사유를 남긴다 (무한루프 방지).
#[allow(clippy::too_many_arguments)]
async fn run_review_gate(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    dod: &str,
    yes: bool,
    system: &str,
    mut outcome: AgentOutcome,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> AgentOutcome {
    // 엔진 고정이 걸린 실행은 manual_verify 를 무시하고 고정 프로바이더의 main 모델로
    // 리뷰한다 (§11.2) — 게이트의 본질은 신선한 컨텍스트이므로 같은 모델이어도 유효하다.
    let pin = engine_pin(cfg).filter(|p| pin_unavailable(cfg, p, true).is_none());
    // [harness] manual_verify 가 지정돼 있으면 그 모델로 검증자를 돌린다.
    let verify_pair = if pin.is_some() {
        None
    } else {
        cfg.file
            .harness
            .manual_verify
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|spec| resolve_spec(cfg, spec, true).ok())
    };
    let reviewer = match bind_profile(
        cfg,
        binding.class,
        Some("reviewer"),
        pin.as_deref()
            .or(verify_pair.as_ref().map(|(p, _)| p.as_str())),
        verify_pair.as_ref().map(|(_, m)| m.as_str()),
    ) {
        Ok(reviewer) => reviewer,
        Err(e) => {
            crate::ui::live_warn_in(
                &run_context,
                &format!("검증자 게이트 생략(바인딩 실패): {e}"),
            );
            return outcome;
        }
    };

    for attempt in 0..2u8 {
        crate::graph::node_in(
            &run_context,
            "critic",
            "start",
            &format!("{} · {}회차", reviewer.model, attempt + 1),
            Some("verify"),
        );
        crate::spinner::set_label_in(&run_context, "독립 검증자 대조 중…");
        crate::ui::live_line_in(
            &run_context,
            &format!("[검증자] {} — 완료 기준 대조", reviewer.model),
        );
        let prompt = review_prompt(task, dod, &outcome.changed_files);
        let review = match run_review_once(
            cfg,
            &reviewer,
            &prompt,
            yes,
            remote.clone(),
            local_ask.clone(),
            run_context.clone(),
        )
        .await
        {
            Ok(review) => review,
            Err(e) => {
                crate::ui::live_warn_in(
                    &run_context,
                    &format!("검증자 게이트 실패(통과 처리): {e}"),
                );
                crate::graph::node_in(
                    &run_context,
                    "critic",
                    "error",
                    &e.to_string(),
                    Some("verify"),
                );
                return outcome;
            }
        };
        outcome.input_tokens = outcome.input_tokens.saturating_add(review.input_tokens);
        outcome.output_tokens = outcome.output_tokens.saturating_add(review.output_tokens);

        let verdict = parse_review_verdict(&agent::assistant_text(&review.messages));
        let summary = match gate_action(verdict, attempt) {
            GateAction::Accept => {
                crate::graph::node_in(&run_context, "critic", "pass", "", Some("verify"));
                crate::ui::live_line_in(&run_context, "[검증자] 완료 기준 충족 — 통과");
                return outcome;
            }
            GateAction::Report(summary) => {
                let headline = verdict_headline(&summary);
                crate::graph::node_in(&run_context, "critic", "fail", &headline, Some("verify"));
                crate::ui::live_warn_in(
                    &run_context,
                    &format!("[검증자] 2회 미통과 — 상태를 보고하고 종료합니다: {headline}"),
                );
                outcome.error = Some(format!("검증자 미통과: {headline}"));
                return outcome;
            }
            GateAction::Resume(summary) => summary,
        };
        let headline = verdict_headline(&summary);
        crate::graph::node_in(&run_context, "critic", "fail", &headline, Some("verify"));
        crate::ui::live_line_in(
            &run_context,
            &format!("[검증자] 미통과 — 지적을 되먹여 1회 재개합니다: {headline}"),
        );

        // 재개는 goal 루프를 다시 돌리지 않고 본 작업 바인딩으로 1회만 이어 실행한다.
        let mut messages = outcome.messages.clone();
        if messages.is_empty() {
            messages.push(Message::user_text(task));
        }
        messages.push(Message::user_text(format!(
            "독립 검증자가 완료 기준 대조에서 미통과로 판정했다. 아래 지적을 실제 파일 수정으로 \
             해소하라. 재설명·변명은 금지(NEVER)다. 고친 뒤 스스로 확인하고 무엇을 바꿨는지 보고하라.\n{summary}"
        )));
        let resumed = agent::run_agent_with_context(
            AgentRun {
                cfg,
                provider_name: &binding.provider_name,
                model: &binding.model,
                task,
                yes,
                max_iterations: binding.max_iterations,
                system: system.to_string(),
                registry: ToolRegistry::with_names(&binding.tools),
                resume: Some(messages),
                remote: remote.clone(),
                local_ask: local_ask.clone(),
                context_window: binding.context_window,
            },
            run_context.clone(),
        )
        .await;
        match resumed {
            Ok(mut next) => {
                next.input_tokens = outcome.input_tokens.saturating_add(next.input_tokens);
                next.output_tokens = outcome.output_tokens.saturating_add(next.output_tokens);
                next.iterations = outcome.iterations.saturating_add(next.iterations);
                let mut changed = outcome.changed_files.clone();
                for path in &next.changed_files {
                    if !changed.contains(path) {
                        changed.push(path.clone());
                    }
                }
                next.changed_files = changed;
                let mut tool_errors = outcome.tool_errors.clone();
                tool_errors.extend(next.tool_errors.clone());
                next.tool_errors = tool_errors;
                let mut denials = outcome.deny_reasons.clone();
                denials.extend(next.deny_reasons.clone());
                next.deny_reasons = denials;
                outcome = next;
                // 재개가 정상 종료하지 못했으면 재검증 없이 그 상태를 그대로 보고한다.
                if outcome.status != "ok" || run_context.is_cancelled() {
                    crate::ui::live_warn_in(
                        &run_context,
                        &format!(
                            "[검증자] 재개가 완료되지 않아 재검증을 생략합니다 (status={})",
                            outcome.status
                        ),
                    );
                    return outcome;
                }
            }
            Err(e) => {
                crate::ui::live_warn_in(
                    &run_context,
                    &format!("검증자 재개 실패(직전 상태 유지): {e}"),
                );
                outcome.error = Some(format!("검증자 미통과: {headline}"));
                return outcome;
            }
        }
    }
    outcome
}

// ---------------------------------------------------------------------------
// 그래프 분야 (discipline = graph) — PEV 상태 그래프를 위상순으로 순차 실행한다.
// 노드마다 신선한 컨텍스트로 돌고, 선행 노드에서는 결론(산출물 요약)만 넘겨받는다
// (서브에이전트 격리 원칙). 병렬 실행은 이번 범위 밖이다.
// ---------------------------------------------------------------------------

/// 선행 노드 산출물 요약 상한 — 다음 노드로 결론만 넘긴다.
const GRAPH_SUMMARY_CAP: usize = 500;
/// 노드 하나의 최소 반복 상한.
const GRAPH_NODE_MIN_ITER: u32 = 8;

/// 계획이 산출한 DAG 노드 하나.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
struct DagNode {
    id: String,
    goal: String,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    produces: String,
}

#[derive(Debug, serde::Deserialize)]
struct DagPlan {
    nodes: Vec<DagNode>,
}

/// deps 에 순환이 있어 위상 정렬이 불가능함 — 그래프 폴백 사유.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CycleError {
    /// 정렬되지 못하고 남은 노드 id 들.
    remaining: Vec<String>,
}

impl std::fmt::Display for CycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "노드 순환: {}", self.remaining.join(" → "))
    }
}

/// 계획 텍스트에서 DAG JSON 앞의 산문만 — [완료 기준] 추출이 JSON 을 빨아들이지 않게.
/// 코드 펜스와 첫 `{` 중 먼저 오는 지점에서 자른다.
fn plan_prose(plan: &str) -> &str {
    let cut = [plan.find("```"), plan.find('{')]
        .into_iter()
        .flatten()
        .min();
    match cut {
        Some(i) => &plan[..i],
        None => plan,
    }
}

/// 첫 `{` 부터 짝이 맞는 지점까지. 문자열 리터럴 안의 중괄호·이스케이프는 건너뛴다.
fn balanced_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 응답에서 JSON 객체를 꺼낸다 — ```json 펜스 우선, 없으면 첫 `{` 부터 균형 매칭.
fn extract_json_object(text: &str) -> Option<&str> {
    if let Some(at) = text.find("```json")
        && let Some(rest) = text.get(at + "```json".len()..)
        && let Some(end) = rest.find("```")
        && let Some(obj) = balanced_object(&rest[..end])
    {
        return Some(obj);
    }
    balanced_object(text)
}

/// 계획 응답 → DAG 노드 목록. 형식이 조금이라도 어긋나면 None 을 돌려 harness 로 폴백한다.
fn parse_dag(text: &str) -> Option<Vec<DagNode>> {
    let json = extract_json_object(text)?;
    let plan: DagPlan = serde_json::from_str(json).ok()?;
    if plan.nodes.is_empty() {
        return None;
    }
    let mut seen: Vec<&str> = Vec::with_capacity(plan.nodes.len());
    for node in &plan.nodes {
        let id = node.id.trim();
        // 빈 id·빈 goal·중복 id 는 실행 순서를 정의할 수 없다 — 형식 오류로 본다.
        if id.is_empty() || node.goal.trim().is_empty() || seen.contains(&id) {
            return None;
        }
        seen.push(id);
    }
    Some(plan.nodes)
}

/// Kahn 위상 정렬 → 실행 순서(노드 인덱스). 모르는 id 를 가리키는 deps 는 간선이 없다.
fn topo_order(nodes: &[DagNode]) -> Result<Vec<usize>, CycleError> {
    let index: std::collections::HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.trim(), i))
        .collect();
    let mut indegree = vec![0usize; nodes.len()];
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        let mut linked: Vec<usize> = Vec::new();
        for dep in &node.deps {
            let Some(&j) = index.get(dep.trim()) else {
                continue;
            };
            if linked.contains(&j) {
                continue; // 같은 deps 가 두 번 적혀도 진입차수를 두 번 세지 않는다.
            }
            linked.push(j);
            edges[j].push(i);
            indegree[i] += 1;
        }
    }
    let mut queue: std::collections::VecDeque<usize> =
        (0..nodes.len()).filter(|i| indegree[*i] == 0).collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(i) = queue.pop_front() {
        order.push(i);
        for &next in &edges[i] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                queue.push_back(next);
            }
        }
    }
    if order.len() != nodes.len() {
        return Err(CycleError {
            remaining: nodes
                .iter()
                .enumerate()
                .filter(|(i, _)| !order.contains(i))
                .map(|(_, n)| n.id.trim().to_string())
                .collect(),
        });
    }
    Ok(order)
}

/// 노드 성공 판정 — 등록한 todo 를 모두 끝내고 반복 상한에 닿은 경우는 구제한다
/// (전역 goal 루프와 같은 규칙).
fn graph_node_ok(status: &str, completed: usize, total: usize) -> bool {
    status == "ok" || (status == "limit" && total > 0 && completed == total)
}

/// 마지막 assistant 텍스트 — 노드 산출물 요약의 원문.
fn last_assistant_text(messages: &[Message]) -> String {
    for m in messages.iter().rev() {
        if m.role != crate::provider::Role::Assistant {
            continue;
        }
        let text = m
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.trim()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

/// 다음 노드로 넘길 결론 — 마지막 답변 앞 500자 + 이 노드가 바꾼 파일 목록.
fn graph_node_summary(outcome: &AgentOutcome) -> String {
    let mut s: String = last_assistant_text(&outcome.messages)
        .chars()
        .take(GRAPH_SUMMARY_CAP)
        .collect();
    if !outcome.changed_files.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str("변경 파일: ");
        s.push_str(&outcome.changed_files.join(", "));
    }
    if s.is_empty() {
        s.push_str("(보고된 산출물 없음)");
    }
    s
}

/// 노드 실행용 시스템 프롬프트 — 메인 조립 결과 + 이 노드의 좌표 + 선행 산출물.
fn graph_node_system(
    system: &str,
    step: usize,
    total: usize,
    node: &DagNode,
    produced: &[(String, String)],
) -> String {
    let mut s = String::with_capacity(system.len() + 512);
    s.push_str(system);
    s.push_str(&format!(
        "\n\n[그래프 노드 {step}/{total}] 목표: {}\n",
        node.goal.trim()
    ));
    let produces = node.produces.trim();
    if !produces.is_empty() {
        s.push_str(&format!("이 노드의 산출물: {produces}\n"));
    }
    for dep in &node.deps {
        let Some((_, summary)) = produced.iter().find(|(id, _)| id == dep.trim()) else {
            continue;
        };
        s.push_str(&format!("\n[선행 산출물 {}]\n{summary}\n", dep.trim()));
    }
    s.push_str(
        "\n이 노드의 목표만 수행한다. 다른 노드가 맡은 범위는 건드리지 않는다(NEVER). \
         끝내면 무엇을 만들었는지 한 문단으로 보고한다.",
    );
    s
}

/// 노드 실행용 사용자 프롬프트 — 원 작업 + 이 노드의 목표.
fn graph_node_prompt(task: &str, node: &DagNode) -> String {
    format!(
        "{task}\n\n[이번 노드] {}\n이 노드의 목표만 수행하라. 나머지는 다른 노드가 처리한다.",
        node.goal.trim()
    )
}

/// 노드 하나의 실행 결과를 그래프 합산본에 누적한다 (실패한 시도도 보존).
fn merge_node_outcome(agg: &mut AgentOutcome, node: &AgentOutcome) {
    agg.input_tokens = agg.input_tokens.saturating_add(node.input_tokens);
    agg.output_tokens = agg.output_tokens.saturating_add(node.output_tokens);
    agg.iterations = agg.iterations.saturating_add(node.iterations);
    agg.context_tokens = node.context_tokens;
    agg.cached_tokens = agg.cached_tokens.saturating_add(node.cached_tokens);
    agg.cache_reported = agg.cache_reported || node.cache_reported;
    for path in &node.changed_files {
        if !agg.changed_files.contains(path) {
            agg.changed_files.push(path.clone());
        }
    }
    agg.tool_errors.extend(node.tool_errors.clone());
    agg.deny_reasons.extend(node.deny_reasons.clone());
    if node.verify_fail.is_some() {
        agg.verify_fail = node.verify_fail.clone();
    }
    agg.messages = node.messages.clone();
}

/// 위상순 노드 실행. 노드는 신선한 messages 로 돌고, 실패하면 사유를 덧붙여 1회만
/// 재시도한다. 재실패면 그래프를 중단하고 그때까지의 산출물을 합산해 돌려준다.
#[allow(clippy::too_many_arguments)]
async fn run_graph_discipline(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    nodes: &[DagNode],
    order: &[usize],
    system: &str,
    yes: bool,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    let total = order.len();
    let node_iter = (binding.max_iterations / 2).max(GRAPH_NODE_MIN_ITER);
    let mut produced: Vec<(String, String)> = Vec::with_capacity(total);
    let mut agg = AgentOutcome::default();
    for (step, &i) in order.iter().enumerate() {
        if run_context.is_cancelled() {
            return Ok(cancelled_outcome());
        }
        let node = &nodes[i];
        let id = node.id.trim();
        let goal_head: String = node.goal.trim().chars().take(120).collect();
        crate::graph::node_in(
            &run_context,
            "graph_node",
            id,
            &goal_head,
            Some("plan_first"),
        );
        crate::ui::live_line_in(
            &run_context,
            &format!("[그래프] 노드 {}/{} {id} — {goal_head}", step + 1, total),
        );
        crate::spinner::set_label_in(
            &run_context,
            &format!("그래프 노드 {}/{}: {id}", step + 1, total),
        );
        // 노드마다 todo 를 비운다 — 앞 노드가 남긴 항목이 이 노드의 완료 판정을 흐리지 않게.
        crate::tools_more::clear_todos_in(&run_context);
        let node_system = graph_node_system(system, step + 1, total, node, &produced);
        let mut prompt = graph_node_prompt(task, node);
        for attempt in 0..2u8 {
            let outcome = agent::run_agent_with_context(
                AgentRun {
                    cfg,
                    provider_name: &binding.provider_name,
                    model: &binding.model,
                    task: &prompt,
                    yes,
                    max_iterations: node_iter,
                    system: node_system.clone(),
                    registry: ToolRegistry::with_names(&binding.tools),
                    // 신선한 컨텍스트: 앞 노드의 대화를 물려받지 않는다.
                    resume: None,
                    remote: remote.clone(),
                    local_ask: local_ask.clone(),
                    context_window: binding.context_window,
                },
                run_context.clone(),
            )
            .await?;
            let progress = crate::tools_more::todo_progress(&crate::tools_more::current_todos_in(
                &run_context,
            ));
            let ok = graph_node_ok(&outcome.status, progress.completed, progress.total);
            let reason = outcome
                .error
                .clone()
                .unwrap_or_else(|| format!("status={}", outcome.status));
            merge_node_outcome(&mut agg, &outcome);
            // 취소는 실패가 아니다 — 재시도하지 않고 그대로 끝낸다.
            if outcome.status == "cancelled" || run_context.is_cancelled() {
                return Ok(cancelled_outcome());
            }
            if ok {
                produced.push((id.to_string(), graph_node_summary(&outcome)));
                crate::graph::node_in(&run_context, "graph_node", id, "ok", Some("plan_first"));
                break;
            }
            if attempt == 0 {
                crate::ui::live_warn_in(
                    &run_context,
                    &format!("[그래프] 노드 {id} 실패 — 1회 재시도합니다: {reason}"),
                );
                prompt = format!(
                    "{prompt}\n\n[직전 시도 실패] {reason}\n같은 접근을 반복하지 마라. \
                     막힌 지점을 먼저 확인하고 다른 경로로 이 노드의 목표를 완수하라."
                );
                continue;
            }
            crate::graph::node_in(&run_context, "graph_node", id, "fail", Some("plan_first"));
            crate::ui::live_warn_in(
                &run_context,
                &format!("[그래프] 노드 {id} 재실패 — 그래프를 중단합니다: {reason}"),
            );
            agg.status = outcome.status.clone();
            agg.error = Some(format!(
                "그래프 중단: 노드 {id} 실패 ({}/{total} 노드 완료)",
                produced.len()
            ));
            return Ok(agg);
        }
    }
    agg.status = "ok".into();
    agg.error = None;
    crate::ui::live_line_in(
        &run_context,
        &format!("[그래프] {total}개 노드 완료 — 검증으로 넘어갑니다"),
    );
    Ok(agg)
}

fn auto_verify_command(cfg: &Config, changed: &[String]) -> String {
    if cfg.workspace.join("Cargo.toml").exists() {
        return "cargo check".into();
    }
    let py_changed: Vec<&str> = changed
        .iter()
        .filter(|p| p.ends_with(".py"))
        .map(|s| s.as_str())
        .collect();
    if cfg.workspace.join("pyproject.toml").exists() || !py_changed.is_empty() {
        if py_changed.is_empty() {
            return String::new();
        }
        let files = py_changed.join(" ");
        #[cfg(windows)]
        {
            return format!("python -m py_compile {files}");
        }
        #[cfg(not(windows))]
        {
            return format!("python3 -m py_compile {files}");
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_simple_hello() {
        assert_eq!(classify_rules("안녕", false), TaskClass::Simple);
    }

    #[test]
    fn korean_file_work_is_never_simple() {
        // 실사례: simple(quick, 도구 0개)로 떨어져 모델이 tool call 을 텍스트로
        // 흉내 내던 질문 — 이제 도구 있는 클래스로 분류되어야 한다.
        let q = "그럼 니가 잘 하는 걸 더 잘 하게 만들 수 있게 마크다운 파일을 업그레이드 하면 좋지 않을 까?";
        assert_ne!(classify_rules(q, false), TaskClass::Simple);
        assert_eq!(
            classify_rules("AGENTS.md 업그레이드해줘", false),
            TaskClass::Dev
        );
        assert_ne!(
            classify_rules("워크스페이스에 뭐가 있어?", false),
            TaskClass::Simple
        );
    }

    #[test]
    fn leaked_tool_call_detects_text_tool_syntax() {
        assert!(leaked_tool_call(
            "워크스페이스부터 확인하겠습니다.]<]minimax[>[<tool_call> { \"name\": \"run_command\""
        ));
        assert!(leaked_tool_call(
            "<tool_call>{\"name\":\"x\",\"arguments\":{}}</tool_call>"
        ));
        assert!(!leaked_tool_call("마크다운을 이렇게 고치면 됩니다."));
        assert!(!leaked_tool_call("파일 이름은 name 필드에 적습니다."));
    }

    #[test]
    fn classifies_dev_from_extension() {
        assert_eq!(
            classify_rules("buggy.py 만들어서 고쳐줘", false),
            TaskClass::Dev
        );
    }

    #[test]
    fn classifies_advanced_from_keyword() {
        assert_eq!(
            classify_rules("이 저장소 구조 분석해서 개선 전략 보고서 써줘", false),
            TaskClass::Advanced
        );
    }

    #[test]
    fn obsidian_flag_alone_stays_simple() {
        assert_eq!(classify_rules("안녕", true), TaskClass::Simple);
        assert_eq!(classify_rules("안녕", false), TaskClass::Simple);
        // 노트 관련 키워드는 여전히 medium
        assert_eq!(classify_rules("내 노트 찾아줘", true), TaskClass::Medium);
    }

    #[test]
    fn maps_design_verify_debug_to_existing_classes() {
        assert_eq!(
            classify_rules("시스템 구성안을 설계해줘", false),
            TaskClass::Advanced
        );
        assert_eq!(classify_rules("이 코드 검증해줘", false), TaskClass::Dev);
        assert_eq!(classify_rules("디버깅 좀 도와줘", false), TaskClass::Dev);
    }

    #[test]
    fn auto_pick_falls_back_to_registered_only() {
        let table = crate::ranks::bundled();
        let regs = vec![
            crate::auth::RegisteredModel {
                provider: "grok".into(),
                id: "grok-3".into(),
                small: false,
            },
            crate::auth::RegisteredModel {
                provider: "anthropic".into(),
                id: "claude-haiku-4-5".into(),
                small: true,
            },
        ];
        let hit = pick_strongest(&regs, &table).expect("ranked");
        assert!(!hit.id.contains("opus-5"));
        assert!(!hit.id.contains("gpt-5.6"));
        assert_eq!(hit.id, "claude-haiku-4-5");

        let only_grok = vec![crate::auth::RegisteredModel {
            provider: "grok".into(),
            id: "grok-3".into(),
            small: false,
        }];
        let hit = pick_strongest(&only_grok, &table).expect("grok");
        assert_eq!(hit.provider, "grok");
        assert_eq!(hit.id, "grok-3");

        let flagships = vec![
            crate::auth::RegisteredModel {
                provider: "anthropic".into(),
                id: "claude-opus-5".into(),
                small: false,
            },
            crate::auth::RegisteredModel {
                provider: "openai".into(),
                id: "gpt-5.6".into(),
                small: false,
            },
        ];
        let cheap = pick_cheap(&flagships, &table, "anthropic").expect("ok flagship");
        assert!(cheap.id.contains("opus") || cheap.id.contains("gpt-5.6"));
    }

    #[test]
    fn goal_continues_only_while_open_todos_make_progress() {
        assert!(goal_should_continue(1, 3, 0, 0, 8));
        assert!(goal_should_continue(2, 3, 1, 1, 8));
        assert!(!goal_should_continue(3, 3, 1, 2, 8));
        assert!(!goal_should_continue(1, 1, 0, 0, 8));
        // 한도는 엔진 사양(EngineSpec.max_continuations)이 정한다.
        assert!(!goal_should_continue(1, 3, 0, 3, 3));
        assert!(goal_should_continue(1, 3, 0, 3, 4));
    }

    #[test]
    fn contract_plan_prompt_keeps_main_system_context() {
        // 회귀 방지: 계획 호출이 system 을 통째로 교체하면 lessons·system_extra·
        // 프로젝트 규칙이 계획에서 사라진다 (v1 결함).
        let dir = std::env::temp_dir().join(format!("rafikx-plan-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let cfg = Config::load(Some(&dir.join("config.toml"))).expect("config");
        let extra = "신중한 시니어 개발자다. 최소 diff 로 고친다.";
        let lessons = "[프로젝트 교훈]\n- 회귀 테스트 없이 고치지 말 것";
        let system = system_prompt(&cfg, extra, lessons);

        let contract = plan_system_prompt(&system, crate::engine::PlanDepth::Contract, "");
        assert!(contract.contains(extra), "system_extra 유실");
        assert!(contract.contains("[프로젝트 교훈]"), "lessons 유실");
        assert!(contract.contains("[계획 모드]"));
        // Contract 는 3부 산출물을 강제한다.
        assert!(contract.contains("[해석]"));
        assert!(contract.contains("[완료 기준]"));
        assert!(contract.contains("[작업 분해]"));

        let brief = plan_system_prompt(&system, crate::engine::PlanDepth::Brief, "");
        assert!(brief.contains(extra), "system_extra 유실");
        assert!(brief.contains("[프로젝트 교훈]"), "lessons 유실");
        assert!(brief.contains("3~7개 항목"));
        assert!(!brief.contains("[완료 기준]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn self_harness_plan_instruction_is_plan_only() {
        let dir = std::env::temp_dir().join(format!("rafikx-planface-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let cfg = Config::load(Some(&dir.join("config.toml"))).expect("config");
        let system = system_prompt(&cfg, "", "");

        // 메타 레이어가 꺼져 있으면(빈 문자열) 계획 프롬프트에 아무것도 붙지 않는다.
        let off = plan_system_prompt(&system, crate::engine::PlanDepth::Brief, "");
        assert!(!off.contains("[Self-Harness 계획 지침]"));

        let on = plan_system_prompt(
            &system,
            crate::engine::PlanDepth::Contract,
            "  완료 기준을 검증 가능한 형태로 쓴다.  ",
        );
        assert!(on.contains("[Self-Harness 계획 지침] 완료 기준을 검증 가능한 형태로 쓴다."));
        // 계획 전용 면이므로 메인 시스템 프롬프트에는 없어야 한다.
        assert!(!system.contains("[Self-Harness 계획 지침]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_line_shows_only_non_default_axes() {
        let dir = std::env::temp_dir().join(format!("rafikx-suffix-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut cfg = Config::load(Some(&dir.join("config.toml"))).expect("config");

        // 기본값(rafikx · harness)이면 표시를 늘리지 않는다.
        assert!(engine_suffix(&cfg).is_empty());

        cfg.file.general.engine = "claude".into();
        assert_eq!(engine_suffix(&cfg), "  ·  engine=claude");

        cfg.file.general.discipline = "graph".into();
        assert_eq!(engine_suffix(&cfg), "  ·  engine=claude  ·  graph");

        // legacy engine="self" 는 rafikx 로 정규화되므로 엔진 표시가 빠진다.
        cfg.file.general.engine = "self".into();
        cfg.file.general.discipline = "loop".into();
        assert_eq!(engine_suffix(&cfg), "  ·  loop");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bind_falls_back_to_builtin_expert_profiles() {
        let dir = std::env::temp_dir().join(format!("rafikx-profile-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut cfg = Config::load(Some(&dir.join("config.toml"))).expect("config");

        // 기본 config 에는 전문가 프로파일이 없다 — 내장 프리셋으로 폴백해야 한다.
        assert!(!cfg.file.subagents.contains_key("planner"));
        let planner = resolve_profile(&cfg, "planner").expect("planner 프리셋");
        assert!(planner.tools.iter().any(|t| t == "todo_write"));
        assert!(!planner.tools.iter().any(|t| t == "write_file"));
        assert!(!planner.plan_first && !planner.verify);
        assert!(planner.system_extra.contains("[완료 기준]"));

        let reviewer = resolve_profile(&cfg, "reviewer").expect("reviewer 프리셋");
        assert_eq!(reviewer.max_iterations, REVIEW_GATE_MAX_ITER);
        assert!(reviewer.tools.iter().any(|t| t == "bash"));
        assert!(!reviewer.tools.iter().any(|t| t == "write_file"));
        assert!(reviewer.system_extra.contains("[판정]"));
        assert!(reviewer.system_extra.contains("[미충족 항목]"));

        for role in ["frontend", "backend"] {
            let sub = resolve_profile(&cfg, role).expect(role);
            assert_eq!(sub.tools, vec!["*".to_string()]);
            assert!(sub.plan_first && sub.verify);
            assert!(sub.system_extra.contains("[변경 요약]"));
        }

        // config 정의가 있으면 사용자 정의가 이긴다.
        let mut custom = crate::config::builtin_profile("planner").expect("preset");
        custom.system_extra = "사내 기획 규칙만 따른다".into();
        custom.max_iterations = 3;
        cfg.file.subagents.insert("planner".into(), custom);
        let planner = resolve_profile(&cfg, "planner").expect("사용자 정의 planner");
        assert_eq!(planner.system_extra, "사내 기획 규칙만 따른다");
        assert_eq!(planner.max_iterations, 3);

        // 등록되지 않은 이름은 폴백 대상이 아니다.
        assert!(resolve_profile(&cfg, "없는프로파일").is_none());
        assert!(resolve_profile(&cfg, "  ").is_none());
        assert!(profile_exists(&cfg, "coder")); // config 정의
        assert!(profile_exists(&cfg, "frontend")); // 내장 프리셋
        assert!(!profile_exists(&cfg, "없는역할"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_verdict_parsing_and_gate_transitions() {
        // 통과 판정.
        assert_eq!(
            parse_review_verdict("[판정] pass\n[미충족 항목] 없음\n[결함] 없음"),
            ReviewVerdict::Pass
        );
        assert_eq!(parse_review_verdict("[판정] 통과"), ReviewVerdict::Pass);
        // 판정 줄이 없으면 통과로 본다 — 게이트가 가용성을 해치면 안 된다.
        assert_eq!(
            parse_review_verdict("리뷰 도중 파일을 열지 못했습니다."),
            ReviewVerdict::Pass
        );
        assert_eq!(parse_review_verdict(""), ReviewVerdict::Pass);

        // 미통과 — 미충족 항목과 결함이 사유로 모인다.
        let text = "[판정] fail\n\
                    [미충족 항목]\n\
                    2. 캐시 히트율 로그 — 로그 출력이 없음\n\
                    [결함]\n\
                    src/cache.rs:42 — unwrap 사용 — 빈 키에서 패닉";
        let ReviewVerdict::Fail { summary } = parse_review_verdict(text) else {
            panic!("fail 판정이어야 한다");
        };
        assert!(summary.contains("캐시 히트율"));
        assert!(summary.contains("src/cache.rs:42"));
        assert!(!summary.contains("[판정]"));
        // '미통과'는 '통과'를 부분 문자열로 포함한다 — 실패 표지를 먼저 봐야 한다.
        assert!(matches!(
            parse_review_verdict("[판정] 미통과\n[결함] 테스트 없음"),
            ReviewVerdict::Fail { .. }
        ));
        // 구조를 지키지 않은 fail 출력은 본문 전체를 사유로 쓴다.
        let ReviewVerdict::Fail { summary } = parse_review_verdict("[판정] fail\n그냥 부족합니다")
        else {
            panic!("fail 판정이어야 한다");
        };
        assert!(summary.contains("그냥 부족합니다"));

        // 상태 전이: 1회차 미통과 → 재개, 재개 후에도 미통과 → 보고 후 종료.
        let fail = || ReviewVerdict::Fail {
            summary: "[결함] 테스트 없음".into(),
        };
        assert_eq!(gate_action(ReviewVerdict::Pass, 0), GateAction::Accept);
        assert_eq!(gate_action(ReviewVerdict::Pass, 1), GateAction::Accept);
        assert_eq!(
            gate_action(fail(), 0),
            GateAction::Resume("[결함] 테스트 없음".into())
        );
        assert_eq!(
            gate_action(fail(), 1),
            GateAction::Report("[결함] 테스트 없음".into())
        );
        assert_eq!(
            verdict_headline("[결함] 테스트 없음\n둘째 줄"),
            "[결함] 테스트 없음"
        );
        assert_eq!(verdict_headline("   "), "사유 미상");
    }

    #[test]
    fn review_prompt_sends_dod_and_files_but_not_diffs() {
        let changed = vec!["src/cache.rs".to_string(), "src/main.rs".to_string()];
        let p = review_prompt("캐시를 추가하라", "1. cargo test 통과", &changed);
        assert!(p.contains("캐시를 추가하라"));
        assert!(p.contains("[완료 기준]\n1. cargo test 통과"));
        assert!(p.contains("- src/cache.rs"));
        assert!(p.contains("- src/main.rs"));
        // 신선한 시각: diff 를 첨부하지 않고 리뷰어가 직접 읽게 한다.
        assert!(p.contains("read_file"));

        // DoD 가 없으면 그 절은 통째로 빠진다.
        let p = review_prompt("캐시를 추가하라", "   ", &[]);
        assert!(!p.contains("[완료 기준]"));
        assert!(p.contains("변경 파일 없음"));
    }

    #[test]
    fn plan_sections_are_extracted_for_the_verifier_gate() {
        let plan = "[해석] 캐시 계층을 추가한다.\n\
                    가정: 기존 저장소는 그대로 둔다.\n\
                    [완료 기준]\n\
                    1. cargo test 통과 — `cargo test` 실행\n\
                    2. 캐시 히트율 로그 — 실행 후 로그 확인\n\
                    [작업 분해]\n\
                    1. 인터페이스 정의\n\
                    2. 구현";
        let dod = extract_plan_section(plan, "[완료 기준]");
        assert!(dod.starts_with("1. cargo test 통과"));
        assert!(dod.contains("캐시 히트율"));
        assert!(!dod.contains("작업 분해"));
        assert!(!dod.contains("캐시 계층을 추가한다"));

        let interp = extract_plan_section(plan, "[해석]");
        assert!(interp.starts_with("캐시 계층을 추가한다."));
        assert!(interp.contains("가정: 기존 저장소는"));

        assert!(extract_plan_section(plan, "[없는 절]").is_empty());
        assert!(extract_plan_section("자유 형식 계획 3줄", "[완료 기준]").is_empty());
    }

    fn dag(spec: &[(&str, &[&str])]) -> Vec<DagNode> {
        spec.iter()
            .map(|(id, deps)| DagNode {
                id: (*id).into(),
                goal: format!("{id} 목표"),
                deps: deps.iter().map(|d| (*d).to_string()).collect(),
                produces: String::new(),
            })
            .collect()
    }

    #[test]
    fn parse_dag_reads_fenced_and_bare_json() {
        let fenced = "[완료 기준]\n1. cargo test 통과 — `cargo test`\n\n\
             ```json\n\
             {\"nodes\":[{\"id\":\"n1\",\"goal\":\"스키마 정의\",\"deps\":[],\"produces\":\"타입 3종\"},\
             {\"id\":\"n2\",\"goal\":\"구현\",\"deps\":[\"n1\"],\"produces\":\"모듈\"}]}\n\
             ```";
        let nodes = parse_dag(fenced).expect("펜스 JSON");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].id, "n1");
        assert_eq!(nodes[0].goal, "스키마 정의");
        assert_eq!(nodes[1].deps, vec!["n1".to_string()]);
        // 완료 기준은 JSON 앞 산문에서만 뽑는다 — JSON 을 빨아들이지 않는다.
        let dod = extract_plan_section(plan_prose(fenced), "[완료 기준]");
        assert_eq!(dod, "1. cargo test 통과 — `cargo test`");
        assert!(!dod.contains("nodes"));

        // 펜스가 없어도 첫 `{` 부터 균형 매칭으로 찾는다. 뒤에 산문이 붙어도 된다.
        let bare = "계획입니다.\n{\"nodes\":[{\"id\":\"a\",\"goal\":\"조사 {중괄호} 포함\",\
                    \"deps\":[]}]}\n이상.";
        let nodes = parse_dag(bare).expect("맨 JSON");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].goal, "조사 {중괄호} 포함");
        assert!(nodes[0].deps.is_empty());
        assert!(nodes[0].produces.is_empty());
    }

    #[test]
    fn parse_dag_rejects_malformed_plans() {
        // JSON 자체가 없다 → harness 폴백.
        assert!(parse_dag("1. 스키마\n2. 구현\n3. 테스트").is_none());
        // 깨진 JSON.
        assert!(parse_dag("{\"nodes\":[{\"id\":\"n1\",").is_none());
        // nodes 키가 없다.
        assert!(parse_dag("{\"steps\":[{\"id\":\"n1\",\"goal\":\"x\"}]}").is_none());
        // 빈 목록.
        assert!(parse_dag("{\"nodes\":[]}").is_none());
        // id·goal 이 비었거나 id 가 중복이면 실행 순서를 정의할 수 없다.
        assert!(parse_dag("{\"nodes\":[{\"id\":\" \",\"goal\":\"x\"}]}").is_none());
        assert!(parse_dag("{\"nodes\":[{\"id\":\"n1\",\"goal\":\"  \"}]}").is_none());
        assert!(
            parse_dag(
                "{\"nodes\":[{\"id\":\"n1\",\"goal\":\"a\"},{\"id\":\"n1\",\"goal\":\"b\"}]}"
            )
            .is_none()
        );
    }

    #[test]
    fn topo_order_sorts_dependencies_and_detects_cycles() {
        // n3 ← n2 ← n1 역순으로 적혀 있어도 위상순으로 되돌린다.
        let nodes = dag(&[("n3", &["n2"]), ("n1", &[]), ("n2", &["n1"])]);
        let order = topo_order(&nodes).expect("정렬");
        let ids: Vec<&str> = order.iter().map(|i| nodes[*i].id.as_str()).collect();
        assert_eq!(ids, vec!["n1", "n2", "n3"]);

        // 병렬 가지도 전부 한 번씩 나온다 (순차 실행이므로 순서만 유효하면 된다).
        let nodes = dag(&[("a", &[]), ("b", &["a"]), ("c", &["a"]), ("d", &["b", "c"])]);
        let order = topo_order(&nodes).expect("정렬");
        assert_eq!(order.len(), 4);
        let pos = |id: &str| {
            order
                .iter()
                .position(|i| nodes[*i].id == id)
                .expect("노드 위치")
        };
        assert!(pos("a") < pos("b") && pos("a") < pos("c"));
        assert!(pos("b") < pos("d") && pos("c") < pos("d"));

        // 같은 deps 가 두 번 적혀도 진입차수를 중복으로 세지 않는다.
        let nodes = dag(&[("a", &[]), ("b", &["a", "a"])]);
        assert_eq!(topo_order(&nodes).expect("중복 deps").len(), 2);

        // 모르는 id 를 가리키는 deps 는 간선이 없다 (계획의 오타로 실행이 막히지 않게).
        let nodes = dag(&[("a", &["없음"])]);
        assert_eq!(topo_order(&nodes).expect("미상 deps"), vec![0]);

        // 순환·자기참조는 폴백 사유로 보고한다.
        let cycle = topo_order(&dag(&[("n1", &["n2"]), ("n2", &["n1"])])).expect_err("순환");
        assert_eq!(cycle.remaining, vec!["n1".to_string(), "n2".to_string()]);
        assert!(cycle.to_string().contains("순환"));
        assert!(topo_order(&dag(&[("n1", &["n1"])])).is_err());
    }

    #[test]
    fn graph_node_prompts_carry_only_dependency_conclusions() {
        let nodes = dag(&[("n1", &[]), ("n2", &[]), ("n3", &["n1"])]);
        let produced = vec![
            ("n1".to_string(), "타입 3종을 정의했다".to_string()),
            ("n2".to_string(), "무관한 노드 산출물".to_string()),
        ];
        let system = graph_node_system("메인 시스템", 3, 3, &nodes[2], &produced);
        assert!(system.starts_with("메인 시스템"));
        assert!(system.contains("[그래프 노드 3/3] 목표: n3 목표"));
        assert!(system.contains("[선행 산출물 n1]\n타입 3종을 정의했다"));
        // deps 에 없는 노드의 산출물은 넘기지 않는다 (컨텍스트 격리).
        assert!(!system.contains("무관한 노드 산출물"));

        let prompt = graph_node_prompt("전체 작업", &nodes[2]);
        assert!(prompt.starts_with("전체 작업"));
        assert!(prompt.contains("[이번 노드] n3 목표"));
        assert!(prompt.contains("이 노드의 목표만 수행하라"));
    }

    #[test]
    fn graph_node_success_rescues_completed_todo_limit() {
        assert!(graph_node_ok("ok", 0, 0));
        // 등록한 todo 를 모두 끝내고 반복 상한에 닿았으면 성공으로 구제한다.
        assert!(graph_node_ok("limit", 3, 3));
        assert!(!graph_node_ok("limit", 2, 3));
        assert!(!graph_node_ok("limit", 0, 0));
        assert!(!graph_node_ok("fail", 3, 3));
        assert!(!graph_node_ok("incomplete", 1, 2));
    }

    #[test]
    fn graph_node_summary_keeps_conclusion_and_changed_files() {
        let mut outcome = AgentOutcome {
            changed_files: vec!["src/a.rs".into()],
            ..Default::default()
        };
        outcome
            .messages
            .push(Message::user_text("무시할 사용자 글"));
        outcome.messages.push(Message {
            role: crate::provider::Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "앞 노드 답변".into(),
            }],
        });
        outcome.messages.push(Message {
            role: crate::provider::Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "마".repeat(700),
            }],
        });
        let s = graph_node_summary(&outcome);
        // 마지막 답변만, 500자까지.
        assert!(!s.contains("앞 노드 답변"));
        assert!(!s.contains("무시할 사용자 글"));
        assert_eq!(s.matches('마').count(), GRAPH_SUMMARY_CAP);
        assert!(s.contains("변경 파일: src/a.rs"));

        let empty = graph_node_summary(&AgentOutcome::default());
        assert_eq!(empty, "(보고된 산출물 없음)");
    }

    #[test]
    fn loop_discipline_prompts_are_wired_to_the_rule_and_the_switch() {
        // 루프 규율은 종료 조건을, 정체 지시는 접근 전환을 못 박는다.
        assert!(LOOP_DISCIPLINE_RULE.contains("완료를 선언하지 마라"));
        assert!(LOOP_STALE_SWITCH.contains("진전이 없었다"));
        assert!(LOOP_STALE_SWITCH.contains("같은 접근의 반복을 금지한다"));
        // 그래프 계획은 [완료 기준] 절을 JSON 앞에 요구한다 (게이트 입력 보존).
        assert!(PLAN_GRAPH_INSTRUCTION.contains("[완료 기준]"));
        assert!(PLAN_GRAPH_INSTRUCTION.contains("\"nodes\""));
        let sys = plan_system_prompt_with("메인", PLAN_GRAPH_INSTRUCTION, "계획 면");
        assert!(sys.starts_with("메인"));
        assert!(sys.contains(PLAN_MODE_HEADER));
        assert!(sys.contains("[Self-Harness 계획 지침] 계획 면"));
    }

    #[test]
    fn pin_beats_automatic_choice_but_yields_to_explicit_override() {
        // 고정이 없으면 아무것도 하지 않는다.
        assert_eq!(decide_pin(None, "anthropic", None, None), PinDecision::Keep);
        assert_eq!(
            decide_pin(Some("  "), "anthropic", None, None),
            PinDecision::Keep
        );

        // 자동 선택(ranks·manual_*·프로파일 기본)이 고른 프로바이더는 고정에 진다.
        assert_eq!(
            decide_pin(Some("minimax"), "anthropic", None, None),
            PinDecision::Apply("minimax".into())
        );
        // sticky 재사용도 "직접 지정"이 아니다 — bind 에는 값이 들어오지만 명시 인자는 비어 있다.
        assert_eq!(
            decide_pin(Some("minimax"), "openai", None, None),
            PinDecision::Apply("minimax".into())
        );

        // 명시 --provider 는 고정을 이긴다 (경고 한 줄).
        assert_eq!(
            decide_pin(Some("minimax"), "anthropic", Some("anthropic"), None),
            PinDecision::Yield {
                pin: "minimax".into(),
                explicit: "provider=anthropic".into()
            }
        );
        // 모델만 직접 고른 경우도 사용자 의지 — 그 모델이 없는 프로바이더로 끌고 가지 않는다.
        assert_eq!(
            decide_pin(Some("minimax"), "openai", None, Some("gpt-5")),
            PinDecision::Yield {
                pin: "minimax".into(),
                explicit: "model=gpt-5".into()
            }
        );

        // 이미 고정 프로바이더면 명시가 있든 없든 조용히 통과한다.
        assert_eq!(
            decide_pin(Some("minimax"), "minimax", None, None),
            PinDecision::Keep
        );
        assert_eq!(
            decide_pin(
                Some("minimax"),
                "MiniMax",
                Some("minimax"),
                Some("minimax-m3")
            ),
            PinDecision::Keep
        );
    }

    #[test]
    fn pinned_fallback_order_keeps_one_provider() {
        let order = || {
            vec![
                "anthropic".to_string(),
                "minimax".to_string(),
                "glm".to_string(),
            ]
        };
        // 고정이면 그 프로바이더 하나만 남는다 (계정 순회는 안쪽이 담당).
        assert_eq!(
            limit_order_to_pin(Some("minimax"), None, order()),
            vec!["minimax".to_string()]
        );
        // 고정이 없으면 원래 순서 그대로.
        assert_eq!(limit_order_to_pin(None, None, order()), order());
        assert_eq!(limit_order_to_pin(Some(""), None, order()), order());
        // 명시 --provider 가 있으면 고정을 양보한다.
        assert_eq!(
            limit_order_to_pin(Some("minimax"), Some("anthropic"), order()),
            order()
        );
        // 고정 프로바이더가 순서에 없으면(미연결) 가용성을 우선해 원래 순서를 지킨다.
        assert_eq!(limit_order_to_pin(Some("moonshot"), None, order()), order());
    }

    #[test]
    fn harness_strategy_accepts_single_and_multi_only() {
        assert_eq!(
            HarnessStrategy::parse("single"),
            Some(HarnessStrategy::Single)
        );
        assert_eq!(
            HarnessStrategy::parse("multi"),
            Some(HarnessStrategy::Multi)
        );
        assert_eq!(HarnessStrategy::parse("manual"), None);
    }
}
