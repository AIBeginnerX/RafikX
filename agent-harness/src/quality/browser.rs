//! 브라우저 스모크 게이트 (S4 보강) — HTML/JS 산출물의 런타임 오류를 실제 브라우저로 잡는다.
//! 기원: 2026-08-29 사용자 실측 — "슈퍼마리오 게임이 실행도 안 되는" 결과물.
//! game.js 의 잔재 변수(camTarget)가 첫 프레임에서 ReferenceError 를 냈지만,
//! node --check(구문만)·eslint(미설치)·내장 보안 스캐너(보안 전용) 어디에도 걸리지 않았다.
//! "사용자가 결함을 발견하는 순간 = 게이트 설계의 실패" 원칙에 따라 추가된 게이트다.

/// 콘솔 로그(stderr)에서 런타임 오류를 추출한다 — 순수 함수(테스트 가능).
pub fn parse_console_errors(stderr: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        // Chrome CONSOLE 로그 형식: "...:INFO:CONSOLE(230)] \"Uncaught ReferenceError: ...\", source: ..."
        if !line.contains("CONSOLE") {
            continue;
        }
        let Some(msg_start) = line.find(']') else { continue };
        let msg = line[msg_start + 1..].trim();
        let is_error = msg.contains("Uncaught")
            || msg.contains("SyntaxError")
            || msg.contains("ReferenceError")
            || msg.contains("TypeError")
            || msg.contains("is not defined")
            || msg.contains("is not a function");
        if !is_error {
            continue;
        }
        // INFO/ERROR 수준 구분 없이 메시지만 남긴다.
        let reason: String = msg.chars().take(300).collect();
        if !out.contains(&reason) {
            out.push(reason);
        }
    }
    out
}

/// 설치된 브라우저 바이너리를 찾는다 — 없으면 None (게이트는 "스킵"으로 기록).
pub fn detect_browser() -> Option<&'static str> {
    const CANDIDATES: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium-browser",
        "/usr/bin/chromium",
    ];
    CANDIDATES
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .copied()
}

/// 엔트리 HTML 을 실제 브라우저로 로드해 콘솔 오류를 수집한다.
/// 브라우저가 없으면 Ok(None) — 스킵으로 기록한다(환경 제한).
pub async fn smoke_test(
    entry_html: &std::path::Path,
) -> anyhow::Result<Option<Vec<String>>> {
    let Some(browser) = detect_browser() else {
        return Ok(None);
    };
    let url = format!("file://{}", entry_html.canonicalize()?.display());
    // --no-sandbox 는 넣지 않는다 — 이 플래그가 있으면 콘솔 로그가 캡처되지
    // 않는다(실측). 컨테이너 root 등 필요한 환경은 RAFIKX_BROWSER_EXTRA_FLAGS 로
    // 추가 플래그를 넣는다 (공백 구분).
    let extra: Vec<String> = std::env::var("RAFIKX_BROWSER_EXTRA_FLAGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::process::Command::new(browser)
            .args([
                "--headless",
                "--disable-gpu",
                "--enable-logging=stderr",
                "--v=0",
                "--virtual-time-budget=5000",
                &url,
            ])
            .args(&extra)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await;
    let stderr = match out {
        Err(_) => anyhow::bail!("브라우저 스모크 시간 초과 (60초)"),
        Ok(Err(e)) => anyhow::bail!("브라우저 실행 실패: {e}"),
        Ok(Ok(o)) => String::from_utf8_lossy(&o.stderr).to_string(),
    };
    Ok(Some(parse_console_errors(&stderr)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uncaught_reference_error() {
        let stderr = "[38240:20464795:0829/143631.043658:INFO:CONSOLE:230] \"Uncaught ReferenceError: camTarget is not defined\", source: file:///tmp/game.js (230)";
        let errors = parse_console_errors(stderr);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("camTarget is not defined"));
    }

    #[test]
    fn ignores_info_logs_and_dedups() {
        let stderr = "\
[1:1:INFO:CONSOLE(1)] \"게임 초기화 완료\", source: x (1)
[2:2:INFO:CONSOLE(2)] \"Uncaught TypeError: foo is not a function\", source: x (2)
[3:3:INFO:CONSOLE(3)] \"Uncaught TypeError: foo is not a function\", source: x (2)";
        let errors = parse_console_errors(stderr);
        assert_eq!(errors.len(), 1, "정보 로그 무시·중복 제거: {errors:?}");
    }

    #[test]
    fn detects_browser_or_returns_none() {
        // macOS 기본 경로에 Chrome 이 있는 환경에서는 Some, 없으면 None — 둘 다 유효.
        let found = detect_browser();
        if std::path::Path::new(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        )
        .exists()
        {
            assert!(found.is_some());
        }
    }
}
