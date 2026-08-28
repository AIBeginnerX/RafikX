//! OMO 설치 마법사 스타일의 터미널 모델 관리 — 선택·등록·수정 원스톱 플로우.
//! rafikx model (마법사) · rafikx model list · rafikx model use <n|provider:model>

use std::io::{self, Write};

use anyhow::{Result, anyhow};

use crate::config::Config;
use crate::spinner::Spinner;

#[derive(Debug, Clone)]
pub struct ProviderRow {
    pub id: String,
    pub label: String,
    pub connected: bool,
    pub enabled: bool,
    pub is_default: bool,
    pub model: String,
}

pub fn rows(cfg: &Config) -> Vec<ProviderRow> {
    let names = crate::auth::menu_provider_names(cfg);
    let mut out: Vec<ProviderRow> = Vec::new();
    for n in &names {
        let Ok(p) = cfg.provider(n) else { continue };
        out.push(ProviderRow {
            id: n.clone(),
            label: crate::auth::provider_label(n),
            connected: crate::auth::is_connected(cfg, n),
            enabled: crate::auth::is_enabled(cfg, n),
            is_default: cfg.file.general.default_provider.eq_ignore_ascii_case(n),
            model: p.model.clone(),
        });
    }
    // 기본 연결을 맨 앞으로 — 마법사 첫 화면이 곧 현재 상태가 되도록.
    out.sort_by_key(|r| (!r.is_default, r.id.clone()));
    out
}

fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

/// "provider:model" | "provider" | 번호 문자열 해석. URL 등은 쌍으로 오해하지 않는다.
pub fn parse_model_arg(s: &str) -> (Option<String>, String) {
    let ident = |x: &str| {
        !x.is_empty()
            && x.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    };
    let t = s.trim();
    if let Some((p, m)) = t.split_once(':')
        && ident(p)
        && ident(m)
    {
        return (Some(p.to_string()), m.to_string());
    }
    (None, t.to_string())
}

pub async fn cmd_list(cfg: &Config) -> Result<()> {
    let rs = rows(cfg);
    println!("연결 · 모델 현황");
    for (i, r) in rs.iter().enumerate() {
        let mark = if r.is_default { "*" } else { " " };
        let state = if !r.enabled {
            "사용중지"
        } else if r.connected {
            "연결됨"
        } else {
            "미연결"
        };
        println!(
            "{mark}[{:>2}] {:<18} {:<6} model={}",
            i + 1,
            r.label,
            state,
            r.model
        );
    }
    println!();
    println!("* = 기본 연결 · 변경: rafikx model use <번호|provider:model>");
    println!("Harness 분류별 지정: rafikx harness <simple|medium|advanced|dev> <provider:model>");
    Ok(())
}

async fn fetch_models(cfg: &Config, name: &str) -> Vec<String> {
    let sp = Spinner::start(format!("{name} 모델 목록 조회…").as_str());
    let remote = crate::auth::list_remote_models(cfg, name).await.ok();
    sp.finish();
    let mut list = remote.unwrap_or_default();
    if !list.is_empty() {
        // 원격 목록을 캐시에 저장 — /model 피커·Harness 선택이 계속 볼 수 있게.
        let _ = crate::auth::save_catalog(cfg, name, &list);
    } else {
        crate::ui::note("원격 목록 unavailable — 등록된 선호 카탈로그 사용");
        list = crate::auth::catalog_models(cfg, name);
    }
    for m in crate::auth::catalog_models(cfg, name) {
        if !list.iter().any(|x| x == &m) {
            list.push(m);
        }
    }
    list
}

/// 기본 연결·모델을 저장하고 ping 으로 검증한다.
pub async fn apply_default(cfg_path_based: &Config, name: &str, model: &str) -> Result<String> {
    crate::accounts_ui::write_provider_model(cfg_path_based, name, model)?;
    crate::accounts_ui::set_default_provider(cfg_path_based, name)?;
    let cfg = cfg_path_based.reload()?;
    let sp = Spinner::start(format!("{name} 연결 검증…").as_str());
    let ping = crate::harness::ping_provider(&cfg, name).await;
    sp.finish();
    let mut msg = format!("기본 설정: {name} / {model}\n{ping}");
    if ping.contains("실패") || ping.contains("400") || ping.contains("401") {
        msg.push_str("\n⚠ 도구 실행 경로(chat/completions)가 이 키를 거부할 수 있습니다. 키를 다시 확인하세요.");
    }
    Ok(msg)
}

pub async fn cmd_use(cfg: &Config, arg: &str) -> Result<()> {
    let rs = rows(cfg);
    let (prov_opt, model_opt) = parse_model_arg(arg);
    let name = match prov_opt {
        Some(p) => p,
        None => {
            // 숫자 또는 프로바이더 이름
            let idx = arg.trim().parse::<usize>().ok();
            match idx {
                Some(n) if n >= 1 && n <= rs.len() => rs[n - 1].id.clone(),
                _ => {
                    let hit = rs.iter().find(|r| {
                        r.id.eq_ignore_ascii_case(arg.trim())
                            || r.label.to_lowercase().contains(&arg.trim().to_lowercase())
                    });
                    hit.map(|r| r.id.clone()).ok_or_else(|| {
                        anyhow!("'{arg}' 를 서비스로 해석하지 못했습니다. rafikx model list")
                    })?
                }
            }
        }
    };

    ensure_connected(cfg, &name)?;

    let model = if model_opt.is_empty() {
        cfg.provider(&name)?.model.clone()
    } else {
        model_opt
    };
    let msg = apply_default(cfg, &name, &model).await?;
    println!("{msg}");
    println!();
    println!("Harness 반영: selection=auto 면 즉시 적용됩니다. rafikx harness 로 확인.");
    Ok(())
}

