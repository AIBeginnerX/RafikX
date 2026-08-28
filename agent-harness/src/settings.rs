use anyhow::Result;

use crate::auth;
use crate::config::{self, Config};
use crate::menu;
use crate::ranks;

pub async fn maybe_first_run(cfg: Config, interactive: bool) -> Result<Config> {
    if !interactive {
        return Ok(cfg);
    }
    if cfg.file.general.setup_done || auth::has_cloud_credential(&cfg) {
        // 순위표 갱신은 main 의 spawn_weekly_refresh 가 백그라운드로 처리한다 —
        // 시작 경로에서 최대 30초 동기 네트워크 대기를 하지 않는다.
        return Ok(cfg);
    }
    crate::ui::banner("처음 설정");
    crate::ui::note("서비스 하나를 고르고 키를 붙이거나 로그인하면 대화가 열립니다.");
    crate::ui::note("키는 secrets.toml 또는 환경변수. config.toml 에는 적지 마세요.");
    let cfg = run_quick_setup(cfg).await?;
    mark_setup_done(&cfg)?;
    cfg.reload()
}

pub async fn cmd_login(cfg: Config) -> Result<()> {
    crate::ui::banner("연결");
    crate::ui::note("키는 secrets.toml / 환경변수. 설정 파일 경로: ~/.rafikx/config.toml");
    let _ = run_quick_setup(cfg).await?;
    Ok(())
}

pub async fn cmd_settings(mut cfg: Config) -> Result<()> {
    crate::ui::banner("설정");
    crate::ui::note(&format!("설정 파일  {}", cfg.path.display()));
    for note in auth::maintain_accounts(&cfg)? {
        crate::ui::note(&note);
    }
    cfg = cfg.reload()?;
    ranks::maybe_refresh_quiet().await;
    admin_loop(&mut cfg).await
}

pub async fn cmd_doctor_interactive(mut cfg: Config, already_ok_intro: bool) -> Result<()> {
    let connected = auth::has_cloud_credential(&cfg);
    if !connected {
        println!();
        println!("아직 연결된 클라우드 서비스가 없습니다. 하나를 고르면 됩니다.");
        cfg = run_quick_setup(cfg).await?;
        mark_setup_done(&cfg)?;
        cfg = cfg.reload()?;
    }
    if already_ok_intro {
        println!();
    }
    admin_loop(&mut cfg).await
}

