//! 코드 품질 엔진 — 언어 프로파일 (S0 감지 + S3 기계 게이트 명령).
//! 근거: docs/agent-upgrade/07_QUALITY.md §3.
//! 프로파일은 언어별 시니어 표준을 데이터화한다: 포맷·린트·테스트·보안·벤치 도구와
//! idiom 규칙. S0 에서 언어를 감지해 이후 게이트의 명령을 결정한다.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Shell,
    Sql,
    Unknown,
}

impl Language {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Go => "go",
            Self::Shell => "shell",
            Self::Sql => "sql",
            Self::Unknown => "unknown",
        }
    }
}

/// 언어 프로파일 — S3 기계 게이트 명령과 idiom 규칙의 데이터.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageProfile {
    pub language: Language,
    /// S3 게이트 명령 — 순서대로 실행, 하나라도 실패하면 게이트 실패.
    /// `{files}` 자리표시자는 변경 파일 목록으로 치환된다.
    pub format_check: Vec<String>,
    pub lint: Vec<String>,
    pub test: Vec<String>,
    /// 의존성 감사 명령 — 가용성 검사는 실행기가 한다.
    pub audit: Vec<String>,
    /// idiom 금지 패턴 — 간단한 텍스트 스캔용 (언어별 최소 세트).
    pub forbidden_patterns: Vec<(String, String)>,
}

type ProfileCommands = (
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<(String, String)>,
);

/// 워크스페이스·변경 파일에서 언어를 감지한다 (S0).
pub fn detect(workspace: &std::path::Path, changed: &[String]) -> Language {
    let has = |marker: &str| workspace.join(marker).exists();
    let ext_of = |p: &str| p.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    // 변경 파일 확장자 우선 — 이번 작업의 언어가 무엇인지가 중요하다.
    let mut counts = std::collections::BTreeMap::new();
    for p in changed {
        *counts.entry(ext_of(p)).or_insert(0u32) += 1;
    }
    let top = |ext: &str| counts.get(ext).copied().unwrap_or(0);
    if top("rs") > 0 || (changed.is_empty() && has("Cargo.toml")) {
        return Language::Rust;
    }
    if top("ts") > 0 || top("tsx") > 0 {
        return Language::TypeScript;
    }
    if top("js") > 0 || top("jsx") > 0 {
        return Language::JavaScript;
    }
    if top("py") > 0 || (changed.is_empty() && (has("pyproject.toml") || has("requirements.txt"))) {
        return Language::Python;
    }
    if top("go") > 0 || (changed.is_empty() && has("go.mod")) {
        return Language::Go;
    }
    if top("sh") > 0 || top("bash") > 0 {
        return Language::Shell;
    }
    if top("sql") > 0 {
        return Language::Sql;
    }
    Language::Unknown
}

/// 언어별 표준 프로파일 — 최엄격 설정의 데이터화 (지시서 §3 표).
pub fn profile_for(lang: Language) -> LanguageProfile {
    let (format_check, lint, test, audit, forbidden): ProfileCommands = match lang {
        Language::Rust => (
            vec!["rustfmt --edition 2024 --check --config skip_children=true {files}".into()],
            vec!["cargo clippy --quiet --message-format=short --no-deps".into()],
            vec!["cargo test --quiet".into()],
            vec!["cargo audit".into()],
            vec![
                (
                    "unwrap()".into(),
                    "프로덕션 경로의 unwrap() — panic 대신 ? 나 expect(사유) 를 쓴다".to_string(),
                ),
                (
                    "unsafe ".into(),
                    "unsafe 블록 — 정당화 사유와 miri 검증 계획이 필요하다".to_string(),
                ),
            ],
        ),
        Language::Python => (
            vec!["ruff format --check .".into()],
            vec!["ruff check --select ALL .".into(), "mypy --strict .".into()],
            vec!["pytest -q".into()],
            vec!["pip-audit".into()],
            vec![
                (
                    "eval(".into(),
                    "eval() — 코드 주입 위험, 대안을 쓴다".to_string(),
                ),
                (
                    "except:".into(),
                    "맨 Except — 구체 예외로 좁힌다".to_string(),
                ),
            ],
        ),
        Language::TypeScript | Language::JavaScript => (
            vec!["npx prettier --check .".into()],
            vec![
                "npx eslint . --max-warnings 0".into(),
                "npx tsc --strict --noEmit".into(),
            ],
            vec!["npx vitest run".into()],
            vec!["npm audit --offline --audit-level=high".into()],
            vec![(
                "any".into(),
                "any 타입 — unknown 으로 좁히거나 구체 타입을 쓴다".to_string(),
            )],
        ),
        Language::Go => (
            vec!["gofmt -l .".into()],
            vec!["go vet ./...".into()],
            vec!["go test ./...".into()],
            vec!["govulncheck ./...".into()],
            vec![(
                "panic(".into(),
                "라이브러리 경로의 panic — error 반환을 우선한다".to_string(),
            )],
        ),
        Language::Shell => (
            vec!["shfmt -d .".into()],
            vec!["shellcheck -S error {files}".into()],
            vec![],
            vec![],
            vec![(
                "rm -rf".into(),
                "rm -rf — 변수 경로면 안전 검증이 필요하다".to_string(),
            )],
        ),
        Language::Sql => (
            vec![],
            vec!["sqlfluff lint .".into()],
            vec![],
            vec![],
            vec![(
                "+ \"".into(),
                "문자열 연결 쿼리 — 파라미터화를 쓴다 (인젝션)".to_string(),
            )],
        ),
        Language::Unknown => (vec![], vec![], vec![], vec![], vec![]),
    };
    LanguageProfile {
        language: lang,
        format_check,
        lint,
        test,
        audit,
        forbidden_patterns: forbidden,
    }
}

