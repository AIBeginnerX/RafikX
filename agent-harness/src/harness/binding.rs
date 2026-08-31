use super::*;

#[derive(Clone)]
pub struct Binding {
    /// 콤보 체인 (provider, model) — 비어 있으면 일반 바인딩 (F8).
    /// 있으면 주 연결 실패 시 체인 다음 (provider, model) 쌍으로 요청 단위 전환한다.
    pub combo_chain: Vec<(String, String)>,
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

pub fn bind(
    cfg: &Config,
    class: TaskClass,
    provider_override: Option<&str>,
    model_override: Option<&str>,
) -> Result<Binding> {
    bind_profile(cfg, class, None, provider_override, model_override)
}

/// 요청 모델과 응답 모델의 일치 판정 — 공급자가 변형 id(날짜 접미 등)로 보고하는
/// 경우는 허용하고, 완전히 다른 모델이 답했을 때만 경고한다.
fn model_matches(requested: &str, answered: &str) -> bool {
    requested.is_empty()
        || answered.is_empty()
        || answered.starts_with(requested)
        || requested.starts_with(answered)
}

/// 응답 모델 검증 — 공급자가 요청과 다른 모델로 답하면 사용자에게 보인다.
/// "사용 안 되는 모델을 골랐는데 답이 왔다" 류의 조용한 대체를 드러낸다.
fn warn_model_mismatch(requested: &str, resp: &ChatResponse) {
    if !model_matches(requested, &resp.model) {
        crate::ui::live_warn(&format!(
            "요청한 모델 '{requested}' 대신 다른 모델 '{}' 이(가) 응답했습니다.",
            resp.model
        ));
    }
}

/// 콤보 체인 최대 홉 수 — 요청 하나가 체인을 따라갈 수 있는 횟수 상한 (F8).
pub const COMBO_MAX_HOPS: usize = 3;

#[derive(Debug)]
pub(crate) struct ProviderAttemptLimitExceeded {
    pub(crate) limit: u32,
}

impl std::fmt::Display for ProviderAttemptLimitExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "프로바이더 HTTP 시도 예산 {}회 소진", self.limit)
    }
}

impl std::error::Error for ProviderAttemptLimitExceeded {}

pub(crate) fn is_provider_attempt_limit_exceeded(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ProviderAttemptLimitExceeded>()
        .is_some()
}

struct ProviderAttemptGate<'a> {
    run: Option<&'a crate::run::RunContext>,
    local_limit: u32,
    local_used: u32,
}

impl<'a> ProviderAttemptGate<'a> {
    const fn local() -> Self {
        Self {
            run: None,
            local_limit: COMBO_MAX_HOPS as u32,
            local_used: 0,
        }
    }

    fn in_run(run: &'a crate::run::RunContext) -> Self {
        Self {
            run: Some(run),
            local_limit: COMBO_MAX_HOPS as u32,
            local_used: 0,
        }
    }

    fn claim(&mut self) -> Result<()> {
        if self.local_used >= self.local_limit {
            return Err(ProviderAttemptLimitExceeded {
                limit: self.local_limit,
            }
            .into());
        }
        if let Some(run) = self.run
            && !run.claim_provider_attempt()
        {
            return Err(ProviderAttemptLimitExceeded {
                limit: run.provider_attempt_limit(),
            }
            .into());
        }
        self.local_used += 1;
        Ok(())
    }

    fn can_dispatch(&self, reserved: u32) -> Result<bool> {
        if self.local_used < self.local_limit.saturating_sub(reserved) {
            return Ok(true);
        }
        if reserved > 0 {
            return Ok(false);
        }
        Err(ProviderAttemptLimitExceeded {
            limit: self.local_limit,
        }
        .into())
    }
}

/// 콤보 바인딩 — [combos.<이름>] chain 을 해석해 첫 쌍은 주 연결로, 전체 쌍은
/// combo_chain 으로 Binding 에 담는다. config 에 없는 이름·빈 체인은 오류.
/// 콤보의 체인 스펙 — COMBO_MAX_HOPS 로 자른다 (순수 조회, F8).
pub(crate) fn combo_chain_specs(cfg: &Config, combo_name: &str) -> Result<Vec<String>> {
    let specs = cfg
        .file
        .combos
        .get(combo_name)
        .ok_or_else(|| anyhow!("콤보 '{combo_name}' 이(가) config [combos] 에 없습니다"))?;
    if specs.is_empty() {
        anyhow::bail!("콤보 '{combo_name}' 의 chain 이 비어 있습니다");
    }
    Ok(specs.iter().take(COMBO_MAX_HOPS).cloned().collect())
}