async fn admin_loop(cfg: &mut Config) -> Result<()> {
    loop {
        *cfg = cfg.reload()?;
        crate::ui::print_footer();
        let items = vec![
            "서비스·계정 연결/추가/해제".into(),
            "모델 (등록된 것만)".into(),
            "Harness 모드 (자동/수동)".into(),
            "텔레그램".into(),
            "옵시디언".into(),
            "모델 순위 보기·갱신".into(),
        ];
        let connected = auth::connected_names(cfg);
        let extra = format!(
            "연결: {}  |  Harness: {}  |  {}",
            if connected.is_empty() {
                "없음".into()
            } else {
                connected
                    .iter()
                    .map(|n| auth::provider_label(n))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            if cfg.file.harness.selection.eq_ignore_ascii_case("manual") {
                "수동"
            } else {
                "자동"
            },
            ranks::status_line()
        );
        let choice = menu::prompt_choice("설정 메뉴", &items, false, &extra)?;
        match choice.first().copied().unwrap_or(0) {
            0 => return Ok(()),
            1 => menu_providers(cfg).await?,
            2 => menu_models(cfg).await?,
            3 => menu_harness(cfg)?,
            4 => menu_telegram(cfg)?,
            5 => menu_obsidian(cfg)?,
            6 => menu_ranks().await?,
            _ => {}
        }
    }
}

async fn run_quick_setup(mut cfg: Config) -> Result<Config> {
    loop {
        cfg = cfg.reload()?;
        let featured = [
            "opencode_zen",
            "opencode_go",
            "anthropic",
            "openai",
            "openrouter",
            "local",
        ];
        let names: Vec<String> = featured
            .iter()
            .filter(|n| cfg.file.providers.contains_key(**n))
            .map(|s| s.to_string())
            .collect();
        let mut items: Vec<String> = names
            .iter()
            .map(|n| format_provider_choice(&cfg, n))
            .collect();
        items.push("다른 서비스 모두 보기".into());
        let choice = menu::prompt_choice(
            "연결할 서비스",
            &items,
            false,
            "이름도 됩니다 (zen, go, anthropic). 키: OPENCODE_API_KEY / OPENCODE_GO_API_KEY",
        )?;
        let n = choice.first().copied().unwrap_or(0);
        if n == 0 {
            break;
        }
        let picked = if n == items.len() {
            pick_any_provider(&cfg)?
        } else {
            names.get(n - 1).cloned()
        };
        let Some(name) = picked else { continue };
        if auth::is_connected(&cfg, &name) && auth::auth_mode(&name, cfg.provider(&name)?) != "none"
        {
            provider_card(&mut cfg, &name).await?;
        } else {
            match auth::connect_provider(&cfg, &name).await {
                Ok(()) => {
                    cfg = cfg.reload()?;
                    pick_provider_model(&cfg, &name).await?;
                    config::write_toml_key(
                        &cfg.path,
                        "[general]",
                        "default_provider",
                        &config::toml_string(&name),
                    )?;
                    println!("기본 서비스: {}", auth::provider_label(&name));
                    cfg = cfg.reload()?;
                }
                Err(e) => println!("연결 실패: {e:#}"),
            }
        }
        println!();
        let more = menu::prompt_choice(
            "다른 서비스도 연결할까요?",
            &["하나 더 연결".into()],
            false,
            "0 이면 끝. 대화 화면으로 갑니다.",
        )?;
        if more.first() != Some(&1) {
            break;
        }
    }
    cfg.reload()
}

fn format_provider_choice(cfg: &Config, name: &str) -> String {
    let mark = if !auth::is_enabled(cfg, name) && auth::is_connected(cfg, name) {
        "중지됨"
    } else if auth::is_connected(cfg, name) {
        "연결됨"
    } else {
        "미연결"
    };
    let hint = auth::env_hint(cfg, name);
    if hint.is_empty() {
        format!("{}  [{mark}]", auth::provider_label(name))
    } else {
        format!("{}  [{mark}]  {hint}", auth::provider_label(name))
    }
}

fn pick_any_provider(cfg: &Config) -> Result<Option<String>> {
    let names = auth::menu_provider_names(cfg);
    let items: Vec<String> = names
        .iter()
        .map(|n| format_provider_choice(cfg, n))
        .collect();
    let choice = menu::prompt_choice("서비스를 고르세요", &items, false, "이름 또는 번호")?;
    let n = choice.first().copied().unwrap_or(0);
    if n == 0 {
        return Ok(None);
    }
    Ok(names.get(n - 1).cloned())
}

async fn menu_providers(cfg: &mut Config) -> Result<()> {
    loop {
        *cfg = cfg.reload()?;
        crate::ui::print_footer();
        let names = auth::menu_provider_names(cfg);
        let mut items: Vec<String> = names
            .iter()
            .map(|n| crate::accounts_ui::manage_row(cfg, n))
            .collect();
        items.push("사용자 지정 OpenAI 호환 추가".into());
        items.push("같은 서비스에 계정 하나 더".into());
        items.push("계정 하나만 해제".into());
        items.push("사용 중지 / 다시 켜기".into());
        let extra = "★ 는 기본값. 서비스를 고르면 등록·수정·삭제.";
        let choice = menu::prompt_choice("서비스 목록", &items, false, extra)?;
        let n = choice.first().copied().unwrap_or(0);
        if n == 0 {
            return Ok(());
        }
        let extra_start = names.len();
        if n == extra_start + 1 {
            add_custom_provider(cfg)?;
        } else if n == extra_start + 2 {
            add_another_account(cfg).await?;
        } else if n == extra_start + 3 {
            disconnect_one_account(cfg)?;
        } else if n == extra_start + 4 {
            pause_menu(cfg)?;
        } else if let Some(name) = names.get(n - 1).cloned() {
            provider_card(cfg, &name).await?;
        }
    }
}

async fn provider_card(cfg: &mut Config, name: &str) -> Result<()> {
    *cfg = cfg.reload()?;
    let mut items = vec![
        "키 등록 / 다시 붙이기".into(),
        "기본값으로 설정".into(),
        "기본 모델 바꾸기".into(),
        "Base URL 바꾸기".into(),
        "연결 해제 (키 삭제)".into(),
    ];
    if auth::is_connected(cfg, name) {
        items.insert(1, "계정 하나 더".into());
    }
    let extra = format!(
        "{}  |  {}",
        crate::accounts_ui::manage_row(cfg, name),
        crate::accounts_ui::auth_console_url(name).unwrap_or("")
    );
    let choice = menu::prompt_choice(&auth::provider_label(name), &items, false, &extra)?;
    let n = choice.first().copied().unwrap_or(0);
    if n == 0 {
        return Ok(());
    }
    let mut idx = 1usize;
    let next = |i: &mut usize| {
        let v = *i;
        *i += 1;
        v
    };
    let i_key = next(&mut idx);
    let i_more = if auth::is_connected(cfg, name) {
        Some(next(&mut idx))
    } else {
        None
    };
    let i_def = next(&mut idx);
    let i_model = next(&mut idx);
    let i_url = next(&mut idx);
    let i_del = next(&mut idx);
    if n == i_key {
        match auth::connect_provider(cfg, name).await {
            Ok(()) => crate::ui::ok("저장했습니다."),
            Err(e) => crate::ui::fail(&format!("{e:#}")),
        }
        *cfg = cfg.reload()?;
    } else if Some(n) == i_more {
        match auth::add_account(cfg, name).await {
            Ok(()) => crate::ui::ok("계정을 추가했습니다."),
            Err(e) => crate::ui::fail(&format!("{e:#}")),
        }
        *cfg = cfg.reload()?;
    } else if n == i_def {
        crate::accounts_ui::set_default_provider(cfg, name)?;
        crate::ui::ok(&format!("기본 서비스: {}", auth::provider_label(name)));
        *cfg = cfg.reload()?;
    } else if n == i_model {
        pick_provider_model(cfg, name).await?;
        *cfg = cfg.reload()?;
    } else if n == i_url {
        let cur = cfg
            .provider(name)
            .ok()
            .and_then(|p| p.base_url.clone())
            .unwrap_or_default();
        println!("현재: {cur}");
        let url = menu::prompt_line("base URL › ")?;
        if !url.is_empty() {
            crate::accounts_ui::write_provider_base_url(cfg, name, &url)?;
            crate::ui::ok("base URL 을 저장했습니다.");
            *cfg = cfg.reload()?;
        }
    } else if n == i_del {
        let yes = menu::prompt_choice(
            "정말 연결을 해제할까요? 키가 삭제됩니다.",
            &["삭제".into()],
            false,
            "0 이면 취소",
        )?;
        if yes.first() == Some(&1) {
            auth::disconnect_provider(name)?;
            *cfg = cfg.reload()?;
        }
    }
    Ok(())
}

fn add_custom_provider(cfg: &mut Config) -> Result<()> {
    let name = menu::prompt_line("이름 (영문 소문자, 예: myproxy) › ")?;
    let url = menu::prompt_line("base URL (https://...) › ")?;
    let model = menu::prompt_line("기본 모델 ID › ")?;
    crate::accounts_ui::append_custom_openai(cfg, &name, &url, &model)?;
    *cfg = cfg.reload()?;
    crate::ui::ok(&format!("'{name}' 추가. 이제 키를 붙이세요."));
    let label = auth::provider_label(&name);
    let env = cfg
        .provider(&name)
        .map(|p| p.api_key_env.clone())
        .unwrap_or_default();
    if let Some(key) = menu::prompt_api_key_box(&label, None, &env)? {
        auth::replace_or_save_key(&name, &key)?;
        crate::ui::ok("키를 저장했습니다.");
        *cfg = cfg.reload()?;
    }
    Ok(())
}

async fn add_another_account(cfg: &mut Config) -> Result<()> {
    let names = auth::connected_names(cfg);
    if names.is_empty() {
        crate::ui::warn("먼저 서비스를 하나 연결하세요.");
        return Ok(());
    }
    let items: Vec<String> = names.iter().map(|n| auth::provider_label(n)).collect();
    let choice = menu::prompt_choice("계정을 더 넣을 서비스:", &items, false, "")?;
    let n = choice.first().copied().unwrap_or(0);
    if n == 0 {
        return Ok(());
    }
    if let Some(name) = names.get(n - 1) {
        match auth::add_account(cfg, name).await {
            Ok(()) => {
                crate::ui::ok("계정을 추가했습니다. 리밋이 나면 자동으로 이 계정으로 넘어갑니다.")
            }
            Err(e) => crate::ui::fail(&format!("{e:#}")),
        }
        *cfg = cfg.reload()?;
    }
    Ok(())
}

fn disconnect_one_account(cfg: &mut Config) -> Result<()> {
    let mut rows = Vec::new();
    for name in auth::connected_names(cfg) {
        for (a, live, tail) in auth::list_account_rows(cfg, &name) {
            let mark =
                if auth::account_has_stored_credential(cfg, &a.provider, &a.id).unwrap_or(false) {
                    if live { "연결" } else { "만료" }
                } else {
                    "미연결"
                };
            rows.push((
                a.id.clone(),
                format!("{}  [{mark} ****{tail}]", crate::accounts::display(&a)),
            ));
        }
    }
    if rows.is_empty() {
        crate::ui::note("해제할 계정이 없습니다.");
        return Ok(());
    }
    let items: Vec<String> = rows.iter().map(|(_, l)| l.clone()).collect();
    let choice = menu::prompt_choice("해제할 계정:", &items, true, "")?;
    if choice.first() == Some(&0) {
        return Ok(());
    }
    for n in choice {
        if let Some((id, _)) = rows.get(n.saturating_sub(1)) {
            auth::disconnect_account(id)?;
        }
    }
    *cfg = cfg.reload()?;
    Ok(())
}

fn pause_menu(cfg: &mut Config) -> Result<()> {
    let names = auth::connected_names(cfg);
    if names.is_empty() {
        crate::ui::warn("먼저 서비스를 하나 연결하세요.");
        return Ok(());
    }
    let items: Vec<String> = names
        .iter()
        .map(|n| {
            let mark = if auth::is_enabled(cfg, n) {
                "사용 중"
            } else {
                "중지됨"
            };
            format!("{}  [{mark}]", auth::provider_label(n))
        })
        .collect();
    let choice = menu::prompt_choice(
        "중지하거나 다시 켤 서비스:",
        &items,
        true,
        "중지하면 로그인은 유지되고 질문은 나가지 않습니다.",
    )?;
    if choice.first() == Some(&0) || choice.is_empty() {
        return Ok(());
    }
    for n in choice {
        if n == 0 {
            continue;
        }
        let Some(name) = names.get(n - 1) else {
            continue;
        };
        let next = !auth::is_enabled(cfg, name);
        auth::set_provider_enabled(cfg, name, next)?;
    }
    *cfg = cfg.reload()?;
    Ok(())
}

#[allow(dead_code)]
fn disconnect_menu(cfg: &mut Config) -> Result<()> {
    let names = auth::connected_names(cfg);
    let remote: Vec<String> = names
        .into_iter()
        .filter(|n| {
            cfg.provider(n)
                .map(|p| auth::auth_mode(n, p) != "none")
                .unwrap_or(true)
        })
        .collect();
    if remote.is_empty() {
        println!("해제할 클라우드 연결이 없습니다.");
        return Ok(());
    }
    let items: Vec<String> = remote.iter().map(|n| auth::provider_label(n)).collect();
    let choice = menu::prompt_choice("해제할 연결 (여러 개 가능):", &items, true, "")?;
    if choice.first() == Some(&0) {
        return Ok(());
    }
    for n in choice {
        if let Some(name) = remote.get(n.saturating_sub(1)) {
            auth::disconnect_provider(name)?;
        }
    }
    *cfg = cfg.reload()?;
    Ok(())
}

async fn pick_provider_model(cfg: &Config, name: &str) -> Result<()> {
    let mut models = auth::catalog_models(cfg, name);
    // 연결 직후 원격 목록을 한 번 받아 캐시에 저장한다 — TUI 키 등록과 같은 통로.
    // (여기서 저장해 두어야 /model 피커가 신규 모델을 계속 보여준다.)
    if let Ok(remote) = auth::list_remote_models(cfg, name).await
        && !remote.is_empty()
    {
        let _ = auth::save_catalog(cfg, name, &remote);
        for m in remote {
            if !models.iter().any(|x| x == &m) {
                models.push(m);
            }
        }
    }
    if models.len() > 24 {
        models.truncate(24);
    }
    let mut items = vec!["자동 (Harness)".to_string()];
    items.extend(models.iter().cloned());
    let extra = format!("'{name}' 기본 모델을 고르세요.");
    let choice = menu::prompt_choice("모델", &items, false, &extra)?;
    let n = choice.first().copied().unwrap_or(0);
    if n == 0 {
        return Ok(());
    }
    let header = format!("[providers.{name}]");
    if n == 1 {
        config::write_toml_key(&cfg.path, &header, "model_auto", "true")?;
        println!("기본: 자동 (Harness가 고릅니다).");
    } else if let Some(id) = models.get(n - 2) {
        config::write_toml_key(&cfg.path, &header, "model_auto", "false")?;
        config::write_toml_key(&cfg.path, &header, "model", &config::toml_string(id))?;
        println!("기본 모델: {id}");
    }
    Ok(())
}

async fn menu_models(cfg: &mut Config) -> Result<()> {
    *cfg = cfg.reload()?;
    let names = auth::menu_provider_names(cfg);
    if names.is_empty() {
        println!("등록된 서비스가 없습니다.");
        return Ok(());
    }
    let items: Vec<String> = names
        .iter()
        .map(|n| crate::accounts_ui::manage_row(cfg, n))
        .collect();
    let choice = menu::prompt_choice(
        "모델을 고를 서비스:",
        &items,
        false,
        "연결 전이어도 기본 모델을 바꿀 수 있습니다.",
    )?;
    let n = choice.first().copied().unwrap_or(0);
    if n == 0 {
        return Ok(());
    }
    if let Some(name) = names.get(n - 1) {
        pick_provider_model(cfg, name).await?;
        *cfg = cfg.reload()?;
    }
    Ok(())
}

fn menu_harness(cfg: &mut Config) -> Result<()> {
    let items = vec![
        "자동 (기본) — 난이도에 맞춰 등록 모델 중 고름".into(),
        "수동 — 역할별로 등록 모델 고르기".into(),
    ];
    let extra = format!(
        "현재: {}",
        if cfg.file.harness.selection.eq_ignore_ascii_case("manual") {
            "수동"
        } else {
            "자동"
        }
    );
    let choice = menu::prompt_choice("Harness 모드", &items, false, &extra)?;
    match choice.first().copied().unwrap_or(0) {
        0 => return Ok(()),
        1 => {
            config::write_toml_key(&cfg.path, "[harness]", "selection", "\"auto\"")?;
            println!("Harness: 자동");
        }
        2 => {
            config::write_toml_key(&cfg.path, "[harness]", "selection", "\"manual\"")?;
            pick_manual_roles(cfg)?;
        }
        _ => {}
    }
    *cfg = cfg.reload()?;
    Ok(())
}

fn registered_choices(cfg: &Config) -> Vec<(String, String)> {
    auth::registered_models(cfg)
        .into_iter()
        .map(|r| {
            (
                format!("{}:{}", r.provider, r.id),
                format!("{} / {}", r.provider, r.id),
            )
        })
        .collect()
}

fn pick_manual_roles(cfg: &Config) -> Result<()> {
    let pairs = registered_choices(cfg);
    if pairs.is_empty() {
        println!("등록된 모델이 없습니다. 먼저 서비스를 연결하세요.");
        return Ok(());
    }
    let labels: Vec<String> = pairs.iter().map(|(_, l)| l.clone()).collect();
    for (key, title) in [
        ("manual_design", "설계·구성 (advanced)"),
        ("manual_verify", "검증"),
        ("manual_debug", "디버깅 (dev)"),
        ("manual_model", "그 외/전역 (simple·medium 포함)"),
    ] {
        let choice = menu::prompt_choice(title, &labels, false, "0 = 이 역할은 자동에 맡김")?;
        let n = choice.first().copied().unwrap_or(0);
        if n == 0 {
            config::write_toml_key(&cfg.path, "[harness]", key, "\"\"")?;
            continue;
        }
        if let Some((spec, _)) = pairs.get(n - 1) {
            config::write_toml_key(&cfg.path, "[harness]", key, &config::toml_string(spec))?;
            println!("{title}: {spec}");
        }
    }
    Ok(())
}

fn menu_telegram(cfg: &mut Config) -> Result<()> {
    loop {
        *cfg = cfg.reload()?;
        let tg = &cfg.file.telegram;
        let token_ok = auth::telegram_token(cfg)?.is_some();
        let extra = format!(
            "enabled={}  allow_agent={}  토큰={}  allowlist={:?}",
            tg.enabled,
            tg.allow_agent,
            if token_ok { "있음" } else { "없음" },
            tg.allowed_user_ids
        );
        let items = vec![
            if tg.enabled {
                "끄기".into()
            } else {
                "켜기".into()
            },
            "봇 토큰 붙여넣기".into(),
            if tg.allow_agent {
                "원격 에이전트 끄기".into()
            } else {
                "원격 에이전트 켜기 (승인 버튼 필수)".into()
            },
            "허용 user id 붙여넣기".into(),
        ];
        let choice = menu::prompt_choice("텔레그램", &items, false, &extra)?;
        match choice.first().copied().unwrap_or(0) {
            0 => return Ok(()),
            1 => {
                let v = if cfg.file.telegram.enabled {
                    "false"
                } else {
                    "true"
                };
                config::write_toml_key(&cfg.path, "[telegram]", "enabled", v)?;
            }
            2 => {
                println!("봇 토큰은 채팅에 다시 붙이지 마세요. 비밀 파일에만 저장됩니다.");
                let key = menu::prompt_secret("토큰> ")?;
                if key.is_empty() {
                    println!("취소했습니다.");
                } else {
                    auth::store_secret("telegram", &key)?;
                    println!("저장했습니다. (마지막 4자만 표시하지 않습니다. doctor에서 확인)");
                }
            }
            3 => {
                let v = if cfg.file.telegram.allow_agent {
                    "false"
                } else {
                    "true"
                };
                config::write_toml_key(&cfg.path, "[telegram]", "allow_agent", v)?;
            }
            4 => {
                println!("텔레그램 @userinfobot 의 숫자 id. 여러 명이면 쉼표.");
                let line = menu::prompt_line("user id> ")?;
                if line.is_empty() {
                    println!("취소했습니다.");
                } else {
                    let ids: Vec<i64> = line
                        .split([',', ' '])
                        .filter_map(|s| s.trim().parse().ok())
                        .collect();
                    if ids.is_empty() {
                        println!("숫자를 찾지 못했습니다.");
                    } else {
                        let arr = format!(
                            "[{}]",
                            ids.iter()
                                .map(|i| i.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        config::write_toml_key(&cfg.path, "[telegram]", "allowed_user_ids", &arr)?;
                    }
                }
            }
            _ => {}
        }
    }
}

fn menu_obsidian(cfg: &mut Config) -> Result<()> {
    loop {
        *cfg = cfg.reload()?;
        let ob = &cfg.file.obsidian;
        let extra = format!("enabled={}  vault={}", ob.enabled, ob.vault_path);
        let items = vec![
            if ob.enabled {
                "끄기".into()
            } else {
                "켜기".into()
            },
            "Vault 경로 붙여넣기".into(),
        ];
        let choice = menu::prompt_choice("옵시디언", &items, false, &extra)?;
        match choice.first().copied().unwrap_or(0) {
            0 => return Ok(()),
            1 => {
                let v = if cfg.file.obsidian.enabled {
                    "false"
                } else {
                    "true"
                };
                config::write_toml_key(&cfg.path, "[obsidian]", "enabled", v)?;
            }
            2 => {
                let p = menu::prompt_line("경로> ")?;
                if p.is_empty() {
                    println!("취소했습니다.");
                } else {
                    config::write_toml_key(
                        &cfg.path,
                        "[obsidian]",
                        "vault_path",
                        &config::toml_string(&p),
                    )?;
                }
            }
            _ => {}
        }
    }
}

async fn menu_ranks() -> Result<()> {
    println!("{}", ranks::status_line());
    let items = vec!["지금 갱신".into()];
    let choice = menu::prompt_choice(
        "모델 순위",
        &items,
        false,
        "인터넷이 없으면 번들 표를 유지합니다.",
    )?;
    if choice.first() == Some(&1) {
        match ranks::refresh(true).await {
            Ok(msg) => println!("{msg}"),
            Err(_) => println!("순위는 번들 기준, 오프라인이면 로컬 유지"),
        }
    }
    Ok(())
}

fn mark_setup_done(cfg: &Config) -> Result<()> {
    config::write_toml_key(&cfg.path, "[general]", "setup_done", "true")?;
    Ok(())
}

pub async fn cmd_ranks_update() -> Result<()> {
    match ranks::refresh(true).await {
        Ok(msg) => {
            println!("{msg}");
            Ok(())
        }
        Err(_) => {
            println!("순위는 번들 기준, 오프라인이면 로컬 유지");
            Ok(())
        }
    }
}

pub fn cmd_ranks_status() {
    println!("{}", ranks::status_line());
    let t = ranks::load();
    println!("출처: {}", t.source);
    println!("상위 항목:");
    for e in t.models.iter().take(8) {
        println!(
            "  {}  score={}  {}",
            e.id_aliases.first().map(|s| s.as_str()).unwrap_or("?"),
            e.score,
            e.tier
        );
    }
}
