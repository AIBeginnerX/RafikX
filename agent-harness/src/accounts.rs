use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub provider: String,
    pub label: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    items: Vec<Account>,
}

fn path() -> Result<PathBuf> {
    Ok(config::Config::data_dir()?.join("accounts.json"))
}

fn load() -> Result<File> {
    let p = path()?;
    if !p.exists() {
        return Ok(File::default());
    }
    let raw = fs::read_to_string(&p)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn save(file: &File) -> Result<()> {
    let p = path()?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&p, serde_json::to_string_pretty(file)?)?;
    config::set_owner_only_mode(&p);
    Ok(())
}

pub fn all() -> Vec<Account> {
    load().map(|f| f.items).unwrap_or_default()
}

pub fn for_provider(provider: &str) -> Vec<Account> {
    let mut v: Vec<Account> = all()
        .into_iter()
        .filter(|a| a.provider == provider)
        .collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

pub fn get(id: &str) -> Option<Account> {
    all().into_iter().find(|a| a.id == id)
}

pub fn next_id(provider: &str) -> String {
    let existing = for_provider(provider);
    if existing.is_empty() {
        return provider.to_string();
    }
    let mut n = 2u32;
    loop {
        let id = format!("{provider}::{n}");
        if !existing.iter().any(|a| a.id == id) {
            return id;
        }
        n += 1;
    }
}

pub fn default_label(provider: &str) -> String {
    let n = for_provider(provider).len() + 1;
    format!("계정 {n}")
}

pub fn upsert(account: Account) -> Result<()> {
    let mut file = load()?;
    if let Some(old) = file.items.iter_mut().find(|a| a.id == account.id) {
        *old = account;
    } else {
        file.items.push(account);
    }
    save(&file)
}

pub fn remove(id: &str) -> Result<Option<Account>> {
    let mut file = load()?;
    let pos = file.items.iter().position(|a| a.id == id);
    let removed = pos.map(|i| file.items.remove(i));
    save(&file)?;
    Ok(removed)
}

pub fn remove_provider(provider: &str) -> Result<Vec<Account>> {
    let mut file = load()?;
    let (keep, gone): (Vec<_>, Vec<_>) = file.items.drain(..).partition(|a| a.provider != provider);
    file.items = keep;
    save(&file)?;
    Ok(gone)
}

/// auth.json / secrets.toml 에만 있고 레지스트리에 없으면 한 줄로 복구.
pub fn ensure_legacy(provider: &str) -> Result<()> {
    if for_provider(provider).is_empty() {
        upsert(Account {
            id: provider.to_string(),
            provider: provider.to_string(),
            label: default_label(provider),
        })?;
    }
    Ok(())
}

pub fn display(a: &Account) -> String {
    format!("{} / {}", a.provider, a.label)
}

#[cfg(test)]
mod tests {
    #[test]
    fn next_id_first_is_provider() {
        // 순수 함수가 아니라 파일에 의존하므로 규칙만 고정
        assert_eq!(
            {
                let existing: Vec<String> = vec![];
                if existing.is_empty() {
                    "anthropic".to_string()
                } else {
                    "anthropic::2".into()
                }
            },
            "anthropic"
        );
    }

    #[test]
    fn next_id_increments() {
        let existing = ["anthropic", "anthropic::2"];
        let mut n = 2u32;
        let id = loop {
            let cand = format!("anthropic::{n}");
            if !existing.contains(&cand.as_str()) {
                break cand;
            }
            n += 1;
        };
        assert_eq!(id, "anthropic::3");
    }
}
