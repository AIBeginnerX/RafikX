use anyhow::{Result, anyhow};

use crate::auth;
use crate::config::{self, Config};

/// 클립보드·브래킷 붙여넣기에서 온 키를 저장용으로 정리한다.
/// 앞뒤 공백, 따옴표, CR/LF/탭을 제거한다. 키 원문은 로그에 쓰지 말 것.
pub fn sanitize_pasted_key(raw: &str) -> String {
    let mut s = raw.replace(['\r', '\n', '\t'], "");
    s = s.trim().to_string();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s = s[1..s.len() - 1].trim().to_string();
    }
    s
}

/// UI용 마스크. 원문이 결과에 포함되지 않는다.
pub fn mask_secret(raw: &str) -> String {
    let n = raw.chars().count();
    if n == 0 {
        return String::new();
    }
    "•".repeat(n.min(64))
}

pub fn auth_console_url(name: &str) -> Option<&'static str> {
    match name {
        "opencode_zen" | "opencode_go" => Some("https://opencode.ai/auth"),
        "anthropic" => Some("https://console.anthropic.com/settings/keys"),
        "openai" => Some("https://platform.openai.com/api-keys"),
        "gemini" => Some("https://aistudio.google.com/apikey"),
        "grok" => Some("https://console.x.ai"),
        "openrouter" => Some("https://openrouter.ai/keys"),
        "groq" => Some("https://console.groq.com/keys"),
        "deepseek" => Some("https://platform.deepseek.com/api_keys"),
        "mistral" => Some("https://console.mistral.ai/api-keys"),
        "together" => Some("https://api.together.xyz/settings/api-keys"),
        "fireworks" => Some("https://fireworks.ai/account/api-keys"),
        "moonshot" => Some("https://platform.moonshot.ai"),
        "glm" => Some("https://open.bigmodel.cn/usercenter/apikeys"),
        "perplexity" => Some("https://www.perplexity.ai/settings/api"),
        "cohere" => Some("https://dashboard.cohere.com/api-keys"),
        "qwen" => Some("https://bailian.console.aliyun.com"),
        _ => None,
    }
}

pub fn set_default_provider(cfg: &Config, name: &str) -> Result<()> {
    if !cfg.file.providers.contains_key(name) {
        return Err(anyhow!("프로바이더 '{name}' 이(가) config에 없습니다"));
    }
    config::write_toml_key(
        &cfg.path,
        "[general]",
        "default_provider",
        &config::toml_string(name),
    )
}

pub fn write_provider_model(cfg: &Config, name: &str, model: &str) -> Result<()> {
    let header = format!("[providers.{name}]");
    config::write_toml_key(&cfg.path, &header, "model_auto", "false")?;
    config::write_toml_key(&cfg.path, &header, "model", &config::toml_string(model))
}

pub fn write_provider_base_url(cfg: &Config, name: &str, url: &str) -> Result<()> {
    let header = format!("[providers.{name}]");
    config::write_toml_key(&cfg.path, &header, "base_url", &config::toml_string(url))
}

pub fn valid_custom_id(name: &str) -> bool {
    let b = name.as_bytes();
    if b.is_empty() || b.len() > 32 {
        return false;
    }
    let first = b[0];
    if !first.is_ascii_lowercase() {
        return false;
    }
    b.iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

pub fn append_custom_openai(cfg: &Config, name: &str, base_url: &str, model: &str) -> Result<()> {
    if !valid_custom_id(name) {
        return Err(anyhow!(
            "이름은 영문 소문자로 시작하고 영문·숫자·밑줄만 (최대 32자)"
        ));
    }
    if cfg.file.providers.contains_key(name) {
        return Err(anyhow!("'{name}' 는 이미 있습니다"));
    }
    let base = base_url.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(anyhow!(
            "base URL 은 http:// 또는 https:// 로 시작해야 합니다"
        ));
    }
    let model = model.trim();
    if model.is_empty() {
        return Err(anyhow!("모델 ID가 비어 있습니다"));
    }
    let env = format!("RAFIKX_{}_API_KEY", name.to_ascii_uppercase());
    let block = format!(
        "\n[providers.{name}]\n\
         kind = \"openai_compat\"\n\
         auth = \"api_key\"\n\
         api_key_env = \"{env}\"\n\
         base_url = {base}\n\
         model = {model}\n\
         supports_tools = true\n",
        base = config::toml_string(base),
        model = config::toml_string(model),
    );
    config::append_toml(&cfg.path, &block)
}

pub fn manage_row(cfg: &Config, name: &str) -> String {
    let def = cfg.file.general.default_provider.eq_ignore_ascii_case(name);
    let star = if def { "★ " } else { "   " };
    let mark = if !auth::is_enabled(cfg, name) && auth::is_connected(cfg, name) {
        "중지됨"
    } else if auth::is_connected(cfg, name) {
        "연결됨"
    } else {
        "미연결"
    };
    let model = cfg.provider(name).map(|p| p.model.as_str()).unwrap_or("");
    format!("{star}{}  [{mark}]  {model}", auth::provider_label(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_quotes_and_newlines() {
        assert_eq!(sanitize_pasted_key("  sk-abc\r\n\t "), "sk-abc");
        assert_eq!(sanitize_pasted_key("\"sk-quoted\""), "sk-quoted");
        assert_eq!(sanitize_pasted_key("'sk-q'"), "sk-q");
        let long = format!("sk-{}", "x".repeat(200));
        assert_eq!(sanitize_pasted_key(&format!("{long}\n")), long);
    }

    #[test]
    fn mask_never_contains_raw() {
        let key = "sk-secret-value-1234";
        let m = mask_secret(key);
        assert!(!m.contains("sk-"));
        assert!(!m.contains("secret"));
        assert_eq!(m.chars().count(), key.chars().count());
        assert!(mask_secret("").is_empty());
        assert_eq!(mask_secret(&"a".repeat(80)).chars().count(), 64);
    }

    #[test]
    fn custom_id_rules() {
        assert!(valid_custom_id("myproxy"));
        assert!(valid_custom_id("acme_v2"));
        assert!(!valid_custom_id("MyProxy"));
        assert!(!valid_custom_id("1bad"));
        assert!(!valid_custom_id("has-dash"));
        assert!(!valid_custom_id(""));
    }

    #[test]
    fn zen_auth_url() {
        assert_eq!(
            auth_console_url("opencode_zen"),
            Some("https://opencode.ai/auth")
        );
    }
}
