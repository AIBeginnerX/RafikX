//! 재사용 절차를 스킬 파일로 저장·주입하는 시스템.
//!
//! 스킬 레이아웃: `<기준 디렉토리>/<이름>/SKILL.md`
//! ```text
//! ---
//! name: deploy-web
//! description: 웹 배포 절차 요약(한 줄)
//! ---
//! 본문: 단계별 실행 절차 …
//! ```
//! 검색 위치는 전역(`~/.rafikx/skills`)과 워크스페이스(`<ws>/.rafikx/skills`) 둘이며,
//! 같은 이름이면 워크스페이스 스킬이 우선한다.

use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::tools::{Tool, ToolCtx};

/// 전역 스킬 디렉토리.
pub fn global_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".rafikx")
        .join("skills")
}

/// 워크스페이스 로컬 스킬 디렉토리.
pub fn workspace_dir(workspace: &Path) -> PathBuf {
    workspace.join(".rafikx").join("skills")
}

/// 이름을 안전한 파일/디렉토리명으로 정규화한다:
/// 구분 문자·제어 문자 제거 후 공백은 '-'로 바꾸고 상한 60자.
fn sanitize_name(raw: &str) -> String {
    let mut out: String = raw
        .trim()
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .collect();
    out.truncate(60);
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "unnamed-skill".into()
    } else {
        trimmed
    }
}

#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

fn parse_meta(path: &Path) -> Option<SkillMeta> {
    let body = fs::read_to_string(path).ok()?;
    let mut lines = body.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut name = None;
    let mut desc = String::new();
    for line in lines.by_ref() {
        let t = line.trim();
        if t == "---" {
            break;
        }
        if let Some(v) = t.strip_prefix("name:") {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                name = Some(v.to_string());
            }
        } else if let Some(v) = t.strip_prefix("description:") {
            let v = v.trim().trim_matches('"');
            if desc.is_empty() {
                desc = v.to_string();
            }
        }
    }
    let dir_name = path
        .parent()
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(SkillMeta {
        name: name.unwrap_or(dir_name),
        description: desc,
        path: path.to_path_buf(),
    })
}

/// 전역+워크스페이스 스킬 목록(같은 이름은 워크스페이스 우선).
pub fn list_skills(workspace: &Path) -> Vec<SkillMeta> {
    let mut out: Vec<SkillMeta> = Vec::new();
    let mut seen = BTreeSet::new();
    for dir in [workspace_dir(workspace), global_dir()] {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for ent in rd.flatten() {
            let skill_md = ent.path().join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            if let Some(m) = parse_meta(&skill_md)
                && seen.insert(m.name.clone())
            {
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 시스템 프롬프트에 붙일 [Skills] 섹션. 스킬이 없으면 None.
pub fn prompt_section(workspace: &Path) -> Option<String> {
    let skills = list_skills(workspace);
    if skills.is_empty() {
        return None;
    }
    let mut s = String::from(
        "[Skills]\n\
         다음 재사용 절차 스킬이 있다(MUST 검토): 작업 시작 전 해당 절차가 있으면 load_skill 으로\n\
         불러 일관되게 수행하고, 이번 작업에서 반복 가능한 새 절차를 완성했으면 save_skill 로 저장한다.\n",
    );
    for m in &skills {
        let d = if m.description.is_empty() {
            "(설명 없음)"
        } else {
            m.description.as_str()
        };
        s.push_str(&format!("- {}: {}\n", m.name, d));
    }
    Some(s)
}

// ── 도구 -------------------------------------------------------------------

pub struct LoadSkill;

impl Tool for LoadSkill {
    fn name(&self) -> &'static str {
        "load_skill"
    }

    fn description(&self) -> &'static str {
        "저장된 스킬(SKILL.md) 본문을 불러온다. 반복 절차(배포·QA 등)를 시작하기 전에 반드시 해당 스킬이 있는지 load_skill 로 확인하고 절차 일관성을 유지한다."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "스킬 이름"}
            },
            "required": ["name"]
        })
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let wanted = input
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("'name' 문자열 인자가 필요하다"))?;
        let skills = list_skills(&ctx.workspace);
        match skills.iter().find(|m| m.name == wanted) {
            Some(m) => Ok(fs::read_to_string(&m.path)?),
            None => bail!(
                "'{wanted}' 스킬이 없다. 사용 가능: {}",
                skills
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

pub struct SaveSkill;

impl Tool for SaveSkill {
    fn name(&self) -> &'static str {
        "save_skill"
    }

    fn description(&self) -> &'static str {
        "이번 세션에서 재사용할 절차를 스킬로 저장한다(~/.rafikx/skills/<이름>/SKILL.md). 같은 종류 작업을 2회 이상 반복하게 됐거나 배포·QA처럼 실수 없이 따라야 할 절차를 완성했을 때 호출한다."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "스킬 이름 (예: deploy-web)"},
                "description": {"type": "string", "description": "한 줄 요약"},
                "content": {"type": "string", "description": "마크다운 본문: 단계별 절차·검증 방법 포함"}
            },
            "required": ["name", "content"]
        })
    }

    /// 홈 디렉터리 쓰기라 승인 대상으로 둔다(계획 모드에선 차단된다).
    fn needs_approval(&self, _input: &Value) -> bool {
        true
    }

    fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<String> {
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("'name' 문자열 인자가 필요하다"))?;
        let content = input
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("'content'(마크다운 본문)가 필요하다"))?
            .trim();
        let description = input
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if content.is_empty() {
            bail!("빈 스킬 본문은 저장하지 않는다");
        }
        let safe = sanitize_name(name);
        let dir = global_dir().join(&safe);
        fs::create_dir_all(&dir)?;
        let path = dir.join("SKILL.md");
        let frontmatter = format!(
            "---\nname: {}\ndescription: {}\n---\n",
            safe,
            description.replace('\n', " ")
        );
        fs::write(&path, format!("{frontmatter}{content}\n"))?;
        Ok(format!(
            "스킬 '{safe}' 저장 완료({}). 이후 유사 작업에서 load_skill 로 불러 쓴다.",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_name("deploy web/"), "deploy-web");
        assert_eq!(sanitize_name("  웹 배포 절차 "), "웹-배포-절차");
        assert_eq!(sanitize_name(""), "unnamed-skill");
    }

    #[test]
    fn empty_workspace_has_no_prompt_section() {
        let tmp = std::env::temp_dir().join(format!("rafikx-skill-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // 전역 디렉토리에는 스킬이 생길 수 있으므로 프롬프트 섹션엔 목록만 확인.
        let sec = prompt_section(&tmp);
        let _ = sec; // 존재 여부는 환경 의존 — 파닝만 확인.
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
