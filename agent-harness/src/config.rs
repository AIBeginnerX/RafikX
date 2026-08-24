use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

/// 5.2절 기본 config.toml (주석 포함). 최초 실행 시 이 내용을 그대로 기록한다.
pub const DEFAULT_CONFIG: &str = r#"# RafikX 설정 — API 키 원문은 여기에 적지 마세요.

[general]
default_provider = "anthropic"
workspace = "~/dev/playground"     # 파일/bash 도구 접근 루트 (이 밖은 차단)
max_tokens = 8192
max_context_chars = 200000
approval = "ask"                   # ask | auto-safe | yolo
classifier = "rules"               # rules | llm

[providers.anthropic]
kind = "anthropic"
auth = "oauth"                     # oauth | api_key  (키는 환경변수/secrets.toml)
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-6"
small_model = "claude-haiku-4-5"
supports_tools = true
enabled = true

[providers.openai]
kind = "openai_compat"
auth = "oauth"                     # ChatGPT/Codex 로그인
api_key_env = "OPENAI_API_KEY"
base_url = "https://api.openai.com/v1"
model = "gpt-5.1"
small_model = "gpt-4.1"
supports_tools = true

[providers.gemini]
kind = "openai_compat"
auth = "oauth"
api_key_env = "GEMINI_API_KEY"
base_url = "https://generativelanguage.googleapis.com/v1beta/openai"
model = "gemini-2.5-pro"
small_model = "gemini-2.5-flash"
supports_tools = true

[providers.grok]
kind = "openai_compat"
auth = "oauth"                     # xAI는 공개 OAuth가 없어 콘솔 키로 연결
api_key_env = "XAI_API_KEY"
base_url = "https://api.x.ai/v1"
model = "grok-4"
small_model = "grok-3-mini"
supports_tools = true

[providers.openrouter]
kind = "openai_compat"
auth = "api_key"
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-sonnet-4.6"
small_model = "openai/gpt-4.1-mini"
supports_tools = true

# OpenCode Zen — https://opencode.ai/auth 에서 키. Bearer. chat/completions.
# GPT/Claude 일부는 /responses·/messages 전용이라 기본 모델은 GLM/Kimi 계열.
[providers.opencode_zen]
kind = "openai_compat"
auth = "api_key"
api_key_env = "OPENCODE_API_KEY"
base_url = "https://opencode.ai/zen/v1"
model = "glm-5.1"
small_model = "minimax-m2.7"
supports_tools = true

# OpenCode Go — 같은 콘솔에서 구독 후 키. OPENCODE_GO_API_KEY 또는 OPENCODE_API_KEY.
[providers.opencode_go]
kind = "openai_compat"
auth = "api_key"
api_key_env = "OPENCODE_GO_API_KEY"
base_url = "https://opencode.ai/zen/go/v1"
model = "kimi-k2.7-code"
small_model = "glm-5.1"
supports_tools = true

[providers.groq]
kind = "openai_compat"
auth = "api_key"
api_key_env = "GROQ_API_KEY"
base_url = "https://api.groq.com/openai/v1"
model = "llama-3.3-70b-versatile"
small_model = "llama-3.1-8b-instant"
supports_tools = true

[providers.deepseek]
kind = "openai_compat"
auth = "api_key"
api_key_env = "DEEPSEEK_API_KEY"
base_url = "https://api.deepseek.com"
model = "deepseek-chat"
small_model = "deepseek-chat"
supports_tools = true

[providers.mistral]
kind = "openai_compat"
auth = "api_key"
api_key_env = "MISTRAL_API_KEY"
base_url = "https://api.mistral.ai/v1"
model = "mistral-large-latest"
small_model = "mistral-small-latest"
supports_tools = true

[providers.together]
kind = "openai_compat"
auth = "api_key"
api_key_env = "TOGETHER_API_KEY"
base_url = "https://api.together.xyz/v1"
model = "meta-llama/Llama-3.3-70B-Instruct-Turbo"
small_model = "meta-llama/Llama-3.2-3B-Instruct-Turbo"
supports_tools = true

[providers.fireworks]
kind = "openai_compat"
auth = "api_key"
api_key_env = "FIREWORKS_API_KEY"
base_url = "https://api.fireworks.ai/inference/v1"
model = "accounts/fireworks/models/llama-v3p3-70b-instruct"
small_model = "accounts/fireworks/models/llama-v3p2-3b-instruct"
supports_tools = true

