use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::agent::{self, AgentOutcome, AgentRun};
use crate::config::{Config, ProviderConfig};
use crate::db::Db;
use crate::provider::{
    AnthropicProvider, ChatRequest, ChatResponse, ContentBlock, DynProvider, Message,
    OpenAiCompatProvider, is_retryable,
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
}

pub fn classify_rules(text: &str, obsidian: bool) -> TaskClass {
    if looks_like_dev(text) {
        return TaskClass::Dev;
    }
    if looks_like_advanced(text) {
        return TaskClass::Advanced;
    }
    if obsidian {
        return TaskClass::Medium;
    }
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

    let mut provider_name = provider_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| sub.provider.clone());
    let needs_tools = !sub.tools.is_empty();
    let p = cfg.provider(&provider_name)?;

    if needs_tools && !p.supports_tools {
        let mut rebound = None;
        for name in &cfg.file.harness.fallback {
            if let Ok(fp) = cfg.provider(name) {
                if fp.supports_tools {
                    rebound = Some(name.clone());
                    break;
                }
            }
        }
        match rebound {
            Some(name) => {
                eprintln!(
                    "경고: '{provider_name}' 는 도구를 지원하지 않아 '{name}' 로 바꿉니다."
                );
                provider_name = name;
            }
            None => {
                if matches!(class, TaskClass::Dev | TaskClass::Advanced) {
                    anyhow::bail!(
                        "도구를 지원하는 모델이 등록되어 있지 않습니다. config에 supports_tools=true 프로바이더를 추가하세요."
                    );
                }
            }
        }
    }

    let p = cfg.provider(&provider_name)?;
    let model = if let Some(m) = model_override {
        m.to_string()
    } else {
        model_for_role(p, &sub.model_role)
    };

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
    })
}

fn model_for_role(p: &ProviderConfig, role: &str) -> String {
    if role == "small" {
        p.small_model.clone().unwrap_or_else(|| p.model.clone())
    } else {
        p.model.clone()
    }
}

pub fn build_provider(cfg: &Config, name: &str) -> Result<DynProvider> {
    let p = cfg.provider(name)?;
    match p.kind.as_str() {
        "anthropic" => {
            let key = cfg.api_key(name)?.ok_or_else(|| {
                anyhow!("환경변수 {} 가 없습니다", p.api_key_env)
            })?;
            Ok(DynProvider::Anthropic(AnthropicProvider::new(key)?))
        }
        "openai_compat" => {
            let base = p
                .base_url
                .clone()
                .ok_or_else(|| anyhow!("프로바이더 '{name}' 에 base_url 이 없습니다"))?;
            let key = cfg.api_key(name)?;
            Ok(DynProvider::OpenAi(OpenAiCompatProvider::new(base, key)?))
        }
        other => Err(anyhow!("알 수 없는 프로바이더 kind: {other}")),
    }
}

pub fn fallback_order(cfg: &Config, primary: &str, cli_provider: Option<&str>) -> Vec<String> {
    let mut order = Vec::new();
    if let Some(p) = cli_provider {
        order.push(p.to_string());
    }
    if !order.iter().any(|x| x == primary) {
        order.push(primary.to_string());
    }
    for f in &cfg.file.harness.fallback {
        if !order.iter().any(|x| x == f) {
            order.push(f.clone());
        }
    }
    order
}

