//! 보안 게이트 (S5) — 내장 휴리스틱 스캐너. 외부 도구(semgrep·gitleaks)가 없는
//! 환경에서도 오프라인으로 동작하고, 도구가 있으면 연결한다.
//! 근거: docs/agent-upgrade/07_QUALITY.md §5. "경고 후 진행"은 없다 — 발견은 fail.

/// 스캔 위반 한 건.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Finding {
    pub kind: &'static str,
    pub file: String,
    pub line: usize,
    pub detail: String,
}

/// 시크릿·자격증명 패턴 — 하드코딩 감지 (레드팀 시나리오 2).
const SECRET_PATTERNS: &[(&str, &str)] = &[
    ("api_key", "API 키 하드코딩 의심"),
    ("apikey", "API 키 하드코딩 의심"),
    ("secret", "시크릿 하드코딩 의심"),
    ("password", "비밀번호 하드코딩 의심"),
    ("passwd", "비밀번호 하드코딩 의심"),
    ("token", "토큰 하드코딩 의심"),
    ("private_key", "개인키 하드코딩 의심"),
    ("aws_access_key", "AWS 키 하드코딩 의심"),
];

/// 값 할당 형태의 시크릿 — key = "긴 리터럴" 패턴.
fn looks_like_assigned_secret(line: &str, keyword: &str) -> bool {
    let lower = line.to_lowercase();
    let Some(kpos) = lower.find(keyword) else {
        return false;
    };
    let after = &line[kpos + keyword.len()..];
    let after_trim = after.trim_start();
    // key: "..." 또는 key = "..." 형태의 문자열 리터럴 할당만.
    let after_trim = after_trim
        .strip_prefix(':')
        .or_else(|| after_trim.strip_prefix('='))
        .map(str::trim_start)
        .unwrap_or(after_trim);
    let Some(rest) = after_trim
        .strip_prefix('"')
        .or_else(|| after_trim.strip_prefix('\''))
    else {
        return false;
    };
    let value: String = rest.chars().take(60).collect();
    // 값이 길고 공백 없는 토큰이면 진짜 시크릿 같다. 예시 더미(skip)·환경변수 조회는 제외.
    value.len() >= 12
        && value.split_whitespace().count() <= 2
        && !lower.contains("env::")
        && !lower.contains("getenv")
        && !lower.contains("process.env")
        && !value.contains("your")
        && !value.contains("example")
        && !value.contains("placeholder")
}

fn contains_call(line: &str, name: &str) -> bool {
    let needle = format!("{name}(");
    line.match_indices(&needle).any(|(index, _)| {
        let quoted = line[..index]
            .char_indices()
            .filter(|(position, character)| {
                *character == '"'
                    && (*position == 0 || line.as_bytes().get(position - 1).copied() != Some(b'\\'))
            })
            .count()
            % 2
            == 1;
        !quoted
            && (index == 0
                || line[..index]
                    .chars()
                    .next_back()
                    .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_'))
    })
}

/// 변경 파일들의 텍스트를 스캔한다 (S5). 발견 목록이 비어 있어야 통과다.
pub fn scan(file: &str, text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let lower_path = file.to_lowercase();
    // 테스트·픽스처·문서 파일은 더미 시크릿이 흔하므로 시크릿 스캔 제외.
    let is_testish = lower_path.contains("test")
        || lower_path.contains("fixture")
        || lower_path.contains("example")
        || lower_path.ends_with(".md");

    let lines = text.lines().collect::<Vec<_>>();
    let production = if lower_path.ends_with(".rs") {
        super::production_line_mask(&lines)
    } else {
        vec![true; lines.len()]
    };
    for (line_no, line) in lines.into_iter().enumerate() {
        if !production[line_no] {
            continue;
        }
        let no = line_no + 1;
        // 1) 시크릿 하드코딩
        if !is_testish {
            for (keyword, why) in SECRET_PATTERNS {
                if lower_path.ends_with(".md") {
                    break;
                }
                if line.to_lowercase().contains(keyword)
                    && looks_like_assigned_secret(line, keyword)
                {
                    findings.push(Finding {
                        kind: "secret",
                        file: file.to_string(),
                        line: no,
                        detail: format!("{why} — {keyword} = \"…\""),
                    });
                }
            }
        }
        // 2) SQL 문자열 연결 인젝션 (레드팀 시나리오 1) — 쿼리 + 변수 연결
        let lower = line.to_lowercase();
        let sql_keyword = ["select ", "insert ", "update ", "delete "]
            .iter()
            .any(|k| lower.contains(k));
        if sql_keyword
            && (line.contains("\" +")
                || line.contains("+ \"")
                || line.contains("' +")
                || line.contains("+ '")
                || line.contains("{") && line.contains("}"))
            && !lower.contains("?")
            && !line.contains("-- ")
        {
            findings.push(Finding {
                kind: "sql-injection",
                file: file.to_string(),
                line: no,
                detail: "SQL 문자열 연결/포맷 — 파라미터화 쿼리로 교체하라".into(),
            });
        }
        // 3) 에러 메시지 내부 정보 노출 (레드팀 시나리오 9)
        let exposes_stack = super::code_contains_pattern(&lower, "e.stack")
            || super::code_contains_pattern(&lower, "backtrace()")
            || super::code_contains_pattern(&lower, "format!(\"{:?}\", err")
            || (super::code_contains_pattern(&lower, "500")
                && super::code_contains_pattern(&lower, "internal error")
                && super::code_contains_pattern(&lower, "path"));
        if exposes_stack {
            findings.push(Finding {
                kind: "error-exposure",
                file: file.to_string(),
                line: no,
                detail: "에러 응답에 스택트레이스·내부 정보 노출 — 일반 메시지로 대체하라".into(),
            });
        }
        // 4) 위험 함수 — eval 계열
        if contains_call(&lower, "eval") || contains_call(&lower, "exec") {
            findings.push(Finding {
                kind: "dangerous-call",
                file: file.to_string(),
                line: no,
                detail: "eval/exec 계열 호출 — 동적 코드 실행은 주입 표면이다".into(),
            });
        }
    }
    findings
}

fn bounded_output_tail(raw: &[u8], max_chars: usize) -> String {
    let text = String::from_utf8_lossy(raw);
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(max_chars)).collect()
}

