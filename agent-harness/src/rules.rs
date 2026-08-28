//! 프로젝트 규칙 주입 + /init-deep (F7) — AGENTS.md·.rafikx/rules/*.md 를 매 요청
//! 시스템 프롬프트에 싣고, 없는 프로젝트에는 초안을 생성한다.
//!
//! oh-my-openagent 의 rules injection·/init-deep 에 해당. 모델 호출 없이 결정적으로
//! 동작한다 — 초안 품질은 규칙이 생기는 비용을 0으로 낮추는 데 있다 (정성 문구는
//! 사람이나 에이전트가 다듬는다).

use std::path::{Path, PathBuf};

/// 주입 상한 — 넘으면 본문 대신 파일 목록만 (F7).
pub const MAX_RULES_CHARS: usize = 2000;

/// 매 요청 주입할 규칙 블록을 모은다. 없으면 빈 문자열.
pub fn collect_rules(workspace: &Path) -> String {
    let mut parts: Vec<(String, String)> = Vec::new();
    if let Ok(body) = std::fs::read_to_string(workspace.join("AGENTS.md")) {
        if !body.trim().is_empty() {
            parts.push(("AGENTS.md".into(), body.trim().to_string()));
        }
    }
    let rules_dir = workspace.join(".rafikx").join("rules");
    if let Ok(rd) = std::fs::read_dir(&rules_dir) {
        let mut files: Vec<_> = rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .collect();
        files.sort_by_key(|e| e.file_name());
        for f in files {
            if let Ok(body) = std::fs::read_to_string(f.path())
                && !body.trim().is_empty()
            {
                parts.push((
                    format!(".rafikx/rules/{}", f.file_name().to_string_lossy()),
                    body.trim().to_string(),
                ));
            }
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    let joined_len: usize = parts.iter().map(|(_, b)| b.chars().count()).sum();
    if joined_len <= MAX_RULES_CHARS {
        let body = parts
            .iter()
            .map(|(name, body)| format!("## {name}\n{body}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        format!("[프로젝트 규칙 — 반드시 따를 것]\n{body}")
    } else {
        let names: Vec<String> = parts.iter().map(|(n, _)| n.clone()).collect();
        format!(
            "[프로젝트 규칙] 규칙 파일이 {MAX_RULES_CHARS}자를 넘어 목록만 표시합니다: {}. 작업과 관련된 파일을 read_file 로 읽고 따르세요.",
            names.join(", ")
        )
    }
}

/// /init-deep 산출물 한 건 — 파일이 있으면 덮어쓰지 않고 diff 만 제안한다.
#[derive(Debug, Clone)]
pub struct InitDeepProposal {
    pub path: PathBuf,
    pub content: String,
    pub exists: bool,
    pub diff: Option<String>,
}

/// 프로젝트 유형 감지 — 검증 명령과 역할 문구의 근거.
fn detect_stack(workspace: &Path) -> (&'static str, &'static str) {
    if workspace.join("Cargo.toml").exists() {
        ("Rust (Cargo)", "cargo check && cargo test")
    } else if workspace.join("pyproject.toml").exists() || workspace.join("requirements.txt").exists() {
        ("Python", "python3 -m pytest -q")
    } else if workspace.join("package.json").exists() {
        ("Node/JS", "npm test")
    } else if workspace.join("go.mod").exists() {
        ("Go", "go test ./...")
    } else {
        ("일반", "프로젝트 검증 명령을 여기에 적는다")
    }
}

/// 초안 본문 — 10줄 이내, 역할·관습·금지·검증.
pub fn draft_agents_md(dir_label: &str, stack: &str, verify: &str) -> String {
    format!(
        "# AGENTS.md — {dir_label}\n\n\
         - 스택: {stack}\n\
         - 역할: 이 디렉터리의 책임 범위를 한 줄로 적는다.\n\
         - 관습: 기존 코드 스타일·명명·구조를 따른다. 옆에 두 번째 관례를 만들지 않는다.\n\
         - 금지: 빌드 산출물·비밀값(키·토큰)을 커밋하지 않는다.\n\
         - 검증: `{verify}`\n"
    )
}

/// 루트 + 1단계 코드 디렉터리 후보.
fn target_dirs(workspace: &Path) -> Vec<PathBuf> {
    let mut out = vec![workspace.to_path_buf()];
    for name in ["src", "agent-harness", "desktop", "docs", "scripts", "tests", "packages", "crates"] {
        let dir = workspace.join(name);
        if dir.is_dir() {
            out.push(dir);
        }
    }
    out
}

/// 초안을 계산한다 (쓰기는 호출부 몫).
pub fn propose_init_deep(workspace: &Path) -> Vec<InitDeepProposal> {
    let (stack, verify) = detect_stack(workspace);
    target_dirs(workspace)
        .into_iter()
        .map(|dir| {
            let label = if dir == workspace {
                workspace
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "프로젝트 루트".into())
            } else {
                dir.file_name().unwrap().to_string_lossy().into_owned()
            };
            let content = draft_agents_md(&label, stack, verify);
            let path = dir.join("AGENTS.md");
            let existing = std::fs::read_to_string(&path).ok();
            let diff = existing.as_ref().and_then(|old| {
                if old == &content {
                    None
                } else {
                    Some(unified_diff(old, &content, &path.display().to_string()))
                }
            });
            InitDeepProposal {
                exists: existing.is_some(),
                path,
                content,
                diff,
            }
        })
        .collect()
}

/// unified diff — similar 크레이트(기존 의존성)로 생성.
fn unified_diff(old: &str, new: &str, path: &str) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut out = format!("--- {path} (기존)\n+++ {path} (제안)\n");
    for hunk in diff.ops() {
        for change in diff.iter_changes(hunk) {
            let sign = match change.tag() {
                similar::ChangeTag::Delete => "-",
                similar::ChangeTag::Insert => "+",
                similar::ChangeTag::Equal => " ",
            };
            out.push_str(sign);
            out.push_str(change.value());
        }
    }
    out
}

/// 제안을 적용한다 — 없는 파일만 생성, 있는 파일은 건드리지 않는다.
/// 반환: (생성된 경로 목록, diff 제안이 있던 경로 목록)
pub fn apply_init_deep(proposals: &[InitDeepProposal]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut created = Vec::new();
    let mut proposed = Vec::new();
    for p in proposals {
        if p.exists {
            if p.diff.is_some() {
                proposed.push(p.path.clone());
            }
        } else if std::fs::write(&p.path, &p.content).is_ok() {
            created.push(p.path.clone());
        }
    }
    (created, proposed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rafikx-rules-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_without_rule_files() {
        let dir = setup("empty");
        assert!(collect_rules(&dir).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn injects_agents_md_and_rules_sorted() {
        let dir = setup("inject");
        std::fs::write(dir.join("AGENTS.md"), "루트 규칙").unwrap();
        let rules = dir.join(".rafikx").join("rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("b-style.md"), "스타일").unwrap();
        std::fs::write(rules.join("a-test.md"), "테스트").unwrap();
        let block = collect_rules(&dir);
        assert!(block.contains("[프로젝트 규칙"));
        assert!(block.contains("## AGENTS.md"));
        assert!(block.find("a-test.md").unwrap() < block.find("b-style.md").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn over_cap_lists_files_only() {
        let dir = setup("cap");
        std::fs::write(dir.join("AGENTS.md"), "가".repeat(3000)).unwrap();
        let block = collect_rules(&dir);
        assert!(block.contains("목록만 표시"));
        assert!(block.contains("AGENTS.md"));
        assert!(!block.contains(&"가".repeat(3000)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn detects_rust_stack() {
        let dir = setup("rust");
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        let (stack, verify) = detect_stack(&dir);
        assert!(stack.contains("Rust"));
        assert!(verify.contains("cargo"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn draft_is_short_and_has_sections() {
        let draft = draft_agents_md("src", "Rust (Cargo)", "cargo check");
        assert!(draft.lines().count() <= 10);
        assert!(draft.contains("역할"));
        assert!(draft.contains("금지"));
        assert!(draft.contains("검증"));
    }

    #[test]
    fn apply_creates_only_missing_and_proposes_diff() {
        let dir = setup("apply");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.join("AGENTS.md"), "기존 내용 — 손대지 말 것").unwrap();
        let proposals = propose_init_deep(&dir);
        let (created, proposed) = apply_init_deep(&proposals);
        // 루트는 기존 파일이 있어 diff 제안만, src 는 새로 생성
        assert!(created.iter().any(|p| p.ends_with("src/AGENTS.md")));
        assert!(proposed.iter().any(|p| p.ends_with("AGENTS.md") && !p.ends_with("src/AGENTS.md")));
        let untouched = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert_eq!(untouched, "기존 내용 — 손대지 말 것");
        let _ = std::fs::remove_dir_all(dir);
    }
}