fn bind_combo(
    cfg: &Config,
    class: TaskClass,
    profile_override: Option<&str>,
    combo_name: &str,
) -> Result<Binding> {
    let specs = combo_chain_specs(cfg, combo_name)?;
    let profile_name = profile_override
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| profile_name_for(cfg, class).to_string());
    let sub = resolve_profile(cfg, &profile_name)
        .ok_or_else(|| anyhow!("서브에이전트 '{profile_name}' 이(가) config에 없습니다"))?;
    let needs_tools = !sub.tools.is_empty();
    let mut chain: Vec<(String, String)> = Vec::new();
    for spec in &specs {
        let (p, m) = resolve_spec(cfg, spec, needs_tools)
            .map_err(|e| anyhow!("콤보 '{combo_name}' 의 '{spec}' 해석 실패: {e}"))?;
        chain.push((p, m));
    }
    let (p0, m0) = chain[0].clone();
    let mut binding = bind_profile(cfg, class, Some(&profile_name), Some(&p0), Some(&m0))?;
    binding.combo_chain = chain;
    Ok(binding)
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
    // 콤보 바인딩 (F8) — model_override 가 "combo:<이름>" 이면 체인 첫 쌍으로 바인딩하고
    // 나머지 쌍을 combo_chain 에 담는다. 체인은 요청당 최대 COMBO_MAX_HOPS 까지만 따라간다.
    if let Some(combo_name) = model_override.and_then(|m| m.strip_prefix("combo:")) {
        return bind_combo(cfg, class, profile_override, combo_name);
    }
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

    // 프로파일별 모델(`[subagents.<name>] model`) — 팀 모드의 역할 분업 자리다.
    // 사용자가 모델을 직접 지정했으면(model_override) 그 의지가 이긴다.
    // 프로바이더 오버라이드(엔진 고정 재바인딩·CLI 지정)는 provider 부분만 이기고
    // 모델 ID 는 그대로 존중한다 — 그래서 pin 재바인딩이 profile.model 을 지우지 않는다.
    let profile_pick = if model_override.is_some() {
        None
    } else {
        decide_profile_model(
            sub.model.as_deref(),
            &sub.provider,
            provider_override,
            |m| provider_for_model(cfg, m),
        )
        .filter(|(p, _)| profile_model_usable(cfg, p, needs_tools))
    };

    let (provider_name, model, verify_model) = if let Some((p, m)) = profile_pick {
        crate::applog::debug(&format!("bind: profile model {profile_name} → {p}/{m}"));
        (p, m, None)
    } else if let (Some(p), Some(m)) = (provider_override, model_override) {
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
    let lane_tools = lane_filtered_tools(&profile_name, &sub.tools);
    let tools = if profile_name.eq_ignore_ascii_case("reviewer") {
        lane_tools
    } else {
        with_memory_tools(&lane_tools)
    };
    Ok(Binding {
        combo_chain: Vec::new(),
        class,
        profile_name,
        provider_name,
        model,
        kind: p.kind.clone(),
        tools,
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

/// 레인(explorer/researcher)이면 허용목록 외 도구(mutation 등)를 걸러낸다.
/// config 재정의로 쓰기 도구를 넣어도 레인의 읽기 전용 성질은 유지된다.
fn lane_filtered_tools(profile_name: &str, tools: &[String]) -> Vec<String> {
    let mut tools = tools.to_vec();
    if let Some(allow) = crate::harness::lane_tool_allowlist(profile_name) {
        tools.retain(|name| allow.contains(&name.as_str()));
    }
    tools
}

/// 메모리 도구 — 어떤 도구든 쓰는 프로파일이면 항상 포함한다 (F1·T4 실측 결함 수정).
/// 기억은 작업 종류와 무관한 상시 능력이어야 한다. 도구 0개 프로파일(quick·plan)은
/// 그 성질(텍스트 전용)을 해치지 않기 위해 제외한다.
const MEMORY_TOOLS: &[&str] = &["remember", "recall", "forget"];

fn with_memory_tools(tools: &[String]) -> Vec<String> {
    if tools.is_empty() {
        return Vec::new();
    }
    let mut tools = tools.to_vec();
    if tools.iter().any(|t| t == "*") {
        return tools; // 와일드카드는 이미 전부 포함
    }
    for m in MEMORY_TOOLS {
        if !tools.iter().any(|t| t == m) {
            tools.push((*m).to_string());
        }
    }
    tools
}

/// 프로파일별 모델 해석 (순수 함수 — 배선 없이 우선순위만 정한다).
///
/// - `profile_model` 이 비면 None → 기존 선택 규칙(manual·single·auto)을 그대로 쓴다.
/// - `"provider:model"` 이면 그 프로바이더, 모델 ID 단독이면 `lookup`(등록 모델) →
///   프로파일 provider 순으로 프로바이더를 정한다.
/// - `provider_override`(엔진 고정 재바인딩·CLI `--provider`)가 있으면 프로바이더는 그것이
///   이기고 **모델 ID 만** 존중한다. 그 모델이 고정 프로바이더에 실재하는지는 카탈로그
///   API 가 없어 확인할 수 없으므로 그대로 시도한다.
pub fn decide_profile_model(
    profile_model: Option<&str>,
    profile_provider: &str,
    provider_override: Option<&str>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<(String, String)> {
    let spec = profile_model.map(str::trim).filter(|s| !s.is_empty())?;
    let (spec_provider, model) = parse_manual_spec(spec);
    let model = model.trim().to_string();
    if model.is_empty() {
        return None;
    }
    let provider = provider_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(spec_provider)
        .or_else(|| lookup(&model))
        .unwrap_or_else(|| profile_provider.to_string());
    if provider.trim().is_empty() {
        return None;
    }
    Some((provider, model))
}

/// 프로파일별 모델이 가리키는 프로바이더를 실제로 쓸 수 있는지.
/// 쓸 수 없으면 지정을 버리고 기존 선택 규칙으로 되돌린다 (설정 오타가 실행을 막지 않게).
fn profile_model_usable(cfg: &Config, provider: &str, needs_tools: bool) -> bool {
    let Ok(p) = cfg.provider(provider) else {
        return false;
    };
    if !crate::auth::is_usable(cfg, provider) {
        return false;
    }
    !needs_tools || p.supports_tools
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
pub(crate) fn pin_unavailable(cfg: &Config, pin: &str, needs_tools: bool) -> Option<&'static str> {
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
    if let Some((p, m)) = t.split_once(':')
        && !p.is_empty()
        && !m.is_empty()
        && !p.contains('/')
    {
        return (Some(p.to_string()), m.to_string());
    }
    (None, t.to_string())
}

pub(crate) fn resolve_spec(
    cfg: &Config,
    spec: &str,
    needs_tools: bool,
) -> Result<(String, String)> {
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

/// Harness 선정 모드 저장 ("auto" | "manual").
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
/// 기본 연결·기본 모델 설정이 자동 Harness보다 우선한다.
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

    if prefer_cheap && let Some(hit) = pick_cheap(&regs, &table, &sub.provider) {
        return Ok((hit.provider, hit.id, None));
    }

    if prefer_strong && let Some(hit) = pick_strongest(&regs, &table) {
        return Ok((hit.provider, hit.id.clone(), Some(hit.id)));
    }

    // 순위 모르면 프로파일 기본 (그 프로바이더가 연결된 경우만)
    if crate::auth::is_usable(cfg, &sub.provider)
        && let Ok(p) = cfg.provider(&sub.provider)
        && (!needs_tools || p.supports_tools)
    {
        let model = model_for_role(p, &sub.model_role);
        return Ok((sub.provider.clone(), model, None));
    }
    let first = regs.remove(0);
    Ok((first.provider, first.id, None))
}

pub(crate) fn pick_cheap(
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

pub(crate) fn pick_strongest(
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
        if matches!(class, TaskClass::Simple | TaskClass::Medium)
            && let Some(h) = pick_cheap(&regs, &table, name)
        {
            return Some(h.id);
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
            if name == "openai"
                && let Some(c) = &cred
                && c.oauth
            {
                return Ok(DynProvider::OpenAi(OpenAiCompatProvider::with_codex_oauth(
                    c.token.clone(),
                    c.account_id.clone(),
                )?));
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

async fn try_accounts<F, Fut>(
    cfg: &Config,
    name: &str,
    gate: &mut ProviderAttemptGate<'_>,
    reserve_fallback_attempt: bool,
    mut call: F,
) -> Result<ChatResponse>
where
    F: FnMut(DynProvider) -> Fut,
    Fut: std::future::Future<Output = Result<ChatResponse>>,
{
    let ids = account_ids_for(name);
    let mut last_err = None;
    for (i, id) in ids.iter().enumerate() {
        // 예산 소진은 새 dispatch 만 막는다 — 앞선 계정에서 이미 받은 오류가 있으면
        // 그 오류를 그대로 돌려준다(이 함수의 반환 관례: last_err 원본).
        match gate.can_dispatch(u32::from(reserve_fallback_attempt)) {
            Ok(true) => {}
            Ok(false) => break,
            Err(limit) => return Err(last_err.take().unwrap_or(limit)),
        }
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
        if let Err(limit) = gate.claim() {
            return Err(last_err.take().unwrap_or(limit));
        }
        match call(client).await.and_then(|resp| {
            crate::provider::validate_chat_response(&resp, "fallback")?;
            Ok(resp)
        }) {
            Ok(resp) => {
                crate::usage::record_success(id, &resp);
                crate::usage::apply_hint(id, &resp.limit);
                return Ok(resp);
            }
            Err(e) if is_typed_rate_limited(&e) => {
                let secs = rate_limit_retry_after(&e);
                crate::usage::mark_limited(id, secs);
                crate::ui::warn("리밋 → 다음 계정으로 전환");
                last_err = Some(e);
            }
            Err(e) if is_auth_failure(&e) => {
                fallback_warn(&e, "authentication rejected; trying next account");
                last_err = Some(e);
            }
            Err(e) => {
                let action = nonstream_failure_action(&e);
                last_err = Some(e);
                if action == NonstreamFailureAction::NextCandidate {
                    break;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("'{name}' 사용 가능한 계정이 없습니다")))
}

/// 401/403 등 키 문제 — 재시도보다 재연결이 답이다.
fn is_auth_failure(e: &anyhow::Error) -> bool {
    request_error_kind(e)
        .is_some_and(|kind| matches!(kind, crate::provider::ProviderRequestErrorKind::Auth { .. }))
}

fn request_error_kind(error: &anyhow::Error) -> Option<crate::provider::ProviderRequestErrorKind> {
    error
        .downcast_ref::<crate::provider::ProviderRequestError>()
        .map(crate::provider::ProviderRequestError::kind)
}

fn is_typed_rate_limited(error: &anyhow::Error) -> bool {
    request_error_kind(error).is_some_and(|kind| {
        matches!(
            kind,
            crate::provider::ProviderRequestErrorKind::RateLimited { .. }
        )
    })
}

fn request_is_retryable(error: &anyhow::Error) -> bool {
    match request_error_kind(error) {
        Some(
            crate::provider::ProviderRequestErrorKind::RateLimited { .. }
            | crate::provider::ProviderRequestErrorKind::Server { .. }
            | crate::provider::ProviderRequestErrorKind::Timeout
            | crate::provider::ProviderRequestErrorKind::Connect
            | crate::provider::ProviderRequestErrorKind::BodyRead
            | crate::provider::ProviderRequestErrorKind::Transport,
        ) => true,
        Some(
            crate::provider::ProviderRequestErrorKind::Auth { .. }
            | crate::provider::ProviderRequestErrorKind::Client { .. },
        )
        | None => false,
    }
}

fn rate_limit_retry_after(error: &anyhow::Error) -> u64 {
    if let Some(error) = error.downcast_ref::<crate::provider::ProviderRequestError>()
        && let crate::provider::ProviderRequestErrorKind::RateLimited { retry_after } = error.kind()
    {
        return retry_after;
    }
    45
}

pub fn fallback_order(cfg: &Config, primary: &str, cli_provider: Option<&str>) -> Vec<String> {
    let mut order = Vec::new();
    if let Some(p) = cli_provider
        && crate::auth::is_usable(cfg, p)
    {
        order.push(p.to_string());
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
/// 엔진 고정이 있으면 그 프로바이더를 **선두**로 올린다 — 계정 다중 순회는 안쪽
/// (chat_with_fallback)이 담당하므로 리밋 시 같은 프로바이더의 다른 계정으로 먼저 넘어가고,
/// 그 프로바이더가 전면 장애일 때만 나머지 폴백으로 내려간다(설계 §15.2: 고정은 선호이지
/// 가용성 희생이 아니다). `pin_strict = true` 면 예전처럼 고정 하나로 제한한다.
/// 백그라운드 보조 호출(교훈 반성·LLM 분류·self-harness 제안)은 고정 대상이 아니므로
/// 기존 `fallback_order` 를 그대로 쓴다 (설계 §11.2).
pub fn fallback_order_pinned(
    cfg: &Config,
    primary: &str,
    cli_provider: Option<&str>,
) -> Vec<String> {
    let order = fallback_order(cfg, primary, cli_provider);
    let (engine, _) = crate::engine::normalize(&cfg.file.general.engine);
    let spec = crate::engine::resolve_with(&cfg.file.engines, &engine);
    limit_order_to_pin(spec.pin(), spec.pin_strict, cli_provider, order)
}

/// 고정이 걸린 실행의 폴백 순서 계산 (순수 함수).
/// 사용자가 --provider 로 직접 지정했으면 고정을 양보하고, 고정 프로바이더가 순서에
/// 아예 없으면(연결 없음) 원래 순서를 지킨다 — 가용성 우선.
pub(crate) fn limit_order_to_pin(
    pin: Option<&str>,
    pin_strict: bool,
    cli_provider: Option<&str>,
    order: Vec<String>,
) -> Vec<String> {
    let Some(pin) = pin.map(str::trim).filter(|p| !p.is_empty()) else {
        return order;
    };
    if cli_provider.map(str::trim).is_some_and(|p| !p.is_empty()) {
        return order;
    }
    if !order.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
        return order;
    }
    if pin_strict {
        return vec![pin.to_string()];
    }
    // 고정을 선두로, 나머지는 후순위로 남긴다. 실제 폴백이 일어나면 stream/chat 의
    // 기존 폴백 경고가 "다른 프로바이더로 넘어갔다"를 그대로 알린다.
    let mut out = vec![pin.to_string()];
    out.extend(
        order
            .into_iter()
            .filter(|p| !p.eq_ignore_ascii_case(pin))
            .collect::<Vec<_>>(),
    );
    out
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

fn fallback_summary(error: &anyhow::Error, action: &'static str) -> String {
    format!("{}; {action}", crate::provider::safe_summary(error))
}

fn sanitized_provider_error(error: anyhow::Error) -> anyhow::Error {
    anyhow!(crate::provider::safe_summary(&error))
}

/// 시도 예산 소진은 **새 HTTP dispatch 만** 막는다 — 이미 관측한 공급자 오류가 있으면
/// 그 오류가 실패의 원인이므로 예산 소진 오류로 덮어쓰지 않는다. 덮으면 비치명 500이
/// "예산 소진"으로 둔갑해 실행 전체가 limit 으로 조기 종료된다.
fn attempt_limit_or_observed(
    limit_error: anyhow::Error,
    primary_err: &mut Option<anyhow::Error>,
    last_err: &mut Option<anyhow::Error>,
) -> anyhow::Error {
    primary_err
        .take()
        .or_else(|| last_err.take())
        .map_or(limit_error, sanitized_provider_error)
}

fn fallback_warn(error: &anyhow::Error, action: &'static str) {
    // 폴백 과정의 개별 실패(503·429·401 등)는 화면에 노출하지 않는다.
    // 폴백이 성공하면 사용자는 최종 답변만 보고, 전부 실패했을 때만 오류가 전달된다.
    crate::applog::debug(&format!("fallback: {}", fallback_summary(error, action)));
}

pub async fn chat_with_fallback(
    cfg: &Config,
    order: &[String],
    model_role: &str,
    req: ChatRequest,
) -> Result<(String, ChatResponse)> {
    let mut gate = ProviderAttemptGate::local();
    chat_with_fallback_inner(cfg, order, model_role, req, None, &mut gate).await
}

pub(crate) async fn chat_with_fallback_in_run(
    cfg: &Config,
    run: &crate::run::RunContext,
    order: &[String],
    model_role: &str,
    req: ChatRequest,
) -> Result<(String, ChatResponse)> {
    let mut gate = ProviderAttemptGate::in_run(run);
    chat_with_fallback_inner(cfg, order, model_role, req, None, &mut gate).await
}

/// 콤보 체인 비스트리밍 호출 (F8) — 계획·상담 등 단발 호출용.
pub async fn chat_with_fallback_combo(
    cfg: &Config,
    combo: &[(String, String)],
    model_role: &str,
    req: ChatRequest,
) -> Result<(String, ChatResponse)> {
    let mut gate = ProviderAttemptGate::local();
    chat_with_fallback_inner(cfg, &[], model_role, req, Some(combo), &mut gate).await
}

#[derive(Clone, Copy)]
struct FallbackCandidate<'a> {
    provider: &'a str,
    combo_model: Option<&'a str>,
}

fn fallback_candidates<'a>(
    order: &'a [String],
    combo: Option<&'a [(String, String)]>,
) -> Vec<FallbackCandidate<'a>> {
    match combo {
        Some(chain) => chain
            .iter()
            .map(|(provider, model)| FallbackCandidate {
                provider,
                combo_model: Some(model),
            })
            .collect(),
        None => order
            .iter()
            .map(|provider| FallbackCandidate {
                provider,
                combo_model: None,
            })
            .collect(),
    }
}

const fn reserve_for_secondary(candidate_index: usize, candidate_count: usize) -> bool {
    candidate_index == 0 && candidate_count > 1
}

async fn chat_with_fallback_inner(
    cfg: &Config,
    order: &[String],
    model_role: &str,
    mut req: ChatRequest,
    combo: Option<&[(String, String)]>,
    gate: &mut ProviderAttemptGate<'_>,
) -> Result<(String, ChatResponse)> {
    let original_model = req.model.clone();
    let candidates = fallback_candidates(order, combo);
    let candidate_count = candidates.len();
    let primary = candidates.first().map(|candidate| candidate.provider);
    // 첫 번째(주 연결) 오류를 끝까지 보존한다 — 마지막 폴백의 오류가 원인을 가리지 않게.
    let mut primary_err: Option<anyhow::Error> = None;
    let mut last_err = None;
    for (candidate_index, candidate) in candidates.into_iter().enumerate() {
        let Some(model) = candidate.combo_model.map(str::to_string).or_else(|| {
            model_for_fallback(
                cfg,
                candidate.provider,
                model_role,
                &original_model,
                primary,
            )
        }) else {
            continue;
        };
        if candidate.combo_model.is_some() && model != original_model {
            // 콤보 전환은 "다른 모델이 답한다"는 사실이라 사용자에게 보인다
            // (프로바이더 수준 폴백의 저소음 규칙과 구분, F8).
            crate::ui::live_warn(&format!(
                "폴 fallback: {}/{} 사용 중",
                candidate.provider, model
            ));
        }
        req.model = model;
        match try_accounts(
            cfg,
            candidate.provider,
            gate,
            reserve_for_secondary(candidate_index, candidate_count),
            |client| {
                let req = req.clone();
                async move { client.chat(&req).await }
            },
        )
        .await
        {
            Ok(resp) => {
                warn_model_mismatch(&req.model, &resp);
                return Ok((candidate.provider.to_string(), resp));
            }
            Err(e) => {
                if is_provider_attempt_limit_exceeded(&e) {
                    return Err(attempt_limit_or_observed(
                        e,
                        &mut primary_err,
                        &mut last_err,
                    ));
                }
                fallback_warn(&e, "trying next provider");
                if Some(candidate.provider) == primary && primary_err.is_none() {
                    primary_err = Some(e);
                } else {
                    last_err = Some(e);
                }
            }
        }
    }
    Err(primary_err
        .or(last_err)
        .map(sanitized_provider_error)
        .unwrap_or_else(|| anyhow!("사용 가능한 프로바이더가 없습니다")))
}

/// 도구 인자 생성 구간의 진행 표시 문구 — 스트림 소비자 세 곳이 함께 쓴다.
pub fn tool_args_label(name: &str, total_bytes: usize) -> String {
    format!("도구 호출 작성 중: {name} · {}KB", total_bytes / 1024)
}

#[derive(Debug, PartialEq, Eq)]
enum StreamFailureAction {
    AbortAfterEmission,
    RetrySameAccount,
    NextAccount,
    NextCandidate,
}

#[derive(Debug, PartialEq, Eq)]
enum NonstreamFailureAction {
    NextAccount,
    NextCandidate,
}

fn protocol_error_kind(error: &anyhow::Error) -> Option<crate::provider::ProtocolErrorKind> {
    error
        .downcast_ref::<crate::provider::ProtocolError>()
        .map(crate::provider::ProtocolError::kind)
}

fn protocol_is_retryable_before_output(error: &anyhow::Error) -> bool {
    match protocol_error_kind(error) {
        Some(
            crate::provider::ProtocolErrorKind::EmptyResponse
            | crate::provider::ProtocolErrorKind::InvalidTool
            | crate::provider::ProtocolErrorKind::InvalidJson
            | crate::provider::ProtocolErrorKind::InvalidSequence
            | crate::provider::ProtocolErrorKind::TruncatedStream
            | crate::provider::ProtocolErrorKind::UpstreamError,
        ) => true,
        Some(crate::provider::ProtocolErrorKind::LimitExceeded) | None => false,
    }
}

fn protocol_is_limit(error: &anyhow::Error) -> bool {
    protocol_error_kind(error) == Some(crate::provider::ProtocolErrorKind::LimitExceeded)
}

fn nonstream_failure_action(error: &anyhow::Error) -> NonstreamFailureAction {
    if is_typed_rate_limited(error)
        || is_auth_failure(error)
        || protocol_is_retryable_before_output(error)
        || request_is_retryable(error)
    {
        NonstreamFailureAction::NextAccount
    } else {
        NonstreamFailureAction::NextCandidate
    }
}

fn stream_failure_action(
    error: &anyhow::Error,
    emitted: usize,
    attempt: u32,
) -> StreamFailureAction {
    if emitted > 0 {
        return StreamFailureAction::AbortAfterEmission;
    }
    if protocol_is_limit(error) {
        return StreamFailureAction::NextCandidate;
    }
    if is_typed_rate_limited(error) || is_auth_failure(error) {
        return StreamFailureAction::NextAccount;
    }
    if protocol_is_retryable_before_output(error) || request_is_retryable(error) {
        return if attempt == 0 {
            StreamFailureAction::RetrySameAccount
        } else {
            StreamFailureAction::NextCandidate
        };
    }
    StreamFailureAction::NextCandidate
}

fn can_retry_same_account(
    gate: &ProviderAttemptGate<'_>,
    has_fallback_candidate: bool,
) -> Result<bool> {
    gate.can_dispatch(u32::from(has_fallback_candidate))
}

/// 주 연결의 오류를 primary_err 로 이관한다 — 후보를 떠나는 경로가 두 갈래
/// (계정 순회 종료·NextCandidate 즉시 전환)라 이관을 한 곳에 모은다.
/// 이관하지 않으면 마지막 폴백의 오류가 주 연결의 원인을 덮어쓴다.
fn promote_primary_error(
    candidate: &str,
    primary: Option<&str>,
    primary_err: &mut Option<anyhow::Error>,
    last_err: &mut Option<anyhow::Error>,
) {
    if last_err.is_some() && Some(candidate) == primary && primary_err.is_none() {
        *primary_err = last_err.take();
        if let Some(error) = primary_err.as_ref() {
            fallback_warn(error, "trying next provider");
        }
    }
}

pub async fn stream_with_fallback<F>(
    cfg: &Config,
    order: &[String],
    model_role: &str,
    req: ChatRequest,
    on_event: F,
) -> Result<(String, ChatResponse)>
where
    F: FnMut(StreamEvent),
{
    let mut on_event = on_event;
    let mut gate = ProviderAttemptGate::local();
    stream_with_fallback_inner(
        cfg,
        order,
        model_role,
        req,
        None,
        true,
        &mut gate,
        |event| on_event(event.public()),
    )
    .await
}

pub(crate) async fn stream_semantic_with_fallback_in_run<F>(
    cfg: &Config,
    run: &crate::run::RunContext,
    order: &[String],
    model_role: &str,
    req: ChatRequest,
    on_event: F,
) -> Result<(String, ChatResponse)>
where
    F: FnMut(SemanticStreamEvent),
{
    let mut gate = ProviderAttemptGate::in_run(run);
    stream_with_fallback_inner(
        cfg, order, model_role, req, None, false, &mut gate, on_event,
    )
    .await
}

/// 콤보 체인 스트리밍 (F8) — 체인의 (provider, model) 쌍을 순서대로 시도한다.
/// 첫 쌍이 아닌 후보로 넘어갈 때 배지를 표시한다 (조용한 전환 금지).
pub async fn stream_with_fallback_combo<F>(
    cfg: &Config,
    combo: &[(String, String)],
    model_role: &str,
    req: ChatRequest,
    on_event: F,
) -> Result<(String, ChatResponse)>
where
    F: FnMut(StreamEvent),
{
    let mut on_event = on_event;
    let mut gate = ProviderAttemptGate::local();
    stream_with_fallback_inner(
        cfg,
        &[],
        model_role,
        req,
        Some(combo),
        true,
        &mut gate,
        |event| on_event(event.public()),
    )
    .await
}

pub(crate) async fn stream_semantic_with_fallback_combo_in_run<F>(
    cfg: &Config,
    run: &crate::run::RunContext,
    combo: &[(String, String)],
    model_role: &str,
    req: ChatRequest,
    on_event: F,
) -> Result<(String, ChatResponse)>
where
    F: FnMut(SemanticStreamEvent),
{
    let mut gate = ProviderAttemptGate::in_run(run);
    stream_with_fallback_inner(
        cfg,
        &[],
        model_role,
        req,
        Some(combo),
        false,
        &mut gate,
        on_event,
    )
    .await
}

async fn stream_with_fallback_inner<F>(
    cfg: &Config,
    order: &[String],
    model_role: &str,
    mut req: ChatRequest,
    combo: Option<&[(String, String)]>,
    candidates_visible: bool,
    gate: &mut ProviderAttemptGate<'_>,
    mut on_event: F,
) -> Result<(String, ChatResponse)>
where
    F: FnMut(SemanticStreamEvent),
{
    use std::sync::atomic::{AtomicUsize, Ordering};
    let original_model = req.model.clone();
    let candidates = fallback_candidates(order, combo);
    let candidate_count = candidates.len();
    let primary = candidates.first().map(|candidate| candidate.provider);
    let mut primary_err: Option<anyhow::Error> = None;
    let mut last_err: Option<anyhow::Error> = None;
    let emitted = AtomicUsize::new(0);

    'candidates: for (candidate_index, candidate) in candidates.into_iter().enumerate() {
        let Some(model) = candidate.combo_model.map(str::to_string).or_else(|| {
            model_for_fallback(
                cfg,
                candidate.provider,
                model_role,
                &original_model,
                primary,
            )
        }) else {
            continue;
        };
        if candidate.combo_model.is_some() && model != original_model {
            // 콤보 전환은 "다른 모델이 답한다"는 사실이라 사용자에게 보인다
            // (프로바이더 수준 폴백의 저소음 규칙과 구분, F8).
            crate::ui::live_warn(&format!(
                "폴 fallback: {}/{} 사용 중",
                candidate.provider, model
            ));
        }
        req.model = model;
        let ids = account_ids_for(candidate.provider);
        for id in &ids {
            match gate.can_dispatch(u32::from(reserve_for_secondary(
                candidate_index,
                candidate_count,
            ))) {
                Ok(true) => {}
                Ok(false) => break,
                Err(limit) => {
                    return Err(attempt_limit_or_observed(
                        limit,
                        &mut primary_err,
                        &mut last_err,
                    ));
                }
            }
            let wait = crate::usage::seconds_left(id);
            if wait > 20 {
                // retry_after 존중 — 마지막 계정이어도 리밋 중이면 건너뛰고
                // 다음 연결로 폴백한다 (429 재시도 폭풍 방지).
                continue;
            }
            if wait > 0 && wait <= 20 {
                tokio::time::sleep(Duration::from_secs(wait as u64)).await;
            }
            let Ok(client) = build_provider_account(cfg, candidate.provider, id) else {
                continue;
            };

            // 스트림 실패(네트워크·5xx·미완료 EOF)는 짧은 백오프 뒤 같은 계정으로 재시도한다.
            // 화면에 이미 텍스트가 흘러나간 뒤의 재시도·폴백은 중복 출력을 만드므로 금지한다.
            let mut attempt = 0u32;
            loop {
                let mut track = |ev: SemanticStreamEvent| {
                    // 진행 신호(ToolArgs)는 화면 출력이 아니므로 재시도 판정에서 제외된다.
                    let chars = match ev {
                        SemanticStreamEvent::ContentCandidate(text) if candidates_visible => {
                            text.chars().count()
                        }
                        _ => ev.displayed_chars(),
                    };
                    emitted.fetch_add(chars, Ordering::Relaxed);
                    on_event(ev);
                };
                if let Err(limit) = gate.claim() {
                    return Err(attempt_limit_or_observed(
                        limit,
                        &mut primary_err,
                        &mut last_err,
                    ));
                }
                match client
                    .chat_semantic_stream(&req, &mut track)
                    .await
                    .and_then(|resp| {
                        crate::provider::validate_chat_response(&resp, "fallback")?;
                        Ok(resp)
                    }) {
                    Ok(resp) => {
                        crate::usage::record_success(id, &resp);
                        crate::usage::apply_hint(id, &resp.limit);
                        warn_model_mismatch(&req.model, &resp);
                        return Ok((candidate.provider.to_string(), resp));
                    }
                    Err(e) => {
                        let action =
                            stream_failure_action(&e, emitted.load(Ordering::Relaxed), attempt);
                        if is_typed_rate_limited(&e) {
                            crate::usage::mark_limited(id, rate_limit_retry_after(&e));
                            crate::ui::warn("리밋 → 다음 계정으로 전환");
                        }
                        last_err = Some(e);
                        match action {
                            StreamFailureAction::AbortAfterEmission => {
                                return Err(last_err
                                    .take()
                                    .map(sanitized_provider_error)
                                    .unwrap_or_else(|| anyhow!("응답 도중 스트림이 끊겼습니다")));
                            }
                            StreamFailureAction::RetrySameAccount => {
                                match can_retry_same_account(
                                    gate,
                                    reserve_for_secondary(candidate_index, candidate_count),
                                ) {
                                    Ok(true) => {}
                                    Ok(false) => break,
                                    Err(limit) => {
                                        return Err(attempt_limit_or_observed(
                                            limit,
                                            &mut primary_err,
                                            &mut last_err,
                                        ));
                                    }
                                }
                                attempt += 1;
                                tokio::time::sleep(Duration::from_millis(800 * u64::from(attempt)))
                                    .await;
                            }
                            StreamFailureAction::NextAccount => break,
                            StreamFailureAction::NextCandidate => {
                                promote_primary_error(
                                    candidate.provider,
                                    primary,
                                    &mut primary_err,
                                    &mut last_err,
                                );
                                continue 'candidates;
                            }
                        }
                    }
                }
            }
        }
        promote_primary_error(candidate.provider, primary, &mut primary_err, &mut last_err);
    }
    Err(primary_err
        .or(last_err)
        .map(sanitized_provider_error)
        .unwrap_or_else(|| anyhow!("사용 가능한 프로바이더가 없습니다")))
}

pub async fn chat_accounts(cfg: &Config, provider: &str, req: ChatRequest) -> Result<ChatResponse> {
    let mut gate = ProviderAttemptGate::local();
    try_accounts(cfg, provider, &mut gate, false, |client| {
        let req = req.clone();
        async move { client.chat(&req).await }
    })
    .await
}

pub fn print_binding(cfg: &Config, b: &Binding) {
    crate::ui::note(&format!(
        "Harness  {} → {}  ·  {}/{}{}",
        b.class.as_str(),
        b.profile_name,
        b.provider_name,
        crate::ui::bold(&b.model),
        engine_suffix(cfg)
    ));
}

/// 기본값이 아닐 때만 붙는 실행 축 표시 — ` · engine=claude · graph · team`.
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
    if team_mode(cfg) == crate::engine::TeamMode::Multi {
        s.push_str("  ·  team");
    }
    s
}

/// working 패널 마지막 줄에 상시 표시하는 실행 축 요약 (§16.2).
/// 기본값도 생략하지 않는다 — "지금 무엇으로 도는가"를 매 턴 한 줄로 못 박는 자리다.
pub fn mode_line(cfg: &Config) -> String {
    let (engine, legacy_self) = crate::engine::normalize(&cfg.file.general.engine);
    let spec = crate::engine::resolve_with(&cfg.file.engines, &engine);
    // legacy `engine = "self"` 는 rafikx+self 메타로 실행된다 — 설정 원값을 함께
    // 보여줘야 사용자가 "내가 고른 self 가 왜 rafikx 로 보이나" 혼란이 없다.
    let engine = if legacy_self {
        format!("self(={engine}+self)")
    } else {
        engine
    };
    let pin = if spec.pin().is_some() { "(고정)" } else { "" };
    let self_layer = if crate::self_harness::meta_active(cfg) {
        format!(
            "self v{}",
            crate::self_harness::SelfHarnessState::load().version
        )
    } else {
        "self off".to_string()
    };
    let gate = if spec.verify_policy == crate::engine::VerifyPolicy::Strict
        && cfg.file.harness.strict_gate
    {
        "gate on"
    } else {
        "gate off"
    };
    format!(
        "engine={engine}{pin} · team={} · discipline={} · {self_layer} · {gate}",
        team_mode(cfg).as_str(),
        crate::engine::normalize_discipline(&cfg.file.general.discipline).as_str(),
    )
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
    let label = safe_ping_label(name);
    let Ok(p) = cfg.provider(name) else {
        return format!("{label}: config 없음");
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build();
    let Ok(client) = client else {
        return format!("{label}: HTTP 클라이언트 실패");
    };
    let cred = crate::auth::resolve_credential(cfg, name).ok().flatten();
    match p.kind.as_str() {
        "anthropic" => {
            let Some(c) = cred else {
                return format!("{label}: 미연결 (ping 생략)");
            };
            let req = crate::auth::apply_anthropic_cred(
                client
                    .get("https://api.anthropic.com/v1/models")
                    .header("anthropic-version", "2023-06-01"),
                &c,
            );
            match req.send().await {
                Ok(r) if r.status().is_success() => format!("{label}: ping OK"),
                Ok(r) => format!("{label}: ping HTTP {}", r.status().as_u16()),
                Err(error) => ping_failure(label, &error),
            }
        }
        "openai_compat" => {
            let oauth_openai = name == "openai" && cred.as_ref().is_some_and(|c| c.oauth);
            let url = if oauth_openai {
                "https://chatgpt.com/backend-api/codex/models".to_string()
            } else {
                let Some(base) = &p.base_url else {
                    return format!("{label}: base_url 없음");
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
                Ok(r) if r.status().is_success() => format!("{label}: ping OK"),
                Ok(r) => format!("{label}: ping HTTP {}", r.status().as_u16()),
                Err(error) => ping_failure(label, &error),
            }
        }
        _ => format!("{label}: 지원하지 않는 kind (ping 생략)"),
    }
}

fn safe_ping_label(name: &str) -> &str {
    if !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || "_- .".contains(character))
    {
        name
    } else {
        "provider"
    }
}

fn ping_failure(label: &str, error: &reqwest::Error) -> String {
    let category = if error.is_timeout() {
        "시간 초과"
    } else if error.is_connect() {
        "연결 실패"
    } else if error.is_body() || error.is_decode() {
        "응답 읽기 실패"
    } else {
        "요청 실패"
    };
    format!("{label}: ping 실패 ({category})")
}

#[cfg(test)]
mod ping_tests {
    use super::*;

    #[tokio::test]
    async fn ping_failure_never_exposes_configured_url_or_query_secret() {
        let secret = "do-not-leak";
        let name = format!("custom?token={secret}");
        let workspace =
            std::env::temp_dir().join(format!("rafikx-ping-redaction-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&workspace).expect("ping workspace");
        let mut cfg = Config::load(Some(&workspace.join("config.toml"))).expect("ping config");
        cfg.file.providers.insert(
            name.clone(),
            ProviderConfig {
                kind: "openai_compat".into(),
                auth: "none".into(),
                api_key_env: String::new(),
                model: "test-model".into(),
                small_model: None,
                base_url: Some(format!(
                    "http://127.0.0.1:0/v1?api_key={secret}&redirect=https://secret.invalid"
                )),
                supports_tools: false,
                models_url: None,
                model_auto: false,
                context_window: Some(8_000),
                enabled: true,
            },
        );

        let result = ping_provider(&cfg, &name).await;

        assert!(result.starts_with("provider: ping 실패 ("));
        assert!(!result.contains(secret));
        assert!(!result.contains("api_key"));
        assert!(!result.contains("secret.invalid"));
        let _ = std::fs::remove_dir_all(workspace);
    }
}

#[cfg(test)]
mod lane_filter_tests {
    use super::*;

    #[test]
    fn explorer_binding_excludes_mutation_tools() {
        let filtered = lane_filtered_tools(
            "explorer",
            &[
                "read_file".into(),
                "edit_file".into(),
                "bash".into(),
                "grep".into(),
            ],
        );
        assert_eq!(filtered, vec!["read_file", "grep"]);
    }

    #[test]
    fn researcher_binding_keeps_only_web_and_read() {
        let filtered = lane_filtered_tools(
            "researcher",
            &[
                "web_search".into(),
                "write_file".into(),
                "read_file".into(),
                "bash".into(),
            ],
        );
        assert_eq!(filtered, vec!["web_search", "read_file"]);
    }

    #[test]
    fn reviewer_binding_excludes_every_mutating_or_executable_tool() {
        let filtered = lane_filtered_tools(
            "reviewer",
            &[
                "read_file".into(),
                "write_file".into(),
                "bash".into(),
                "grep".into(),
                "remember".into(),
            ],
        );
        assert_eq!(filtered, vec!["read_file", "grep"]);
    }

    #[test]
    fn non_lane_profiles_are_untouched() {
        let tools = vec!["read_file".into(), "write_file".into(), "bash".into()];
        let filtered = lane_filtered_tools("backend", &tools);
        assert_eq!(filtered, tools);
    }

    #[test]
    fn builtin_lane_presets_exist_with_read_only_tools() {
        let explorer = crate::config::builtin_profile("explorer").expect("explorer 프리셋");
        assert!(explorer.tools.iter().all(|t| {
            ![
                "edit_file",
                "multi_edit",
                "write_file",
                "apply_patch",
                "bash",
            ]
            .contains(&t.as_str())
        }));
        let researcher = crate::config::builtin_profile("researcher").expect("researcher 프리셋");
        assert!(researcher.tools.contains(&"web_search".to_string()));
        let reviewer = crate::config::builtin_profile("reviewer").expect("reviewer 프리셋");
        assert!(reviewer.tools.iter().all(|tool| {
            crate::harness::lane_tool_allowlist("reviewer")
                .expect("reviewer allowlist")
                .contains(&tool.as_str())
        }));
    }
}

#[cfg(test)]
mod combo_tests {
    use super::*;

    fn cfg_with_combos() -> crate::config::Config {
        let dir = std::env::temp_dir().join(format!("rafikx-combo-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = crate::config::Config::load(Some(&dir.join("config.toml"))).unwrap();
        cfg.file.combos.insert(
            "메인".into(),
            vec![
                "claude/opus".into(),
                "kimi/k2".into(),
                "minimax/m2".into(),
                "openai/gpt".into(), // 4번째 — COMBO_MAX_HOPS(3)에서 잘려야 한다
            ],
        );
        cfg
    }

    #[test]
    fn chain_specs_are_capped_at_max_hops() {
        let cfg = cfg_with_combos();
        let specs = combo_chain_specs(&cfg, "메인").unwrap();
        assert_eq!(specs.len(), COMBO_MAX_HOPS);
        assert_eq!(specs[0], "claude/opus");
        assert!(!specs.iter().any(|s| s == "openai/gpt"));
    }

    #[test]
    fn unknown_combo_is_error() {
        let cfg = cfg_with_combos();
        assert!(combo_chain_specs(&cfg, "없는콤보").is_err());
    }

    #[test]
    fn empty_chain_is_error() {
        let mut cfg = cfg_with_combos();
        cfg.file.combos.insert("빈콤보".into(), vec![]);
        assert!(combo_chain_specs(&cfg, "빈콤보").is_err());
    }

    #[test]
    fn no_combos_means_no_regression() {
        let dir = std::env::temp_dir().join(format!("rafikx-nocombo-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = crate::config::Config::load(Some(&dir.join("config.toml"))).unwrap();
        assert!(cfg.file.combos.is_empty());
        // 콤보 미설정 시 model_override "combo:x" 만 오류 — 일반 경로는 무영향
        assert!(combo_chain_specs(&cfg, "x").is_err());
    }

    #[test]
    fn combo_candidates_keep_same_provider_models_in_order() {
        let combo = vec![
            ("openai".to_string(), "gpt-5.6-sol".to_string()),
            ("openai".to_string(), "gpt-5.6-codex".to_string()),
            ("anthropic".to_string(), "claude-opus".to_string()),
        ];
        let candidates = fallback_candidates(&[], Some(&combo));
        let actual: Vec<_> = candidates
            .iter()
            .map(|candidate| (candidate.provider, candidate.combo_model))
            .collect();
        assert_eq!(
            actual,
            vec![
                ("openai", Some("gpt-5.6-sol")),
                ("openai", Some("gpt-5.6-codex")),
                ("anthropic", Some("claude-opus")),
            ]
        );
    }

    #[test]
    fn stream_failures_rotate_accounts_before_candidates_without_output() {
        let rate_limited = crate::provider::ProviderRequestError::new(
            "OpenAI",
            crate::provider::ProviderRequestErrorKind::RateLimited { retry_after: 7 },
        )
        .into();
        let auth = crate::provider::ProviderRequestError::new(
            "OpenAI",
            crate::provider::ProviderRequestErrorKind::Auth { status: 401 },
        )
        .into();
        let timeout = crate::provider::ProviderRequestError::new(
            "OpenAI",
            crate::provider::ProviderRequestErrorKind::Timeout,
        )
        .into();
        let client = crate::provider::ProviderRequestError::new(
            "OpenAI",
            crate::provider::ProviderRequestErrorKind::Client { status: 400 },
        )
        .into();
        let server = crate::provider::ProviderRequestError::new(
            "OpenAI",
            crate::provider::ProviderRequestErrorKind::Server { status: 500 },
        )
        .into();
        assert_eq!(
            stream_failure_action(&rate_limited, 0, 0),
            StreamFailureAction::NextAccount
        );
        assert_eq!(rate_limit_retry_after(&rate_limited), 7);
        assert_eq!(
            stream_failure_action(&auth, 0, 0),
            StreamFailureAction::NextAccount
        );
        assert_eq!(
            stream_failure_action(&timeout, 0, 2),
            StreamFailureAction::NextCandidate
        );
        assert_eq!(
            stream_failure_action(&client, 0, 0),
            StreamFailureAction::NextCandidate
        );
        assert_eq!(
            stream_failure_action(&server, 1, 0),
            StreamFailureAction::AbortAfterEmission
        );
    }

    #[test]
    fn untyped_http_like_errors_never_control_fallback_routing() {
        for message in [
            "HTTP 401 token=secret",
            "HTTP 429 rate_limited",
            "stream timed out",
            "HTTP 500 https://secret.invalid/v1?key=do-not-log",
        ] {
            let error = anyhow!(message);
            assert!(!is_auth_failure(&error));
            assert!(!is_typed_rate_limited(&error));
            assert!(!request_is_retryable(&error));
            assert_eq!(rate_limit_retry_after(&error), 45);
            assert_eq!(
                stream_failure_action(&error, 0, 0),
                StreamFailureAction::NextCandidate
            );
            assert_eq!(
                nonstream_failure_action(&error),
                NonstreamFailureAction::NextCandidate
            );
            assert_eq!(
                crate::provider::safe_summary(&error),
                "provider operation failed"
            );
        }
    }

    #[test]
    fn protocol_failures_have_exhaustive_fallback_routing() {
        use crate::provider::ProtocolErrorKind;

        for kind in [
            ProtocolErrorKind::EmptyResponse,
            ProtocolErrorKind::InvalidTool,
            ProtocolErrorKind::InvalidJson,
            ProtocolErrorKind::InvalidSequence,
            ProtocolErrorKind::TruncatedStream,
            ProtocolErrorKind::UpstreamError,
        ] {
            let error = crate::provider::protocol_error("test", kind);
            assert_eq!(
                stream_failure_action(&error, 0, 0),
                StreamFailureAction::RetrySameAccount,
                "unexpected stream routing for {kind:?}"
            );
            assert_eq!(
                stream_failure_action(&error, 0, 1),
                StreamFailureAction::NextCandidate,
                "retryable protocol error must leave the candidate after one retry for {kind:?}"
            );
            assert_eq!(
                nonstream_failure_action(&error),
                NonstreamFailureAction::NextAccount,
                "unexpected nonstream routing for {kind:?}"
            );
            assert_eq!(
                stream_failure_action(&error, 1, 0),
                StreamFailureAction::AbortAfterEmission,
                "displayed output must win for {kind:?}"
            );
        }

        let limit = crate::provider::protocol_error("test", ProtocolErrorKind::LimitExceeded);
        assert_eq!(
            stream_failure_action(&limit, 0, 0),
            StreamFailureAction::NextCandidate
        );
        assert_eq!(
            nonstream_failure_action(&limit),
            NonstreamFailureAction::NextCandidate
        );
        assert_eq!(
            stream_failure_action(&limit, 1, 0),
            StreamFailureAction::AbortAfterEmission
        );
    }

    #[test]
    fn in_run_attempt_gate_caps_each_call_without_consuming_the_run_budget() {
        let run = crate::run::RunContext::isolated(
            crate::run::RunId::new("dual-provider-attempt-gate"),
            std::env::temp_dir(),
        );
        assert_eq!(run.ensure_provider_attempt_limit(7), 7);

        let mut first_call = ProviderAttemptGate::in_run(&run);
        assert!(first_call.claim().is_ok());
        assert!(first_call.claim().is_ok());
        assert!(first_call.claim().is_ok());
        let error = first_call
            .claim()
            .expect_err("one fallback invocation must stop before a fourth dispatch");
        assert!(is_provider_attempt_limit_exceeded(&error));
        assert_eq!(run.provider_attempts_used(), 3);

        let mut second_call = ProviderAttemptGate::in_run(&run);
        assert!(second_call.claim().is_ok());
        assert_eq!(run.provider_attempts_used(), 4);
    }

    #[test]
    fn primary_account_rotation_reserves_the_third_attempt_for_fallback() {
        let mut gate = ProviderAttemptGate::local();
        assert_eq!(gate.can_dispatch(1).expect("first primary slot"), true);
        assert!(gate.claim().is_ok());
        assert_eq!(gate.can_dispatch(1).expect("second primary slot"), true);
        assert!(gate.claim().is_ok());
        assert_eq!(gate.can_dispatch(1).expect("reserved fallback slot"), false);
        assert_eq!(gate.can_dispatch(0).expect("fallback slot"), true);
        assert!(gate.claim().is_ok());
        assert!(
            gate.can_dispatch(0)
                .expect_err("fourth dispatch must report local exhaustion")
                .is::<ProviderAttemptLimitExceeded>()
        );
    }

    #[test]
    fn only_the_primary_candidate_reserves_a_secondary_slot() {
        assert!(reserve_for_secondary(0, 3));
        assert!(!reserve_for_secondary(1, 3));
        assert!(!reserve_for_secondary(2, 3));
        assert!(!reserve_for_secondary(0, 1));
    }

    #[test]
    fn auth_then_retryable_primary_failure_preserves_the_secondary_slot() {
        let auth: anyhow::Error = crate::provider::ProviderRequestError::new(
            "OpenAI",
            crate::provider::ProviderRequestErrorKind::Auth { status: 401 },
        )
        .into();
        let server: anyhow::Error = crate::provider::ProviderRequestError::new(
            "OpenAI",
            crate::provider::ProviderRequestErrorKind::Server { status: 503 },
        )
        .into();
        let mut gate = ProviderAttemptGate::local();

        assert!(gate.claim().is_ok());
        assert_eq!(
            stream_failure_action(&auth, 0, 0),
            StreamFailureAction::NextAccount
        );
        assert_eq!(gate.can_dispatch(1).expect("second primary account"), true);
        assert!(gate.claim().is_ok());
        assert_eq!(
            stream_failure_action(&server, 0, 0),
            StreamFailureAction::RetrySameAccount
        );
        assert_eq!(
            can_retry_same_account(&gate, true).expect("reserved secondary slot"),
            false
        );
        assert_eq!(gate.can_dispatch(0).expect("secondary dispatch"), true);
        assert!(gate.claim().is_ok());
    }

    #[test]
    fn fallback_summary_never_includes_untyped_error_details() {
        let secret = "https://secret.invalid/v1?api_key=do-not-log";
        let error = anyhow!("request failed at {secret}");

        let summary = fallback_summary(&error, "trying next provider");

        assert_eq!(summary, "provider operation failed; trying next provider");
        assert!(!summary.contains(secret));
        assert!(!summary.contains("api_key"));

        let returned = sanitized_provider_error(error).to_string();
        assert_eq!(returned, "provider operation failed");
        assert!(!returned.contains(secret));
    }

    /// 요청마다 같은 HTTP 오류만 돌려주는 최소 공급자 — 받은 dispatch 수를 센다.
    async fn spawn_always_failing(
        status: u16,
        reason: &'static str,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("mock listener");
        let port = listener.local_addr().expect("mock addr").port();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&requests);
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // 요청 본문까지 받아야 클라이언트가 응답을 정상 수신한다.
                let mut raw = Vec::new();
                let mut chunk = [0u8; 4096];
                while let Ok(read) = socket.read(&mut chunk).await {
                    if read == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..read]);
                    if let Some(head_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&raw[..head_end]).to_lowercase();
                        let body_len = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if raw.len() >= head_end + 4 + body_len {
                            break;
                        }
                    }
                }
                let body = format!(r#"{{"error":{{"message":"forced {status}"}}}}"#);
                let head = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://127.0.0.1:{port}/v1"), requests, server)
    }

    async fn spawn_always_500() -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        spawn_always_failing(500, "Internal Server Error").await
    }

    fn insert_mock_provider(cfg: &mut Config, name: &str, base_url: &str) {
        cfg.file.providers.insert(
            name.to_string(),
            ProviderConfig {
                kind: "openai_compat".into(),
                auth: "none".into(),
                api_key_env: String::new(),
                model: "mock-model".into(),
                small_model: None,
                base_url: Some(base_url.to_string()),
                supports_tools: false,
                models_url: None,
                model_auto: false,
                context_window: Some(8_000),
                enabled: true,
            },
        );
    }

    fn cfg_with_mock_providers(base_url: &str, names: &[String]) -> Config {
        let workspace =
            std::env::temp_dir().join(format!("rafikx-attempt-budget-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&workspace).expect("attempt budget workspace");
        let mut cfg =
            Config::load(Some(&workspace.join("config.toml"))).expect("attempt budget config");
        for name in names {
            insert_mock_provider(&mut cfg, name, base_url);
        }
        cfg
    }

    fn mock_request() -> ChatRequest {
        ChatRequest {
            model: "mock-model".into(),
            system: String::new(),
            messages: vec![crate::provider::Message::user_text("시도 예산 회귀 검증")],
            tools: Vec::new(),
            max_tokens: 64,
            stream: true,
        }
    }

    #[tokio::test]
    async fn attempt_exhaustion_preserves_the_observed_provider_error() {
        let (base_url, requests, server) = spawn_always_500().await;
        let suffix = crate::db::Db::new_id();
        let names = vec![format!("only500a{suffix}"), format!("only500b{suffix}")];
        let cfg = cfg_with_mock_providers(&base_url, &names);

        let error = stream_with_fallback(&cfg, &names, "main", mock_request(), |_event| {})
            .await
            .expect_err("500 만 돌려주는 공급자는 성공할 수 없다");

        assert!(
            !is_provider_attempt_limit_exceeded(&error),
            "관측된 500 이 예산 소진 오류로 둔갑했다: {error}"
        );
        assert!(
            error.to_string().contains("HTTP 500"),
            "공급자 오류가 보존되지 않았다: {error}"
        );
        let dispatched = requests.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            dispatched <= COMBO_MAX_HOPS,
            "dispatch 상한({COMBO_MAX_HOPS})을 넘었다: {dispatched}"
        );
        server.abort();
        let _ = std::fs::remove_dir_all(cfg.path.parent().expect("config dir"));
    }

    #[tokio::test]
    async fn attempt_exhaustion_without_observed_error_still_reports_the_limit() {
        let (base_url, requests, server) = spawn_always_500().await;
        let names = vec![format!("only500c{}", crate::db::Db::new_id())];
        let cfg = cfg_with_mock_providers(&base_url, &names);
        let run = crate::run::RunContext::isolated(
            crate::run::RunId::new("provider-attempt-budget-exhausted"),
            std::env::temp_dir(),
        );
        assert_eq!(run.ensure_provider_attempt_limit(1), 1);
        // 공유 예산을 앞선 호출이 이미 다 쓴 상태 — 이번 호출은 dispatch 전에 막힌다.
        let mut spent = ProviderAttemptGate::in_run(&run);
        assert!(spent.claim().is_ok());

        let error = chat_with_fallback_in_run(&cfg, &run, &names, "main", mock_request())
            .await
            .expect_err("공유 예산이 소진되면 호출은 실패한다");

        assert!(
            is_provider_attempt_limit_exceeded(&error),
            "관측 오류가 전혀 없으면 예산 소진이 그대로 보고돼야 한다: {error}"
        );
        assert_eq!(requests.load(std::sync::atomic::Ordering::Relaxed), 0);
        server.abort();
        let _ = std::fs::remove_dir_all(cfg.path.parent().expect("config dir"));
    }

    #[tokio::test]
    async fn next_candidate_switch_preserves_the_primary_error() {
        let (primary_url, primary_hits, primary_server) = spawn_always_500().await;
        let (secondary_url, secondary_hits, secondary_server) =
            spawn_always_failing(401, "Unauthorized").await;
        let suffix = crate::db::Db::new_id();
        let names = vec![
            format!("primary500{suffix}"),
            format!("secondary401{suffix}"),
        ];
        let mut cfg = cfg_with_mock_providers(&primary_url, &names[..1]);
        insert_mock_provider(&mut cfg, &names[1], &secondary_url);

        let error = stream_with_fallback(&cfg, &names, "main", mock_request(), |_event| {})
            .await
            .expect_err("두 공급자 모두 실패하면 호출도 실패한다");

        assert!(
            error.to_string().contains("HTTP 500"),
            "주 연결 오류가 폴백 오류에 덮였다: {error}"
        );
        assert!(
            !error.to_string().contains("HTTP 401"),
            "마지막 폴백 오류가 원인으로 보고됐다: {error}"
        );
        assert!(primary_hits.load(std::sync::atomic::Ordering::Relaxed) > 0);
        assert!(secondary_hits.load(std::sync::atomic::Ordering::Relaxed) > 0);
        primary_server.abort();
        secondary_server.abort();
        let _ = std::fs::remove_dir_all(cfg.path.parent().expect("config dir"));
    }
}

#[cfg(test)]
mod memory_tool_tests {
    use super::*;

    #[test]
    fn memory_tools_appended_to_tool_profiles() {
        let tools = with_memory_tools(&["read_file".into(), "grep".into()]);
        for m in ["remember", "recall", "forget"] {
            assert!(tools.iter().any(|t| t == m), "{m} 누락");
        }
    }

    #[test]
    fn empty_tool_profiles_stay_tool_free() {
        assert!(with_memory_tools(&[]).is_empty());
    }

    #[test]
    fn wildcard_profiles_unchanged() {
        let tools = with_memory_tools(&["*".into()]);
        assert_eq!(tools, vec!["*"]);
    }

    #[test]
    fn memory_intent_is_not_simple() {
        assert_ne!(
            crate::harness::classify_rules("이 프로젝트는 pytest 써. 기억해줘", false),
            crate::harness::TaskClass::Simple
        );
        assert_ne!(
            crate::harness::classify_rules("이전에 뭐라고 했는지 기억나?", false),
            crate::harness::TaskClass::Simple
        );
    }
}

#[cfg(test)]
mod model_match_tests {
    use super::*;

    #[test]
    fn exact_and_variant_ids_match() {
        assert!(model_matches("kimi-k2.5", "kimi-k2.5"));
        assert!(model_matches("kimi-k2.5", "kimi-k2.5-20250901"));
        assert!(model_matches("gpt-5", "gpt-5.1"));
    }

    #[test]
    fn different_models_do_not_match() {
        assert!(!model_matches("x-preview-f-free", "gpt-5.6"));
        assert!(!model_matches("MiniMax-M3", "glm-5.2"));
    }

    #[test]
    fn empty_sides_are_silent() {
        assert!(model_matches("", "anything"));
        assert!(model_matches("anything", ""));
    }
}