/// 의존성 감사 (S5) — 외부 도구가 있으면 실행, 없으면 스킵 보고.
/// 반환: (실행한 명령, 통과 여부, 출력 요약)
pub async fn run_dependency_audit(
    lang: crate::quality::profile::Language,
    workspace: &std::path::Path,
) -> Vec<(String, Option<bool>, String)> {
    use crate::quality::profile::{profile_for, runnable_commands_in};
    let profile = profile_for(lang);
    let (runnable, missing) = runnable_commands_in(workspace, &profile.audit);
    let mut results = Vec::new();
    for cmd in runnable {
        let argv = super::command_argv(&cmd, workspace, &[]);
        let (program, args) = match argv {
            Ok(Some(command)) => command,
            Ok(None) => continue,
            Err(error) => {
                results.push((cmd, Some(false), error));
                continue;
            }
        };
        let out = super::run_bounded_command(
            &program,
            &args,
            workspace,
            std::time::Duration::from_secs(120),
        )
        .await;
        let (ok, summary) = match out {
            Err(error) => (Some(false), error),
            Ok(output) if output.overflow => {
                (Some(false), "출력이 수집 상한을 초과했습니다".into())
            }
            Ok(output) => {
                let raw = if output.stderr.is_empty() {
                    &output.stdout
                } else {
                    &output.stderr
                };
                (Some(output.status.success()), bounded_output_tail(raw, 200))
            }
        };
        results.push((cmd, ok, summary));
    }
    for cmd in missing {
        results.push((cmd, None, "도구 미설치 — 설치 권장".into()));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_output_tail_keeps_short_multiline_context() {
        let raw = b"Authorization:\n  [Bearer SYNTHETIC]\n";
        assert_eq!(bounded_output_tail(raw, 200), String::from_utf8_lossy(raw));
    }

    #[test]
    fn hardcoded_api_key_is_caught() {
        let code = "const config = { api_key: \"sk-live-1234567890abcdef\" };";
        let findings = scan("src/config.ts", code);
        assert!(
            findings.iter().any(|f| f.kind == "secret"),
            "시크릿 감지: {findings:?}"
        );
    }

    #[test]
    fn env_lookup_is_not_flagged() {
        let code = "let key = std::env::var(\"API_KEY\").unwrap_or_default();";
        assert!(scan("src/main.rs", code).iter().all(|f| f.kind != "secret"));
    }

    #[test]
    fn production_secrets_after_test_modules_are_scanned() {
        let code = concat!(
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    let api_key = \"dummy-test-value\";\n",
            "}\n",
            "#[cfg(not(test))]\n",
            "mod production {\n",
            "    let api_key = \"sk-prod-1234567890abcdef\";\n",
            "}\n",
            "let api_key = \"sk-live-1234567890abcdef\";\n",
        );
        let findings = scan("src/config.rs", code);
        let secret_lines = findings
            .iter()
            .filter(|finding| finding.kind == "secret")
            .map(|finding| finding.line)
            .collect::<Vec<_>>();
        assert_eq!(secret_lines, [7, 9]);
    }

    #[test]
    fn test_files_skip_secret_scan() {
        let code = "let t = test_password: \"dummy-12345678\";";
        assert!(
            scan("tests/fixtures.rs", code)
                .iter()
                .all(|f| f.kind != "secret")
        );
    }

    #[test]
    fn sql_concatenation_is_caught() {
        let code = "let q = \"SELECT * FROM users WHERE name = '\" + name + \"'\";";
        let findings = scan("src/db.rs", code);
        assert!(
            findings.iter().any(|f| f.kind == "sql-injection"),
            "SQL 인젝션 감지: {findings:?}"
        );
    }

    #[test]
    fn parameterized_query_passes() {
        let code = "let q = \"SELECT * FROM users WHERE name = ?\";";
        assert!(
            scan("src/db.rs", code)
                .iter()
                .all(|f| f.kind != "sql-injection")
        );
    }

    #[test]
    fn pre_exec_is_not_misclassified_as_exec() {
        assert!(scan("src/process.rs", "command.pre_exec(|| Ok(()));").is_empty());
        assert!(
            scan("src/process.rs", "exec(user_input);")
                .iter()
                .any(|finding| finding.kind == "dangerous-call")
        );
    }

    #[test]
    fn stack_trace_exposure_is_caught() {
        let code = "res.status(500).json({ error: e.stack });";
        let findings = scan("src/server.ts", code);
        assert!(
            findings.iter().any(|f| f.kind == "error-exposure"),
            "스택 노출 감지: {findings:?}"
        );
    }
}