pub async fn chat_with_fallback(
    cfg: &Config,
    order: &[String],
    model_role: &str,
    mut req: ChatRequest,
) -> Result<(String, ChatResponse)> {
    let mut last_err = None;
    for name in order {
        let Ok(p) = cfg.provider(name) else { continue };
        req.model = model_for_role(p, model_role);
        let Ok(client) = build_provider(cfg, name) else {
            continue;
        };
        let mut delay = 1u64;
        for attempt in 0..3 {
            match client.chat(&req).await {
                Ok(resp) => return Ok((name.clone(), resp)),
                Err(e) if is_retryable(&e) && attempt < 2 => {
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    delay *= 2;
                    last_err = Some(e);
                }
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("사용 가능한 프로바이더가 없습니다")))
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
    let mut last_err = None;
    for name in order {
        let Ok(p) = cfg.provider(name) else { continue };
        req.model = model_for_role(p, model_role);
        let Ok(client) = build_provider(cfg, name) else {
            continue;
        };
        let mut delay = 1u64;
        for attempt in 0..3 {
            match client.chat_stream(&req, &mut on_text).await {
                Ok(resp) => return Ok((name.clone(), resp)),
                Err(e) if is_retryable(&e) && attempt < 2 => {
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    delay *= 2;
                    last_err = Some(e);
                }
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("사용 가능한 프로바이더가 없습니다")))
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
    println!(
        "[하네스] {} → {} ({})",
        b.class.as_str(),
        b.profile_name,
        b.model
    );
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
    match p.kind.as_str() {
        "anthropic" => {
            let Ok(Some(key)) = cfg.api_key(name) else {
                return format!("{name}: 키 없음 (ping 생략)");
            };
            match client
                .get("https://api.anthropic.com/v1/models")
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => format!("{name}: ping OK"),
                Ok(r) => format!("{name}: ping HTTP {}", r.status().as_u16()),
                Err(e) => format!("{name}: ping 실패 ({e})"),
            }
        }
        "openai_compat" => {
            let Some(base) = &p.base_url else {
                return format!("{name}: base_url 없음");
            };
            let url = format!("{}/models", base.trim_end_matches('/'));
            let mut req = client.get(url);
            if let Ok(Some(key)) = cfg.api_key(name) {
                req = req.header("Authorization", format!("Bearer {key}"));
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
        "You are agent-harness, a personal CLI assistant.\n\
         Workspace: {}\n\
         If the user writes in Korean, reply in Korean.\n\
         {extra}",
        cfg.workspace.display()
    );
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
    }
    let system = system_prompt(cfg, &binding.system_extra, &lessons_block);

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
                println!("[계획]");
                for b in &resp.content {
                    if let ContentBlock::Text { text } = b {
                        println!("{text}");
                    }
                }
            }
            Err(e) => eprintln!("계획 단계 실패(계속 진행): {e}"),
        }
    }

    let use_tools = !binding.tools.is_empty();
    if !use_tools {
        let mut messages = resume.unwrap_or_else(|| vec![Message::user_text(task)]);
        let req = ChatRequest {
            model: binding.model.clone(),
            system,
            messages: messages.clone(),
            tools: vec![],
            max_tokens: cfg.file.general.max_tokens,
            stream: true,
        };
        let (_name, resp) = stream_with_fallback(cfg, &order, role, req, |piece| {
            print!("{piece}");
            let _ = io::stdout().flush();
        })
        .await?;
        println!();
        println!(
            "[tokens] in={} out={} stop={:?}",
            resp.input_tokens, resp.output_tokens, resp.stop_reason
        );
        messages.push(Message {
            role: crate::provider::Role::Assistant,
            content: resp.content.clone(),
        });
        return Ok(AgentOutcome {
            status: "ok".into(),
            iterations: 1,
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
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
        eprintln!("워크스페이스 폴더를 만들었습니다: {}", cfg.workspace.display());
    }

    let client = build_provider(cfg, &binding.provider_name).or_else(|_| {
        let mut last = anyhow!("프로바이더를 만들 수 없습니다");
        for name in &order {
            match build_provider(cfg, name) {
                Ok(c) => return Ok(c),
                Err(e) => last = e,
            }
        }
        Err(last)
    })?;

    let registry = ToolRegistry::with_names(&binding.tools);
    let mut outcome = agent::run_agent(AgentRun {
        cfg,
        client: &client,
        model: &binding.model,
        task,
        yes,
        max_iterations: binding.max_iterations,
        system: system.clone(),
        registry,
        resume,
    })
    .await?;

    if binding.verify {
        outcome = run_verify(cfg, binding, &client, task, yes, system, outcome).await?;
    }
    Ok(outcome)
}

async fn run_verify(
    cfg: &Config,
    binding: &Binding,
    client: &DynProvider,
    task: &str,
    yes: bool,
    system: String,
    mut outcome: AgentOutcome,
) -> Result<AgentOutcome> {
    let mut cmd = binding.verify_command.clone();
    if cmd.trim().is_empty() {
        cmd = auto_verify_command(cfg, &outcome.changed_files);
    }
    if cmd.is_empty() {
        println!("검증 생략: 자동 감지할 빌드가 없습니다.");
        return Ok(outcome);
    }

    let bash = ToolRegistry::all();
    let Some(tool) = bash.get("bash") else {
        println!("검증 생략: bash 도구가 없습니다.");
        return Ok(outcome);
    };
    let mut ctx = ToolCtx::new(cfg.workspace.clone());
    ctx.vault = Some(crate::config::expand_tilde(&cfg.file.obsidian.vault_path));
    ctx.db_path = crate::config::expand_tilde(&cfg.file.obsidian.db_path);

    for round in 0..3 {
        println!("[검증] {cmd}");
        let input = serde_json::json!({"command": cmd});
        if tool.needs_approval(&input) && !yes {
            match tools::approval_preview("bash", &input, &ctx) {
                Ok(p) => {
                    println!("{p}");
                    print!("[y] 이번만  / [n] 거부  / [a] 이번 실행 모두 허용 : ");
                    let _ = io::stdout().flush();
                    let mut line = String::new();
                    io::stdin().read_line(&mut line)?;
                    let t = line.trim().to_lowercase();
                    if t == "n" || t == "no" {
                        println!("검증이 거부되었습니다.");
                        outcome.status = "denied".into();
                        return Ok(outcome);
                    }
                }
                Err(e) => {
                    println!("검증 명령을 실행할 수 없습니다: {e}");
                    return Ok(outcome);
                }
            }
        }
        match tool.run(serde_json::json!({"command": cmd}), &ctx) {
            Ok(out) if !out.contains("[exit") => {
                println!("검증 성공");
                println!("{out}");
                return Ok(outcome);
            }
            other => {
                let err = match other {
                    Ok(o) => o,
                    Err(e) => e.to_string(),
                };
                if round >= 2 {
                    println!("검증이 2회 재시도 후에도 실패했습니다.");
                    println!("{err}");
                    outcome.status = "fail".into();
                    outcome.error = Some(err.chars().take(500).collect());
                    outcome.verify_fail = Some(err.chars().take(500).collect());
                    return Ok(outcome);
                }
                println!("검증 실패, 오류를 되먹여 재시도합니다 ({}/2)", round + 1);
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
                    client,
                    model: &binding.model,
                    task,
                    yes,
                    max_iterations: binding.max_iterations,
                    system: system.clone(),
                    registry: ToolRegistry::with_names(&binding.tools),
                    resume: Some(msgs),
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
    fn classifies_medium_from_obsidian_flag() {
        assert_eq!(classify_rules("안녕", true), TaskClass::Medium);
    }
}