[providers.moonshot]
kind = "openai_compat"
auth = "api_key"
api_key_env = "MOONSHOT_API_KEY"
base_url = "https://api.moonshot.ai/v1"
model = "kimi-k2.5"
small_model = "moonshot-v1-auto"
supports_tools = true

[providers.glm]
kind = "openai_compat"
auth = "api_key"
api_key_env = "ZAI_API_KEY"
base_url = "https://open.bigmodel.cn/api/paas/v4"
model = "glm-4.5"
small_model = "glm-4-flash"
supports_tools = true

[providers.perplexity]
kind = "openai_compat"
auth = "api_key"
api_key_env = "PERPLEXITY_API_KEY"
base_url = "https://api.perplexity.ai"
model = "sonar-pro"
small_model = "sonar"
supports_tools = false

[providers.cohere]
kind = "openai_compat"
auth = "api_key"
api_key_env = "COHERE_API_KEY"
base_url = "https://api.cohere.ai/compatibility/v1"
model = "command-r-plus"
small_model = "command-r"
supports_tools = true

[providers.qwen]
kind = "openai_compat"
auth = "api_key"
api_key_env = "DASHSCOPE_API_KEY"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
model = "qwen-max"
small_model = "qwen-turbo"
supports_tools = true

[providers.local]
kind = "openai_compat"
auth = "none"
base_url = "http://localhost:11434/v1"   # Ollama
model = "qwen3:8b"
api_key_env = ""
supports_tools = false

[harness]
simple   = "quick"
medium   = "worker"
advanced = "thinker"
dev      = "coder"
fallback = ["anthropic", "openai", "gemini", "opencode_zen", "opencode_go", "local"]
selection = "auto"             # auto | manual  (수동이면 아래 모델 ID)
# manual_design = ""           # 설계·구성 (advanced)
# manual_verify = ""           # 검증
# manual_debug = ""            # 디버깅 (dev)
# manual_model = ""            # 전역 수동 모델 (provider:model)


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
db_path = "~/.rafikx/data.db"
context_limit_chars = 12000
tokenizer = "unicode61"

[telegram]
enabled = true
token_env = "TELEGRAM_BOT_TOKEN"
allowed_user_ids = [123456789]
allow_agent = false
approval_timeout_secs = 300