fn ensure_connected(cfg: &Config, name: &str) -> Result<()> {
    if !cfg.file.providers.contains_key(name) {
        return Err(anyhow!("'{name}' 은(는) config에 없는 서비스입니다"));
    }
    let p = cfg.provider(name)?;
    if !crate::auth::is_enabled(cfg, name) {
        return Err(anyhow!("'{name}' 은(는) 사용 중지 상태입니다"));
    }
    let mode = crate::auth::auth_mode(name, p);
    if mode != "none" && !crate::auth::is_connected(cfg, name) {
        return Err(anyhow!(
            "'{name}' 키가 없습니다. 먼저 rafikx login 에서 연결하세요."
        ));
    }
    Ok(())
}

/// 인터랙티브 마법사 — OMO 설치 인터뷰의 축소판.
/// ① 서비스 선택(키 없으면 즉시 등록) ② 원격 모델 조회 ③ 번호 선택 ④ 저장·검증.
pub async fn run_wizard(cfg: Config) -> Result<()> {
    println!("{}", crate::ui::gold("── 모델 설정 마법사 ──"));
    let rs = rows(&cfg);
    for (i, r) in rs.iter().enumerate() {
        let mark = if r.is_default { "*" } else { " " };
        let state = if r.connected {
            "연결됨"
        } else {
            "미연결"
        };
        println!("{mark}[{:>2}] {:<18} {state}", i + 1, r.label);
    }
    let ans = read_line("서비스 번호 (Enter=취소): ")?;
    if ans.is_empty() {
        return Ok(());
    }
    let n: usize = ans
        .parse()
        .map_err(|_| anyhow!("번호를 입력하세요 (예: 1)"))?;
    let row = rs
        .get(n - 1)
        .ok_or_else(|| anyhow!("범위를 벗어난 번호입니다"))?
        .clone();

    ensure_connected(&cfg, &row.id)?;
    if !row.connected {
        // 키 즉시 등록 (등록형 플로우)
        let key = read_line(&format!("{} API 키 붙여넣기: ", row.label))?;
        if key.trim().is_empty() {
            return Err(anyhow!("키가 비어 있습니다"));
        }
        let sp = Spinner::start("키 저장…");
        let saved = crate::auth::save_pasted_key(&row.id, key.trim());
        sp.finish();
        saved?;
        println!("{} 키 저장 완료", row.label);
    }

    // 원격 모델 조회 (스피너)
    let models = fetch_models(&cfg, &row.id).await;
    if models.is_empty() {
        return Err(anyhow!("모델 목록이 비어 있습니다"));
    }
    println!("{} 사용 가능한 모델 {}개:", row.label, models.len());
    for (i, m) in models.iter().enumerate().take(40) {
        println!("  [{:>2}] {}", i + 1, m);
    }
    if models.len() > 40 {
        println!("  … 외 {}개", models.len() - 40);
    }
    let ans = read_line("모델 번호 또는 모델 ID 직접 입력: ")?;
    if ans.is_empty() {
        return Err(anyhow!("취소되었습니다"));
    }
    let model = match ans.parse::<usize>() {
        Ok(i) if (1..=models.len()).contains(&i) => models[i - 1].clone(),
        _ => ans.clone(),
    };

    let cfg2 = cfg.reload()?;
    let msg = apply_default(&cfg2, &row.id, &model).await?;
    println!("{msg}");

    // 마침 후 Harness 요약 (자동 모드면 그대로 반영됨)
    let cfg3 = cfg2.reload()?;
    println!();
    println!("Harness 바인딩:");
    for c in [
        crate::harness::TaskClass::Simple,
        crate::harness::TaskClass::Medium,
        crate::harness::TaskClass::Advanced,
        crate::harness::TaskClass::Dev,
    ] {
        match crate::harness::bind(&cfg3, c, None, None) {
            Ok(b) => println!("  {:8} → {}/{}", b.class.as_str(), b.provider_name, b.model),
            Err(e) => println!("  {:8} → ({e:#})", c.as_str()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_model_pairs() {
        let (p, m) = parse_model_arg("openai:gpt-5.6");
        assert_eq!(p.as_deref(), Some("openai"));
        assert_eq!(m, "gpt-5.6");

        let (p, _m) = parse_model_arg("opencode_zen:x-preview-f-free");
        assert_eq!(p.as_deref(), Some("opencode_zen"));

        let (p, m) = parse_model_arg("gpt-5.6");
        assert!(p.is_none());
        assert_eq!(m, "gpt-5.6");

        // URL 같은 입력은 provider:model 로 오해하지 않는다
        let (p, _) = parse_model_arg("https://x.ai/v1:model");
        assert!(p.is_none());
    }
}