/// S3 게이트 명령 생성 — 도구 가용성을 검사해 실행 가능한 명령만 남긴다.
pub fn runnable_commands(commands: &[String]) -> (Vec<String>, Vec<String>) {
    runnable_commands_in(std::path::Path::new("."), commands)
}

pub fn runnable_commands_in(
    workspace: &std::path::Path,
    commands: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut runnable = Vec::new();
    let mut missing = Vec::new();
    for cmd in commands {
        let mut words = cmd.split_whitespace();
        let binary = words.next().unwrap_or("");
        if binary == "npx" {
            let Some(package) = words.next() else {
                missing.push(cmd.clone());
                continue;
            };
            let local = workspace.join("node_modules/.bin").join(package);
            if local.is_file() {
                let suffix = words.collect::<Vec<_>>().join(" ");
                let local_cmd = format!("./node_modules/.bin/{package}");
                runnable.push(if suffix.is_empty() {
                    local_cmd
                } else {
                    format!("{local_cmd} {suffix}")
                });
            } else {
                missing.push(cmd.clone());
            }
            continue;
        }
        if binary == "npm"
            && words.next() == Some("audit")
            && !workspace.join("package-lock.json").is_file()
            && !workspace.join("npm-shrinkwrap.json").is_file()
        {
            missing.push(cmd.clone());
            continue;
        }
        if tool_available(binary) {
            runnable.push(cmd.clone());
        } else {
            missing.push(cmd.clone());
        }
    }
    (runnable, missing)
}

pub fn tool_available(binary: &str) -> bool {
    if binary == "cargo" {
        return true; // 자기 자신의 빌드 환경
    }
    let candidate = std::path::Path::new(binary);
    if candidate.components().count() > 1 {
        return candidate.is_file();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| {
        let direct = directory.join(binary);
        if direct.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            return ["exe", "cmd", "bat"]
                .iter()
                .any(|extension| direct.with_extension(extension).is_file());
        }
        #[cfg(not(windows))]
        false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_language_from_changed_files_first() {
        let dir = std::env::temp_dir().join(format!("rk-prof-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Cargo.toml 이 있어도 이번 변경이 py 면 python 이 우선 — 태스크의 언어가 중요.
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        let changed = vec!["src/main.py".to_string()];
        assert_eq!(detect(&dir, &changed), Language::Python);
        let changed = vec!["src/lib.rs".to_string()];
        assert_eq!(detect(&dir, &changed), Language::Rust);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rust_profile_is_strict_and_has_forbidden_idioms() {
        let p = profile_for(Language::Rust);
        assert!(
            p.lint
                .iter()
                .any(|command| command.contains("message-format=short"))
        );
        assert!(
            p.format_check
                .iter()
                .any(|command| command.contains("{files}"))
        );
        assert!(p.test.iter().any(|c| c.contains("cargo test")));
        assert!(!p.forbidden_patterns.is_empty());
    }

    #[test]
    fn runnable_filters_missing_tools() {
        let (runnable, missing) = runnable_commands(&[
            "cargo test --quiet".into(),
            "분명히-없는-도구42 check".into(),
        ]);
        assert!(runnable.iter().any(|c| c.contains("cargo test")));
        assert!(missing.iter().any(|c| c.contains("분명히-없는-도구42")));
    }

    #[test]
    fn javascript_tools_must_be_installed_in_the_workspace() {
        let dir = std::env::temp_dir().join(format!("rk-js-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
        let commands = vec!["npx prettier --check .".to_string()];

        let (runnable, missing) = runnable_commands_in(&dir, &commands);
        assert!(runnable.is_empty());
        assert_eq!(missing, commands);

        std::fs::write(dir.join("node_modules/.bin/prettier"), "").unwrap();
        let (runnable, missing) = runnable_commands_in(&dir, &commands);
        assert_eq!(runnable, vec!["./node_modules/.bin/prettier --check ."]);
        assert!(missing.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn npm_audit_requires_a_local_lockfile() {
        let dir = std::env::temp_dir().join(format!("rk-npm-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let commands = vec!["npm audit --offline --audit-level=high".to_string()];

        let (runnable, missing) = runnable_commands_in(&dir, &commands);
        assert!(runnable.is_empty());
        assert_eq!(missing, commands);
        let _ = std::fs::remove_dir_all(dir);
    }
}