[ui]
theme = "rafikx"                   # rafikx | opal | synth
appearance = "auto"                # 데스크탑: light | dark | auto (시간대 자동)
"#;

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigFile {
    pub general: GeneralConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub harness: HarnessConfig,
    pub subagents: HashMap<String, SubAgentConfig>,
    pub memory: MemoryConfig,
    pub inspector: InspectorConfig,
    pub obsidian: ObsidianConfig,
    pub telegram: TelegramConfig,
    /// 옛 설정에 없으면 기본값 (테마 rafikx).
    #[serde(default)]
    pub ui: UiConfig,
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
    /// 첫 실행 마법사를 이미 지났으면 true. 옛 설정에는 없음 → false.
    #[serde(default)]
    pub setup_done: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub kind: String,
    #[serde(default)]
    pub auth: String,
    #[serde(default)]
    pub api_key_env: String,
    pub model: String,
    #[serde(default)]
    pub small_model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub supports_tools: bool,
    /// true면 이 프로바이더 기본은 「자동 (하네스)」.
    #[serde(default)]
    pub model_auto: bool,
    /// 있으면 팩커가 이 값을 우선한다.
    #[serde(default)]
    pub context_window: Option<u32>,
    /// false 면 로그인은 유지하고 호출만 막는다. 옛 설정에 없으면 사용 중.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

fn default_selection() -> String {
    "auto".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct HarnessConfig {
    pub simple: String,
    pub medium: String,
    pub advanced: String,
    pub dev: String,
    pub fallback: Vec<String>,
    #[serde(default = "default_selection")]
    pub selection: String,
    /// 수동 모드에서 각 분류가 쓸 모델 ("provider:model" 또는 모델 ID). 빈 값이면 자동.
    #[serde(default)]
    pub manual_simple: Option<String>,
    #[serde(default)]
    pub manual_medium: Option<String>,
    #[serde(default)]
    pub manual_design: Option<String>,
    #[serde(default)]
    pub manual_verify: Option<String>,
    #[serde(default)]
    pub manual_debug: Option<String>,
    #[serde(default)]
    pub manual_model: Option<String>,
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
pub struct InspectorConfig {
    pub subagent: String,
    #[allow(dead_code)]
    pub auto_interval_hours: f64,
    #[allow(dead_code)]
    pub notify_telegram: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ObsidianConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub vault_path: String,
    pub db_path: String,
    pub context_limit_chars: u32,
    pub tokenizer: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub token_env: String,
    pub allowed_user_ids: Vec<i64>,
    pub allow_agent: bool,
    pub approval_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 데스크탑 화면 배경: light | dark | auto (시간대 자동).
    #[serde(default = "default_appearance")]
    pub appearance: String,
}

fn default_theme() -> String {
    "rafikx".into()
}

fn default_appearance() -> String {
    "auto".into()
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            appearance: default_appearance(),
        }
    }
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
        if let Some(p) = env_nonempty("RAFIKX_HOME") {
            return Ok(expand_tilde(&p));
        }
        if let Some(p) = env_nonempty("AGENT_HARNESS_HOME") {
            return Ok(expand_tilde(&p));
        }
        let home = dirs::home_dir().ok_or_else(|| anyhow!("홈 폴더를 찾을 수 없습니다"))?;
        Ok(resolve_default_data_dir(&home))
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
        let raw = merge_missing_providers(&path, &raw)?;
        crate::auth::ensure_secrets_template();
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub fn api_key_tail(&self, provider_name: &str) -> Result<Option<String>> {
        Ok(self.api_key(provider_name)?.map(|k| {
            let tail: String = k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            tail
        }))
    }

    pub fn reload(&self) -> Result<Self> {
        Self::load(Some(&self.path))
    }
}

/// `header` 예: `[harness]`, `[providers.anthropic]`, `[telegram]`.
/// `formatted` 는 이미 TOML 값 (`"auto"`, `true`, `[1, 2]`).
pub fn upsert_toml_key(raw: &str, header: &str, key: &str, formatted: &str) -> String {
    let nl = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let lines: Vec<&str> = raw.split('\n').collect();
    let header_trim = header.trim();
    let mut start = None;
    for (i, line) in lines.iter().enumerate() {
        if line.trim() == header_trim || line.trim().trim_end_matches('\r') == header_trim {
            start = Some(i);
            break;
        }
    }
    let mut out: Vec<String> = Vec::new();
    if let Some(s) = start {
        let mut end = lines.len();
        for (i, line) in lines.iter().enumerate().skip(s + 1) {
            let t = line.trim().trim_end_matches('\r');
            if t.starts_with('[') && t.ends_with(']') {
                end = i;
                break;
            }
        }
        let mut replaced = false;
        for (i, line) in lines.iter().enumerate() {
            let mut row = (*line).trim_end_matches('\r').to_string();
            if i > s && i < end && !replaced {
                let t = row.trim();
                if let Some(rest) = t.strip_prefix(key) {
                    let rest = rest.trim_start();
                    if rest.starts_with('=') {
                        let indent = &row[..row.len() - row.trim_start().len()];
                        row = format!("{indent}{key} = {formatted}");
                        replaced = true;
                    }
                }
            }
            out.push(row);
            if i + 1 == end && !replaced {
                out.push(format!("{key} = {formatted}"));
                replaced = true;
            }
        }
        if !replaced {
            out.push(format!("{key} = {formatted}"));
        }
    } else {
        for line in &lines {
            out.push((*line).trim_end_matches('\r').to_string());
        }
        if !out.last().map(|s| s.is_empty()).unwrap_or(true) {
            out.push(String::new());
        }
        out.push(header_trim.to_string());
        out.push(format!("{key} = {formatted}"));
    }
    let mut s = out.join(nl);
    if !s.ends_with('\n') {
        s.push_str(nl);
    }
    s
}

pub fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn write_toml_key(path: &Path, header: &str, key: &str, formatted: &str) -> Result<()> {
    let raw = fs::read_to_string(path).with_context(|| format!("{} 파일을 읽을 수 없습니다", path.display()))?;
    let new_raw = upsert_toml_key(&raw, header, key, formatted);
    fs::write(path, new_raw)?;
    set_owner_only_mode(path);
    Ok(())
}

pub fn append_toml(path: &Path, block: &str) -> Result<()> {
    let mut raw = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("{} 파일을 읽을 수 없습니다", path.display()))?
    } else {
        String::new()
    };
    if !raw.is_empty() && !raw.ends_with('\n') {
        raw.push('\n');
    }
    raw.push_str(block);
    if !raw.ends_with('\n') {
        raw.push('\n');
    }
    fs::write(path, raw)?;
    set_owner_only_mode(path);
    Ok(())
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `.rafikx`가 있으면 사용. 없고 예전 `.agent-harness`만 있으면 그걸 사용. 둘 다 없으면 새 설치용 `.rafikx`.
pub(crate) fn resolve_default_data_dir(home: &Path) -> PathBuf {
    let new_dir = home.join(".rafikx");
    let old_dir = home.join(".agent-harness");
    if new_dir.exists() {
        new_dir
    } else if old_dir.exists() {
        old_dir
    } else {
        new_dir
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

pub fn set_owner_only_mode(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    let _ = path;
}

fn merge_missing_providers(path: &Path, raw: &str) -> Result<String> {
    let names = [
        "openai",
        "gemini",
        "grok",
        "openrouter",
        "opencode_zen",
        "opencode_go",
        "groq",
        "deepseek",
        "mistral",
        "together",
        "fireworks",
        "moonshot",
        "glm",
        "perplexity",
        "cohere",
        "qwen",
    ];
    let mut extra = String::new();
    for name in names {
        let header = format!("[providers.{name}]");
        if raw.contains(&header) {
            continue;
        }
        if let Some(block) = extract_table(DEFAULT_CONFIG, &header) {
            extra.push('\n');
            extra.push_str(&block);
            extra.push('\n');
        }
    }
    if extra.is_empty() {
        return Ok(raw.to_string());
    }
    let new_raw = format!(
        "{}\n\n# --- 자동 추가된 프로바이더 (키는 secrets.toml 또는 OAuth) ---\n{}",
        raw.trim_end(),
        extra
    );
    fs::write(path, &new_raw)?;
    Ok(new_raw)
}

fn extract_table(src: &str, header: &str) -> Option<String> {
    let start = src.find(header)?;
    let rest = &src[start..];
    let end = rest[header.len()..]
        .find("\n[")
        .map(|i| header.len() + i)
        .unwrap_or(rest.len());
    Some(rest[..end].trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_prefers_rafikx_then_legacy() {
        let tmp = std::env::temp_dir().join(format!(
            "rafikx-dir-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        assert_eq!(resolve_default_data_dir(&tmp), tmp.join(".rafikx"));

        fs::create_dir_all(tmp.join(".agent-harness")).unwrap();
        assert_eq!(resolve_default_data_dir(&tmp), tmp.join(".agent-harness"));

        fs::create_dir_all(tmp.join(".rafikx")).unwrap();
        assert_eq!(resolve_default_data_dir(&tmp), tmp.join(".rafikx"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn old_harness_without_selection_defaults_auto() {
        let raw = DEFAULT_CONFIG.replace("selection = \"auto\"             # auto | manual  (수동이면 아래 모델 ID)\n", "");
        let file: ConfigFile = toml::from_str(&raw).expect("parse");
        assert_eq!(file.harness.selection, "auto");
        assert!(!file.general.setup_done);
        assert!(file.obsidian.enabled);
    }

    #[test]
    fn default_config_includes_opencode_zen_and_go() {
        assert!(DEFAULT_CONFIG.contains("[providers.opencode_zen]"));
        assert!(DEFAULT_CONFIG.contains("https://opencode.ai/zen/v1"));
        assert!(DEFAULT_CONFIG.contains("[providers.opencode_go]"));
        assert!(DEFAULT_CONFIG.contains("https://opencode.ai/zen/go/v1"));
        let file: ConfigFile = toml::from_str(DEFAULT_CONFIG).expect("parse");
        let zen = file.providers.get("opencode_zen").expect("zen");
        assert_eq!(zen.api_key_env, "OPENCODE_API_KEY");
        assert_eq!(zen.kind, "openai_compat");
        let go = file.providers.get("opencode_go").expect("go");
        assert_eq!(go.api_key_env, "OPENCODE_GO_API_KEY");
        assert_eq!(go.base_url.as_deref(), Some("https://opencode.ai/zen/go/v1"));
    }

    #[test]
    fn upsert_inserts_and_replaces() {
        let raw = "[harness]\nsimple = \"quick\"\n\n[telegram]\nenabled = true\n";
        let a = upsert_toml_key(raw, "[harness]", "selection", "\"auto\"");
        assert!(a.contains("selection = \"auto\""));
        let b = upsert_toml_key(&a, "[harness]", "selection", "\"manual\"");
        assert!(b.contains("selection = \"manual\""));
        assert_eq!(b.matches("selection =").count(), 1);
    }
}
