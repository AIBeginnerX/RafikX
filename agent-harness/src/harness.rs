use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::agent::{self, AgentOutcome, AgentRun};
use crate::config::{Config, ProviderConfig};
use crate::db::Db;
use crate::provider::{
    AnthropicProvider, ChatRequest, ChatResponse, ContentBlock, DynProvider, Message,
    OpenAiCompatProvider, is_rate_limited, is_retryable,
};
use crate::tools::{self, ToolCtx, ToolRegistry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskClass {
    Simple,
    Medium,
    Advanced,
    Dev,
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
        &["요약", "정리", "번역", "초안", "검색", "찾아", "노트", "문서"],
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
        ".h", ".cs", ".rb", ".php", ".kt", ".swift", ".sh", ".ps1",
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
                || t
                    .chars()
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

pub fn bind(
    cfg: &Config,
    class: TaskClass,
    provider_override: Option<&str>,
    model_override: Option<&str>,
) -> Result<Binding> {
    let profile_name = profile_name_for(cfg, class).to_string();
    let sub = cfg
        .file
        .subagents
        .get(&profile_name)
        .ok_or_else(|| anyhow!("서브에이전트 '{profile_name}' 이(가) config에 없습니다"))?;

    let needs_tools = !sub.tools.is_empty();
    let selection = cfg.file.harness.selection.trim().to_ascii_lowercase();
    let manual = selection == "manual";

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
        crate::applog::debug(&format!("bind: direct pair {p}/{m} tools_ok={}", pc.supports_tools));
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
    } else {
        pick_auto(cfg, class, sub, needs_tools)?
    };

    let p = cfg.provider(&provider_name)?;
    if !crate::auth::is_usable(cfg, &provider_name) && crate::auth::auth_mode(&provider_name, p) != "none"
    {
        if crate::auth::is_connected(cfg, &provider_name) && !crate::auth::is_enabled(cfg, &provider_name) {
            anyhow::bail!(
                "'{provider_name}' 는 사용 중지입니다. rafikx settings 에서 다시 켜세요."
            );
        }
        anyhow::bail!(
            "'{provider_name}' 연결이 없습니다. rafikx settings 에서 번호로 연결하세요."
        );
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

fn resolve_spec(
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

/// 하네스 선정 모드 저장 ("auto" | "manual").
pub fn set_selection_mode(cfg: &Config, mode: &str) -> Result<()> {
    let m = if mode.eq_ignore_ascii_case("manual") {
        "manual"
    } else {
        "auto"
    };
    crate::config::write_toml_key(&cfg.path, "[harness]", "selection", &crate::config::toml_string(m))
}

/// 분류별 수동 모델 지정. 빈 spec 이면 해당 분류의 수동 지정을 지운다(자동 폴백).
pub fn set_manual_model(cfg: &Config, class: TaskClass, spec: &str) -> Result<String> {
    let key = manual_key_for(class);
    if spec.trim().is_empty() {
        // 값 제거: auto 로 덮어쓰고 주석 처리된 형태가 되지 않게 빈 문자열로 둔다.
        crate::config::write_toml_key(&cfg.path, "[harness]", key, &crate::config::toml_string(""))?;
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
    Ok(format!(
        "{} 수동 모델: {} / {}",
        class.as_str(),
        p,
        m
    ))
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

    // ① 기본 연결 우선 — 사용자가 고른 서비스·모델을 존중한다.
    let def = cfg.file.general.default_provider.clone();
    if crate::auth::is_usable(cfg, &def) {
        if let Ok(dp) = cfg.provider(&def) {
            if !needs_tools || dp.supports_tools {
                let model = pick_for_provider(cfg, &def, class, &sub.model_role, needs_tools)
                    .unwrap_or_else(|| model_for_role(dp, &sub.model_role));
                return Ok((def, model, None));
            }
        }
    }

    // ② 기본 연결을 못 쓰면(미연결·도구 미지원) 순위표로 전역 자동 선택.
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
        let pref = if r.provider == preferred_provider { 0 } else { 1 };
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
            Some((e.score, crate::ranks::Tier::parse(&e.tier) == crate::ranks::Tier::Top5, r))
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
                anyhow!(
                    "'{name}' 연결이 없습니다. rafikx settings 에서 번호로 연결하세요"
                )
            })?;
            if c.oauth {
                Ok(DynProvider::Anthropic(AnthropicProvider::with_oauth(c.token)?))
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

async fn try_accounts<F, Fut>(
    cfg: &Config,
    name: &str,
    mut call: F,
) -> Result<ChatResponse>
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
        } else if wait > 20 && i + 1 < ids.len() {
            crate::ui::note(&format!(
                "{} 리밋 {}분 → 다른 계정",
                crate::accounts::get(id)
                    .map(|a| a.label)
                    .unwrap_or_else(|| id.clone()),
                (wait + 59) / 60
            ));
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

/// 백그라운드 작업(교훈 반성 등)이 폴백 실패를 화면에 띄우지 않게 하는 스위치.
static FALLBACK_QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_fallback_quiet(quiet: bool) {
    FALLBACK_QUIET.store(
        quiet,
        std::sync::atomic::Ordering::Relaxed,
    );
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
        let Some(model) = model_for_fallback(cfg, name, model_role, &original_model, primary) else {
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
                fallback_warn(&format!(
                    "{name} 호출 실패 ({}) → 다음 연결",
                    short_err(&e)
                ));
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
        let Some(model) = model_for_fallback(cfg, name, model_role, &original_model, primary) else {
            continue;
        };
        req.model = model;
        let ids = account_ids_for(name);
        for (i, id) in ids.iter().enumerate() {
            let wait = crate::usage::seconds_left(id);
            if wait > 20 && i + 1 < ids.len() {
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
                        crate::usage::mark_limited(id, crate::usage::parse_retry_after(&format!("{e:#}")));
                        crate::ui::warn("리밋 → 다음 계정으로 전환");
                        last_err = Some(e);
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e);
                        if is_retryable(last_err.as_ref().unwrap())
                            && emitted.load(Ordering::Relaxed) == 0
                            && attempt < 2
                        {
                            attempt += 1;
                            tokio::time::sleep(Duration::from_millis(800 * u64::from(attempt))).await;
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

pub async fn chat_accounts(
    cfg: &Config,
    provider: &str,
    req: ChatRequest,
) -> Result<ChatResponse> {
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
        return TaskClass::parse(s).ok_or_else(|| anyhow!("--class 값은 simple|medium|advanced|dev 여야 합니다"));
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

pub fn print_binding(b: &Binding) {
    crate::ui::note(&format!(
        "하네스  {} → {}  ·  {}/{}",
        b.class.as_str(),
        b.profile_name,
        b.provider_name,
        crate::ui::bold(&b.model)
    ));
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
            Ok(b) => println!(
                "  {} → {} → {} ({}) → {}",
                b.class.as_str(),
                b.profile_name,
                b.provider_name,
                b.kind,
                b.model
            ),
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

pub fn system_prompt(cfg: &Config, extra: &str, lessons: &str) -> String {
    let mut s = format!(
        "You are RafikX, a personal CLI assistant.\n\
         Workspace: {}\n\
         If the user writes in Korean, reply in Korean.\n\
         [도표 규칙] 비교·수치·비율을 시각화할 때 ASCII 아트 다이어그램(+---+, ->, 문자 박스)을 그리지 않는다.\n\
         수치 비교는 반드시 아래 형식의 ```chart 블록으로 줘라 — 터미널이 실제 막대그래프로 렌더링한다.\n\
         ```chart\n라벨1: 수치1\n라벨2: 수치2\n```\n\
         항목 나열은 마크다운 표를 쓴다.\n\
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
    let role = cfg
        .file
        .subagents
        .get(&binding.profile_name)
        .map(|s| s.model_role.as_str())
        .unwrap_or("main");
    let order = fallback_order(cfg, &binding.provider_name, cli_provider);
    let lessons_block = if cfg.file.memory.enabled {
        Db::open(&Db::db_path()?)
            .ok()
            .map(|db| {
                crate::lessons::inject_block(
                    &db,
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
        crate::graph::node("pre_step", "lessons", "injected", Some("bind"));
    } else {
        crate::graph::node("pre_step", "lessons", "none", Some("bind"));
    }
    let system = system_prompt(cfg, &binding.system_extra, &lessons_block);

    // 난이도 기반 단계별 실행 (dsh ctx.goals 영향 수용):
    // 단순 업무는 즉답, medium 이상은 todo 스테이징. deepseek/dk 엔진은 모든 도구 작업에 적용.
    let engine = cfg.file.general.engine.to_ascii_lowercase();
    let engine_deep = engine == "deepseek" || engine == "dk";
    let engine_dk = engine == "dk";
    let staged = !binding.tools.is_empty()
        && (engine_deep || binding.class != crate::harness::TaskClass::Simple);
    let mut system = system;
    if staged {
        crate::ui::live_line(&format!(
            "[하네스] {} 난이도{} — 단계별 실행(todo) 활성",
            binding.class.as_str(),
            if engine_deep {
                " · dk/deepseek 엔진"
            } else {
                ""
            }
        ));
        crate::graph::node(
            "pre_step",
            "staging",
            if engine_deep { "deepseek" } else { "auto" },
            Some("bind"),
        );
        let mut directive = String::from(
            "\n\n[실행 방식 — 단계별 처리]\n\
             이 작업은 여러 단계가 필요하다. 다른 도구를 쓰기 전에 먼저 todo_write 로 2~6개의 실행 단계를 등록하고, \
             한 단계를 마칠 때마다 todo_write 로 상태를 갱신하라. \
             모든 단계가 끝나면 단계별 핵심 결과를 짧게 요약해 답을 마친다.",
        );
        if engine_deep {
            directive.push_str(" 각 단계 시작 시 `[N/총M] 단계명` 형식의 한 줄 상태를 출력한다. \
                검증 가능한 작업(빌드·테스트·파일 수정)은 마지막에 검증 방법과 결과를 함께 남긴다.");
        }
        if engine_dk {
            // DeepSeek DSH 호환 모드 — 사고 채널을 먼저 충분히 쓰고 답한다.
            directive.push_str(" 답변 앞추측 없이 먼저 계획을 확정한 뒤 실행하고, 중간 추론은 출력하지 않는다.");
        }
        system.push_str(&directive);
    }

    if binding.plan_first {
        let req = ChatRequest {
            model: binding.model.clone(),
            system: "작업 계획을 3~7개 항목으로만 출력하라. 도구는 쓰지 마라.".into(),
            messages: vec![Message::user_text(task)],
            tools: vec![],
            max_tokens: 1024,
            stream: false,
        };
        match chat_with_fallback(cfg, &order, role, req).await {
            Ok((_n, resp)) => {
                crate::graph::node("plan", "plan_first", "", Some("pre_step"));
                crate::ui::live_line("[계획]");
                for b in &resp.content {
                    if let ContentBlock::Text { text } = b {
                        crate::ui::live_line(text);
                    }
                }
            }
            Err(e) => crate::ui::live_warn(&format!("계획 단계 실패(계속 진행): {e}")),
        }
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
        let (_name, resp) = stream_with_fallback(cfg, &order, role, req, |piece| {
            crate::ui::live_chunk(piece);
        })
        .await?;
        crate::graph::node(
            "request",
            &binding.model,
            &format!("in={} out={}", resp.input_tokens, resp.output_tokens),
            Some("pre_step"),
        );
        crate::ui::live_chunk("\n");
        crate::ui::live_status(&format!(
            "[tokens] in={} out={} stop={:?}",
            resp.input_tokens, resp.output_tokens, resp.stop_reason
        ));
        messages.push(Message {
            role: crate::provider::Role::Assistant,
            content: resp.content.clone(),
        });
        return Ok(AgentOutcome {
            status: "ok".into(),
            iterations: 1,
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            cached_tokens: resp.cached_tokens,
            error: None,
            messages,
            changed_files: vec![],
            tool_errors: vec![],
            deny_reasons: vec![],
            verify_fail: None,
        });
    }

    if !cfg.workspace.exists() {
        std::fs::create_dir_all(&cfg.workspace)?;
        crate::ui::live_line(&format!(
            "워크스페이스 폴더를 만들었습니다: {}",
            cfg.workspace.display()
        ));
    }

    let registry = ToolRegistry::with_names(&binding.tools);
    let mut outcome = agent::run_agent(AgentRun {
        cfg,
        provider_name: &binding.provider_name,
        model: &binding.model,
        task,
        yes,
        max_iterations: binding.max_iterations,
        system: system.clone(),
        registry,
        resume,
        remote: remote.clone(),
        local_ask: local_ask.clone(),
        context_window: binding.context_window,
    })
    .await?;

    if binding.verify {
        crate::graph::node("verify", "start", "", Some("request"));
        crate::spinner::set_label("검증 중…");
        outcome = run_verify(cfg, binding, task, yes, system, outcome, remote, local_ask).await?;
        crate::graph::node("verify", &outcome.status, "", Some("verify"));
    }
    Ok(outcome)
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
) -> Result<AgentOutcome> {
    let mut cmd = binding.verify_command.clone();
    if cmd.trim().is_empty() {
        cmd = auto_verify_command(cfg, &outcome.changed_files);
    }
    if cmd.is_empty() {
        crate::ui::live_line("검증 생략: 자동 감지할 빌드가 없습니다.");
        return Ok(outcome);
    }

    let bash = ToolRegistry::all();
    let Some(tool) = bash.get("bash") else {
        crate::ui::live_line("검증 생략: bash 도구가 없습니다.");
        return Ok(outcome);
    };
    let mut ctx = ToolCtx::new(cfg.workspace.clone());
    ctx.vault = Some(crate::config::expand_tilde(&cfg.file.obsidian.vault_path));
    ctx.db_path = crate::config::expand_tilde(&cfg.file.obsidian.db_path);

    let yes = agent::effective_yes(yes, &remote);
    for round in 0..3 {
        crate::ui::live_line(&format!("[검증] {cmd}"));
        crate::spinner::set_label(&format!("검증 실행: {cmd}"));
        let input = serde_json::json!({"command": cmd});
        if tool.needs_approval(&input) && !yes {
            match tools::approval_preview("bash", &input, &ctx) {
                Ok(p) => {
                    crate::ui::live_line(&p);
                    let denied = if let Some(ask) = &local_ask {
                        !matches!(
                            ask(p.clone()).await,
                            crate::agent::ApprovalChoice::Yes | crate::agent::ApprovalChoice::Always
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
                        crate::ui::live_line("검증이 거부되었습니다.");
                        outcome.status = "denied".into();
                        return Ok(outcome);
                    }
                }
                Err(e) => {
                    crate::ui::live_line(&format!("검증 명령을 실행할 수 없습니다: {e}"));
                    return Ok(outcome);
                }
            }
        }
        match tool.run(serde_json::json!({"command": cmd}), &ctx) {
            Ok(out) if !out.contains("[exit") => {
                crate::ui::live_line("검증 성공");
                crate::ui::live_line(&out);
                return Ok(outcome);
            }
            other => {
                let err = match other {
                    Ok(o) => o,
                    Err(e) => e.to_string(),
                };
                if round >= 2 {
                    crate::ui::live_line("검증이 2회 재시도 후에도 실패했습니다.");
                    crate::ui::live_line(&err);
                    outcome.status = "fail".into();
                    outcome.error = Some(err.chars().take(500).collect());
                    outcome.verify_fail = Some(err.chars().take(500).collect());
                    return Ok(outcome);
                }
                crate::ui::live_line(&format!(
                    "검증 실패, 오류를 되먹여 재시도합니다 ({}/2)",
                    round + 1
                ));
                let cause: String = err.chars().take(500).collect();
                let mut msgs = outcome.messages.clone();
                if msgs.is_empty() {
                    msgs.push(Message::user_text(task));
                }
                msgs.push(Message::user_text(format!(
                    "검증 명령이 실패했습니다. 오류를 고치세요.\n{err}"
                )));
                let mut next = agent::run_agent(AgentRun {
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
                })
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
}
