use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

/// 5.2절 기본 config.toml (주석 포함). 최초 실행 시 이 내용을 그대로 기록한다.
pub const DEFAULT_CONFIG: &str = r#"# agent-harness 설정 — API 키 원문은 여기에 적지 마세요.

[general]
default_provider = "anthropic"
workspace = "~/dev/playground"     # 파일/bash 도구 접근 루트 (이 밖은 차단)
max_tokens = 8192
max_context_chars = 200000
approval = "ask"                   # ask | auto-safe | yolo
classifier = "rules"               # rules | llm

[providers.anthropic]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"  # 환경변수 '이름'만 기록 (키 원문 저장 금지)
model = "claude-sonnet-4-6"        # 예시 — 시점에 맞는 모델 ID로 교체
small_model = "claude-haiku-4-5"
supports_tools = true

[providers.local]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"   # Ollama
model = "qwen3:8b"                       # 설치한 로컬 모델명으로 교체
api_key_env = ""
supports_tools = false                   # 도구 미지원 → 하네스가 자동으로 도구 작업에서 제외

[harness]
simple   = "quick"
medium   = "worker"
advanced = "thinker"
dev      = "coder"
fallback = ["anthropic", "local"]

[subagents.quick]
provider = "local"
model_role = "small"
tools = []
max_iterations = 3
plan_first = false
verify = false
system_extra = "짧고 정확하게 답한다. 불필요한 설명을 붙이지 않는다."

[subagents.worker]
provider = "anthropic"
model_role = "small"
tools = ["read_file","list_dir","grep","obsidian_search"]
max_iterations = 10
plan_first = false
verify = false
system_extra = "자료를 찾아 정확히 정리하는 실무 보조자다. 출처(파일 경로)를 밝힌다."

[subagents.thinker]
provider = "anthropic"
model_role = "main"
tools = ["read_file","list_dir","grep","obsidian_search","write_file"]
max_iterations = 15
plan_first = true
verify = false
system_extra = "복잡한 문제를 구조화하는 분석가다. 결론 전에 근거를 제시한다."

[subagents.coder]
provider = "anthropic"
model_role = "main"
tools = ["*"]
max_iterations = 25
plan_first = true
verify = true
verify_command = ""
system_extra = "신중한 시니어 개발자다. 수정 전 반드시 원문을 읽고, 최소 diff로 고친다."

[memory]
enabled = true
max_lessons = 500
inject_limit_chars = 2000

[inspector]
subagent = "thinker"
auto_interval_hours = 24
notify_telegram = true

[obsidian]
vault_path = "~/Documents/TestVault"
db_path = "~/.agent-harness/data.db"
context_limit_chars = 12000
tokenizer = "unicode61"

[telegram]
enabled = true
token_env = "TELEGRAM_BOT_TOKEN"
allowed_user_ids = [123456789]
allow_agent = false
approval_timeout_secs = 300
"#;

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigFile {
    pub general: GeneralConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub harness: HarnessConfig,
    pub subagents: HashMap<String, SubAgentConfig>,
    pub memory: MemoryConfig,
    #[allow(dead_code)]
    pub inspector: InspectorConfig,
    pub obsidian: ObsidianConfig,
    #[allow(dead_code)]
    pub telegram: TelegramConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneralConfig {
    pub default_provider: String,
    pub workspace: String,
    pub max_tokens: u32,
    pub max_context_chars: u32,
    #[allow(dead_code)]
    pub approval: String,
    pub classifier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub kind: String,
    #[serde(default)]
    pub api_key_env: String,
    pub model: String,
    #[serde(default)]
    pub small_model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub supports_tools: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HarnessConfig {
    pub simple: String,
    pub medium: String,
    pub advanced: String,
    pub dev: String,
    pub fallback: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubAgentConfig {
    pub provider: String,
    pub model_role: String,
    #[serde(default)]
    pub tools: Vec<String>,
    pub max_iterations: u32,
    #[serde(default)]
    pub plan_first: bool,
    #[serde(default)]
    pub verify: bool,
    #[serde(default)]
    pub verify_command: String,
    #[serde(default)]
    pub system_extra: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub max_lessons: u32,
    pub inject_limit_chars: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct InspectorConfig {
    pub subagent: String,
    pub auto_interval_hours: f64,
    pub notify_telegram: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ObsidianConfig {
    pub vault_path: String,
    pub db_path: String,
    pub context_limit_chars: u32,
    pub tokenizer: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub token_env: String,
    pub allowed_user_ids: Vec<i64>,
    pub allow_agent: bool,
    pub approval_timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub path: PathBuf,
    #[allow(dead_code)]
    pub data_dir: PathBuf,
    pub workspace: PathBuf,
    pub file: ConfigFile,
}

impl Config {
    pub fn data_dir() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("홈 폴더를 찾을 수 없습니다"))?;
        Ok(home.join(".agent-harness"))
    }

    pub fn default_path() -> Result<PathBuf> {
        Ok(Self::data_dir()?.join("config.toml"))
    }

    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let data_dir = Self::data_dir()?;
        fs::create_dir_all(&data_dir).with_context(|| format!("{} 폴더를 만들 수 없습니다", data_dir.display()))?;

        let path = match explicit {
            Some(p) => expand_tilde(&p.to_string_lossy()),
            None => Self::default_path()?,
        };

        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, DEFAULT_CONFIG)
                .with_context(|| format!("{} 파일을 만들 수 없습니다", path.display()))?;
            set_owner_only_mode(&path);
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("{} 파일을 읽을 수 없습니다", path.display()))?;
        let file: ConfigFile = toml::from_str(&raw)
            .with_context(|| format!("{} 형식이 올바르지 않습니다", path.display()))?;
        let workspace = expand_tilde(&file.general.workspace);

        Ok(Self {
            path,
            data_dir,
            workspace,
            file,
        })
    }

    pub fn provider(&self, name: &str) -> Result<&ProviderConfig> {
        self.file
            .providers
            .get(name)
            .ok_or_else(|| anyhow!("프로바이더 '{name}' 이(가) config에 없습니다"))
    }

    /// 환경변수에서 키를 읽는다. 없으면 None. 키 원문은 로그에 쓰지 말 것.
    pub fn api_key(&self, provider_name: &str) -> Result<Option<String>> {
        let p = self.provider(provider_name)?;
        if p.api_key_env.is_empty() {
            return Ok(None);
        }
        match std::env::var(&p.api_key_env) {
            Ok(v) if !v.trim().is_empty() => Ok(Some(v)),
            _ => Ok(None),
        }
    }

    /// doctor용: 마지막 4자만. 키가 없으면 None.
    pub fn api_key_tail(&self, provider_name: &str) -> Result<Option<String>> {
        Ok(self.api_key(provider_name)?.map(|k| {
            let tail: String = k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            tail
        }))
    }
}

pub fn expand_tilde(input: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    if input == "~" {
        return home;
    }
    let Some(rest) = input
        .strip_prefix("~/")
        .or_else(|| input.strip_prefix("~\\"))
    else {
        return PathBuf::from(input);
    };
    let mut p = home;
    for part in rest.split(['/', '\\']) {
        if !part.is_empty() {
            p.push(part);
        }
    }
    p
}

fn set_owner_only_mode(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    let _ = path;
}
