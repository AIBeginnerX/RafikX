use super::*;

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

pub(crate) fn resolve_spec(cfg: &Config, spec: &str, needs_tools: bool) -> Result<(String, String)> {
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

/// 도구 인자 생성 구간의 진행 표시 문구 — 스트림 소비자 세 곳이 함께 쓴다.
pub fn tool_args_label(name: &str, total_bytes: usize) -> String {
    format!("도구 호출 작성 중: {name} · {}KB", total_bytes / 1024)
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
    mut on_event: F,
) -> Result<(String, ChatResponse)>
where
    F: FnMut(StreamEvent),
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
                let mut track = |ev: StreamEvent| {
                    // 진행 신호(ToolArgs)는 화면 출력이 아니므로 재시도 판정에서 제외된다.
                    emitted.fetch_add(emitted_chars(&ev), Ordering::Relaxed);
                    on_event(ev);
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
                                on_event(StreamEvent::Text("\n[연결 끊김 — 같은 연결로 재시도]\n"));
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
