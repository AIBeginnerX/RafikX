use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, anyhow};
use regex::Regex;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::db::Db;
use crate::obsidian;
use crate::provider::ToolSpec;
use crate::run::{RunContext, RunEventKind};

use crate::agent::{LocalAsk, RemoteApproval};
use crate::tools_more::{
    ApplyPatch, GlobTool, MultiEdit, TodoRead, TodoWrite, WebFetch, WebSearch,
};

mod facts;
pub(crate) mod hashline;
mod lsp_tools;
pub mod mutation;
mod task;
pub(crate) mod workspace_delta;

use lsp_tools::{LspDefinition, LspDiagnostics, LspHover, LspReferences};
pub use task::{TaskArgs, TaskResult, TaskTool};

pub const MAX_LIST_ITEMS: usize = 500;
pub const MAX_GREP_LINES: usize = 200;
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
pub const MAX_BASH_OUTPUT: usize = 20 * 1024;
pub const BASH_TIMEOUT_SECS: u64 = 60;

pub trait Tool {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn input_schema(&self) -> Value;
    fn needs_approval(&self, input: &Value) -> bool;
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String>;
}

pub struct ToolCtx {
    pub workspace: PathBuf,
    pub vault: Option<PathBuf>,
    pub db_path: PathBuf,
    /// 해시 앵커(hashline) 모드 — config [edit] hashline. 테스트 기본 true.
    pub hashline: bool,
    /// 서브에이전트(task 도구)가 승인 흐름을 이어받기 위한 채널.
    pub local_ask: Option<LocalAsk>,
    pub remote: Option<RemoteApproval>,
    pub run: Option<RunContext>,
}

impl ToolCtx {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            vault: None,
            db_path: PathBuf::from("."),
            hashline: true,
            local_ask: None,
            remote: None,
            run: None,
        }
    }

    pub(crate) fn commit_mutation(
        &self,
        plan: mutation::MutationPlan,
    ) -> Result<mutation::MutationReceipt> {
        let baselines = plan.baselines();
        let receipt = plan.commit()?;
        if let Some(run) = &self.run {
            run.record_committed_changes(baselines);
            run.emit(
                RunEventKind::Mutation,
                json!({
                    "committed": receipt.committed,
                    "changed": receipt.changed,
                    "created": receipt.created,
                    "updated": receipt.updated,
                    "deleted": receipt.deleted,
                }),
            );
        }
        Ok(receipt)
    }
}

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool + Send + Sync>>,
}

impl ToolRegistry {
    pub fn all() -> Self {
        Self {
            tools: vec![
                Box::new(ReadFile),
                Box::new(ListDir),
                Box::new(Grep),
                Box::new(GlobTool),
                Box::new(WebFetch),
                Box::new(WebSearch),
                Box::new(LspDiagnostics),
                Box::new(LspDefinition),
                Box::new(LspHover),
                Box::new(LspReferences),
                Box::new(EditFile),
                Box::new(MultiEdit),
                Box::new(WriteFile),
                Box::new(Bash),
                Box::new(ApplyPatch),
                Box::new(TodoWrite),
                Box::new(TodoRead),
                Box::new(ObsidianSearch),
                Box::new(TaskTool),
                Box::new(crate::skills::LoadSkill),
                Box::new(crate::skills::SaveSkill),
                Box::new(crate::mcp::McpList),
                Box::new(crate::mcp::McpCall),
                Box::new(facts::Remember),
                Box::new(facts::Recall),
                Box::new(facts::Forget),
            ],
        }
    }

    /// 읽기 전용 도구 집합 (plan 모드와 프로파일 확장에 사용).
    pub const READ_ONLY: &'static [&'static str] = &[
        "read_file",
        "list_dir",
        "grep",
        "glob",
        "webfetch",
        "web_search",
        "lsp_diagnostics",
        "lsp_definition",
        "lsp_hover",
        "lsp_references",
        "todo_read",
        "todo_write",
        "obsidian_search",
        "load_skill",
        "mcp_list",
    ];

    pub fn with_names(names: &[String]) -> Self {
        if names.iter().any(|n| n == "*") {
            return Self::all();
        }
        let mut expanded: Vec<String> = names.to_vec();
        // 읽기 도구를 하나라도 쓰는 프로파일엔 나머지 읽기 전용 도구를 자동 포함
        // (옛 config 가 새 도구 없이도 동일한 읽기 능력을 갖도록).
        if names
            .iter()
            .any(|n| matches!(n.as_str(), "read_file" | "list_dir" | "grep"))
        {
            for ro in Self::READ_ONLY {
                if !expanded.iter().any(|e| e == ro) {
                    expanded.push((*ro).to_string());
                }
            }
        }
        let all = Self::all();
        Self {
            tools: all
                .tools
                .into_iter()
                .filter(|t| expanded.iter().any(|n| n == t.name()))
                .collect(),
        }
    }

    /// 지정한 도구들을 제외한 레지스트리 (task 재귀 방지·plan 모드 등).
    pub fn without(self, exclude: &[&str]) -> Self {
        Self {
            tools: self
                .tools
                .into_iter()
                .filter(|t| !exclude.contains(&t.name()))
                .collect(),
        }
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&(dyn Tool + Send + Sync)> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }
}

pub fn resolve_in_workspace(workspace: &Path, user_path: &str) -> Result<PathBuf> {
    if user_path.trim().is_empty() {
        return Err(anyhow!("경로가 비어 있습니다"));
    }
    let ws = workspace
        .canonicalize()
        .map_err(|_| anyhow!("워크스페이스가 없습니다: {}", workspace.display()))?;
    let requested = Path::new(user_path);
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    };
    let normalized = normalize_path(&joined);
    if normalized.exists() {
        let canon = normalized
            .canonicalize()
            .map_err(|_| anyhow!("경로를 열 수 없습니다"))?;
        if !canon.starts_with(&ws) {
            return Err(anyhow!("경로 jail 위반: workspace 밖은 접근할 수 없습니다"));
        }
        Ok(canon)
    } else {
        let mut ancestor = normalized.as_path();
        let mut missing = Vec::new();
        loop {
            if ancestor.exists() {
                break;
            }
            match ancestor.file_name() {
                Some(name) => missing.push(name.to_os_string()),
                None => return Err(anyhow!("경로 jail 위반: workspace 밖은 접근할 수 없습니다")),
            }
            ancestor = ancestor
                .parent()
                .ok_or_else(|| anyhow!("경로 jail 위반: workspace 밖은 접근할 수 없습니다"))?;
        }
        let canon_anc = ancestor
            .canonicalize()
            .map_err(|_| anyhow!("경로를 열 수 없습니다"))?;
        if !canon_anc.starts_with(&ws) {
            return Err(anyhow!("경로 jail 위반: workspace 밖은 접근할 수 없습니다"));
        }
        let mut out = canon_anc;
        for name in missing.into_iter().rev() {
            out.push(name);
        }
        Ok(out)
    }
}

/// 쓰기 대상 경로 해석 — 파일 수정 도구(edit/write/multi_edit/apply_patch) 전용.
/// 읽기 도구는 이 게이트를 거치지 않는다 (acceptance 읽기는 자유).
pub fn resolve_tool_path(ctx: &ToolCtx, user_path: &str) -> Result<PathBuf> {
    let resolved = resolve_in_workspace(&ctx.workspace, user_path)?;
    reject_vault(&resolved, ctx.vault.as_deref())?;
    reject_acceptance(&resolved)?;
    Ok(resolved)
}

/// tests/acceptance/ 는 SPEC 동결 산출물 — 에이전트 도구의 쓰기 대상이 아니다.
/// 읽기는 자유로우므로 검증자·Executor 가 내용을 확인하는 데는 지장이 없다 (M3: G4).
fn reject_acceptance(path: &Path) -> Result<()> {
    let rendered = path.to_string_lossy();
    if rendered.contains("tests/acceptance/") || rendered.contains("tests\\acceptance\\") {
        return Err(anyhow!(
            "tests/acceptance/ 는 SPEC 동결 산출물이다 — 에이전트 도구로 수정할 수 없다(NEVER). 인수 테스트 변경은 사용자 절차로만 한다"
        ));
    }
    Ok(())
}

fn reject_vault(path: &Path, vault: Option<&Path>) -> Result<()> {
    let Some(vault) = vault else {
        return Ok(());
    };
    if !vault.exists() {
        return Ok(());
    }
    let Ok(v) = vault.canonicalize() else {
        return Ok(());
    };
    if path.starts_with(&v) {
        return Err(anyhow!(
            "Obsidian Vault는 파일 도구로 열 수 없습니다. obsidian_search 를 사용하세요"
        ));
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

pub fn unified_diff(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
            ChangeTag::Equal => ' ',
        };
        out.push(sign);
        out.push_str(change.value());
        if !change.value().ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

pub fn code_change_summary(
    action: &str,
    path: &std::path::Path,
    before: &str,
    old: &str,
    new: &str,
) -> String {
    let start = before
        .find(old)
        .map(|idx| before[..idx].bytes().filter(|b| *b == b'\n').count() + 1)
        .unwrap_or(1);
    let removed = if old.is_empty() {
        0
    } else {
        old.lines().count().max(1)
    };
    let added = if new.is_empty() {
        0
    } else {
        new.lines().count().max(1)
    };
    let span = removed.max(added).max(1);
    let end = start + span - 1;
    format!(
        "[코드 변경] {action} {}:{start}-{end} · +{added} -{removed}",
        path.display()
    )
}

/// 편집 후 언어 서버 진단 자동 수집 — 자가 검증 루프의 핵심.
/// 예산(5초) 안에 서버가 답하면 결과를 덧붙이고, 서버가 없거나 늦거나
/// 런타임이 block_in_place 를 못 쓰는 형태면 조용히 포기한다 (본 결과를 절대 막지 않는다).
pub(crate) fn with_auto_diagnostics(output: String, resolved: &Path, ctx: &ToolCtx) -> String {
    if !crate::lsp::has_server(resolved) {
        return output;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return output;
    };
    // current_thread 런타임에선 block_in_place 가 패닉 — 자동 진단은 포기.
    if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
        return output;
    }
    let workspace = ctx.workspace.clone();
    let path = resolved.to_path_buf();
    let run = ctx.run.as_ref();
    let note = tokio::task::block_in_place(|| {
        handle.block_on(crate::lsp::diagnostics_quick(&workspace, &path, run))
    });
    let Ok(text) = note else {
        return output;
    };
    let summary: Vec<&str> = text.lines().take(5).collect();
    format!("{output}\n\n[lsp] {}", summary.join("\n"))
}

pub fn approval_preview(name: &str, input: &Value, ctx: &ToolCtx) -> Result<String> {
    match name {
        "write_file" => {
            let path = str_field(input, "path")?;
            let content = str_field(input, "content")?;
            let resolved = resolve_tool_path(ctx, path)?;
            let old = if resolved.exists() {
                fs::read_to_string(&resolved).unwrap_or_default()
            } else {
                String::new()
            };
            Ok(format!(
                "[승인] write_file\npath: {}\n--- diff ---\n{}",
                resolved.display(),
                unified_diff(&old, content)
            ))
        }
        "edit_file" => {
            let path = str_field(input, "path")?;
            let old_str = str_field(input, "old_str")?;
            let new_str = str_field(input, "new_str")?;
            let resolved = resolve_tool_path(ctx, path)?;
            let body = fs::read_to_string(&resolved)
                .map_err(|_| anyhow!("파일을 읽을 수 없습니다: {}", resolved.display()))?;
            let count = body.matches(old_str).count();
            if count != 1 {
                return Err(anyhow!(
                    "old_str 가 파일에서 {count}번 나타납니다. 정확히 1번이어야 합니다."
                ));
            }
            let updated = body.replacen(old_str, new_str, 1);
            Ok(format!(
                "[승인] edit_file\npath: {}\n--- diff ---\n{}",
                resolved.display(),
                unified_diff(&body, &updated)
            ))
        }
        "multi_edit" => {
            let path = str_field(input, "path")?;
            let edits = crate::tools_more::MultiEdit::parse_edits(input)?;
            let resolved = resolve_tool_path(ctx, path)?;
            let body = fs::read_to_string(&resolved)
                .map_err(|_| anyhow!("파일을 읽을 수 없습니다: {}", resolved.display()))?;
            let updated = crate::tools_more::MultiEdit::apply(&body, &edits)?;
            Ok(format!(
                "[승인] multi_edit ({})\npath: {}\n--- diff ---\n{}",
                edits.len(),
                resolved.display(),
                unified_diff(&body, &updated)
            ))
        }
        "bash" => {
            let command = str_field(input, "command")?;
            if let Some(why) = bash_blocked(command) {
                return Err(anyhow!("차단된 명령입니다 ({why})"));
            }
            Ok(format!("[승인] bash\ncommand: {command}"))
        }
        "apply_patch" => {
            let patch = str_field(input, "patch")?;
            let ops = crate::tools_more::ApplyPatch::parse(patch)?;
            let report = crate::tools_more::ApplyPatch::dry_run(ctx, &ops)?;
            Ok(format!(
                "[승인] apply_patch\n{report}\n--- patch ---\n{patch}"
            ))
        }
        other => Ok(format!("[승인] {other}")),
    }
}

fn str_field<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("{key} 인자가 필요합니다"))
}

struct ReadFile;
struct ListDir;
struct Grep;
struct EditFile;
struct WriteFile;
struct Bash;
struct ObsidianSearch;

impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "워크스페이스 안의 파일을 읽습니다. 큰 파일은 offset/limit(줄 단위)로 범위를 지정하세요. 출력 각 줄에는 N#HASH| 태그가 붙습니다 — 편집할 때 edit_file 의 anchors(start/end) 인자로 이 태그를 사용하세요."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "파일 경로"},
                "offset": {"type": "integer", "description": "시작 줄(1부터). 선택"},
                "limit": {"type": "integer", "description": "읽을 줄 수. 선택"}
            },
            "required": ["path"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let path = str_field(&input, "path")?;
        // 읽기는 쓰기 게이트를 통과하지 않는다 — 검증자·Executor 가 acceptance
        // 내용을 확인하는 것은 자유다. 쓰기 차단은 resolve_tool_path 가 담당.
        let resolved = resolve_in_workspace(&ctx.workspace, path)?;
        reject_vault(&resolved, ctx.vault.as_deref())?;
        if !resolved.is_file() {
            return Err(anyhow!("파일이 아닙니다: {}", resolved.display()));
        }
        let len = fs::metadata(&resolved)?.len();
        let offset = input.get("offset").and_then(|v| v.as_u64());
        let limit = input.get("limit").and_then(|v| v.as_u64());
        if len > MAX_FILE_BYTES && offset.is_none() && limit.is_none() {
            return Err(anyhow!(
                "파일이 256KB를 넘습니다 ({} bytes). offset과 limit으로 범위를 지정하세요.",
                len
            ));
        }
        let text = fs::read_to_string(&resolved)
            .map_err(|_| anyhow!("텍스트 파일이 아니거나 읽을 수 없습니다"))?;
        let lines: Vec<&str> = text.lines().collect();
        let start = offset.unwrap_or(1).max(1) as usize;
        let start_idx = start.saturating_sub(1).min(lines.len());
        let take = limit.map(|n| n as usize).unwrap_or(lines.len());
        let slice = &lines[start_idx..lines.len().min(start_idx + take)];
        let body = slice.join("\n");
        if ctx.hashline {
            return Ok(hashline::tag_lines(&body, start));
        }
        Ok(body)
    }
}

impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }
    fn description(&self) -> &'static str {
        "워크스페이스 안의 폴더 목록을 보여줍니다. 최대 500항목."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "폴더 경로. 기본값 ."}
            },
            "required": ["path"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let resolved = resolve_tool_path(ctx, path)?;
        if !resolved.is_dir() {
            return Err(anyhow!("폴더가 아닙니다: {}", resolved.display()));
        }
        let mut names: Vec<String> = fs::read_dir(&resolved)?
            .filter_map(|e| e.ok())
            .map(|e| {
                let mut n = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() {
                    n.push('/');
                }
                n
            })
            .collect();
        names.sort();
        let total = names.len();
        names.truncate(MAX_LIST_ITEMS);
        if total > MAX_LIST_ITEMS {
            names.push(format!("... ({total}개 중 {MAX_LIST_ITEMS}개만 표시)"));
        }
        Ok(names.join("\n"))
    }
}

impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "워크스페이스에서 정규식으로 파일을 검색합니다. 최대 200줄."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "정규식"},
                "path": {"type": "string", "description": "검색 시작 경로. 선택"},
                "glob": {"type": "string", "description": "파일 이름 글롭. 예: *.rs. 선택"},
                "context": {"type": "integer", "description": "일치 줄 앞뒤 문맥 줄 수(0-5). 기본 0"}
            },
            "required": ["pattern"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let pattern = str_field(&input, "pattern")?;
        let re = Regex::new(pattern).map_err(|e| anyhow!("정규식이 올바르지 않습니다: {e}"))?;
        let start = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let root = resolve_tool_path(ctx, start)?;
        let glob_re = match input.get("glob").and_then(|v| v.as_str()) {
            Some(g) if !g.is_empty() => Some(glob_to_regex(g)?),
            _ => None,
        };
        let context = input
            .get("context")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .clamp(0, 5) as usize;
        let mut hits = Vec::new();
        let walker = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(true)
            .build();
        for entry in walker.flatten() {
            if hits.len() >= MAX_GREP_LINES {
                break;
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Some(gr) = &glob_re {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !gr.is_match(name) {
                    continue;
                }
            }
            let Ok(text) = fs::read_to_string(path) else {
                continue;
            };
            let rel = path.strip_prefix(&ctx.workspace).unwrap_or(path);
            let lines: Vec<&str> = text.lines().collect();
            let matched: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, l)| re.is_match(l))
                .map(|(i, _)| i)
                .collect();
            if matched.is_empty() {
                continue;
            }
            if context == 0 {
                for i in matched
                    .iter()
                    .take(MAX_GREP_LINES.saturating_sub(hits.len()))
                {
                    hits.push(format!("{}:{}:{}", rel.display(), i + 1, lines[*i]));
                }
            } else {
                // 겹치는 범위를 병합해 블록으로 출력 (-- 구분자로 블록 분리)
                let mut ranges: Vec<(usize, usize)> = Vec::new();
                for &i in &matched {
                    match ranges.last_mut() {
                        Some(r) if i <= r.1 + 1 => {
                            r.1 = (i + context).min(lines.len().saturating_sub(1))
                        }
                        _ => ranges.push((
                            i.saturating_sub(context),
                            (i + context).min(lines.len().saturating_sub(1)),
                        )),
                    }
                }
                for (s, e) in ranges {
                    if hits.len() >= MAX_GREP_LINES {
                        break;
                    }
                    for (i, line) in lines.iter().enumerate().take(e + 1).skip(s) {
                        let mark = if matched.contains(&i) { ":" } else { "-" };
                        hits.push(format!("{}{}{}:{}", rel.display(), mark, i + 1, line));
                    }
                    hits.push("--".to_string());
                }
                if hits.last().map(|h| h == "--").unwrap_or(false) {
                    hits.pop();
                }
            }
        }
        if hits.is_empty() {
            Ok("(일치 없음)".to_string())
        } else {
            Ok(hits.join("\n"))
        }
    }
}

fn glob_to_regex(glob: &str) -> Result<Regex> {
    let mut pat = String::from("^");
    for c in glob.chars() {
        match c {
            '*' => pat.push_str(".*"),
            '?' => pat.push('.'),
            _ => pat.push_str(&regex::escape(&c.to_string())),
        }
    }
    pat.push('$');
    Regex::new(&pat).map_err(|e| anyhow!("{e}"))
}

impl Tool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }
    fn description(&self) -> &'static str {
        "파일을 바꿉니다. 두 모드: ① anchors(start/end에 read_file 의 N#HASH 태그) — 해시 검증 후 구간 교체(가장 정확) ② old_str/new_str — 파일 안에서 정확히 한 번만 나타나야 합니다."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_str": {"type": "string", "description": "모드②: 바꿀 원문 (anchors 없을 때 필수)"},
                "new_str": {"type": "string"},
                "anchors": {
                    "type": "object",
                    "properties": {
                        "start": {"type": "string", "description": "시작 앵커 (예: 12#abc)"},
                        "end": {"type": "string", "description": "끝 앵커 (예: 15#def)"}
                    },
                    "required": ["start", "end"],
                    "description": "모드①: read_file 의 N#HASH 태그 구간"
                }
            },
            "required": ["path", "new_str"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        true
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let path = str_field(&input, "path")?;
        let new_str = str_field(&input, "new_str")?;
        let resolved = resolve_tool_path(ctx, path)?;
        let body = fs::read_to_string(&resolved)
            .map_err(|_| anyhow!("파일을 읽을 수 없습니다: {}", resolved.display()))?;

        // 모드① 해시 앵커 — 검증 실패 시 아무것도 쓰지 않는다 (원자 거부).
        if let Some(anchors) = input.get("anchors") {
            let start = anchors.get("start").and_then(|v| v.as_str()).unwrap_or("");
            let end = anchors.get("end").and_then(|v| v.as_str()).unwrap_or("");
            match hashline::verify_span(&body, start, end) {
                Ok((s, e)) => {
                    let old_span = body.lines().skip(s).take(e - s + 1).collect::<Vec<_>>().join("\n");
                    let updated = hashline::replace_span(&body, s, e, new_str);
                    let mut plan = mutation::MutationPlan::new(&ctx.workspace)?;
                    plan.replace(
                        &resolved,
                        mutation::MutationState::Present(body.as_bytes().to_vec()),
                        updated.into_bytes(),
                    )?;
                    ctx.commit_mutation(plan)?;
                    hashline::record_metric(ctx, "edit_file", "ok:anchors");
                    return Ok(with_auto_diagnostics(
                        code_change_summary("수정", &resolved, &body, &old_span, new_str),
                        &resolved,
                        ctx,
                    ));
                }
                Err(e) => {
                    hashline::record_metric(ctx, "edit_file", "fail:hash_mismatch");
                    return Err(e);
                }
            }
        }

        // 모드② old_str (하위호환) — 실패 시 앵커 모드 힌트를 붙인다.
        let old_str = str_field(&input, "old_str")?;
        let count = body.matches(old_str).count();
        if count != 1 {
            hashline::record_metric(ctx, "edit_file", "fail:old_str_miss");
            return Err(anyhow!(
                "old_str 가 파일에서 {count}번 나타납니다. 정확히 1번이어야 합니다.\n{}",
                hashline::ANCHOR_HINT
            ));
        }
        hashline::record_metric(ctx, "edit_file", "ok:old_str");
        let updated = body.replacen(old_str, new_str, 1);
        let mut plan = mutation::MutationPlan::new(&ctx.workspace)?;
        plan.replace(
            &resolved,
            mutation::MutationState::Present(body.as_bytes().to_vec()),
            updated.into_bytes(),
        )?;
        ctx.commit_mutation(plan)?;
        Ok(with_auto_diagnostics(
            code_change_summary("수정", &resolved, &body, old_str, new_str),
            &resolved,
            ctx,
        ))
    }
}

impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "파일을 새로 만들거나 전체를 덮어씁니다."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        true
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let path = str_field(&input, "path")?;
        let content = str_field(&input, "content")?;
        let resolved = resolve_tool_path(ctx, path)?;
        let before = fs::read_to_string(&resolved).unwrap_or_default();
        let action = if resolved.exists() {
            "덮어쓰기"
        } else {
            "등록"
        };
        let before_state = mutation::read_state(&resolved)?;
        let mut plan = mutation::MutationPlan::new(&ctx.workspace)?;
        plan.replace(&resolved, before_state, content.as_bytes().to_vec())?;
        ctx.commit_mutation(plan)?;
        Ok(with_auto_diagnostics(
            code_change_summary(action, &resolved, &before, &before, content),
            &resolved,
            ctx,
        ))
    }
}

impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn description(&self) -> &'static str {
        "워크스페이스에서 명령을 실행합니다. 기본 타임아웃 60초(timeout_secs 로 최대 600초까지 조정). 위험 명령은 차단됩니다."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout_secs": {"type": "integer", "description": "타임아웃 초. 5-600. 기본 60"}
            },
            "required": ["command"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        true
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let command = str_field(&input, "command")?.to_string();
        if let Some(why) = bash_blocked(&command) {
            return Err(anyhow!("차단된 명령입니다 ({why})"));
        }
        let timeout_secs = input
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(BASH_TIMEOUT_SECS)
            .clamp(5, 600);
        let workspace = ctx.workspace.clone();
        let run = ctx.run.clone();
        let track_changes = run.is_some();
        let tracked = tokio::task::block_in_place(|| -> Result<_> {
            let snapshot = if track_changes {
                Some(
                    workspace_delta::WorkspaceSnapshot::capture(&workspace)
                        .map_err(|error| anyhow!("명령 실행 전 변경 추적 실패: {error}"))?,
                )
            } else {
                None
            };
            let result = tokio::runtime::Handle::current().block_on(run_bash(
                command,
                workspace,
                timeout_secs,
            ));
            let changes = match snapshot {
                Some(snapshot) => snapshot
                    .changed_baselines()
                    .map_err(|error| anyhow!("명령 실행 후 변경 추적 실패: {error}"))?,
                None => Vec::new(),
            };
            Ok((result, changes))
        });
        let (result, changes) = match tracked {
            Ok(tracked) => tracked,
            Err(error) => {
                if let Some(run) = &run {
                    run.mark_change_tracking_incomplete();
                }
                return Err(error);
            }
        };
        if let Some(run) = run {
            run.record_committed_changes(changes);
        }
        result
    }
}

impl Tool for ObsidianSearch {
    fn name(&self) -> &'static str {
        "obsidian_search"
    }
    fn description(&self) -> &'static str {
        "Obsidian Vault 노트를 FTS5로 검색합니다. 제목·경로·발췌를 반환합니다."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "검색어"},
                "limit": {"type": "integer", "description": "결과 개수. 기본 5"}
            },
            "required": ["query"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let query = str_field(&input, "query")?;
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .clamp(1, 20) as usize;
        let db = Db::open(&ctx.db_path)?;
        let hits = db.search_notes(query, limit)?;
        Ok(obsidian::format_tool_results(&hits))
    }
}

fn bash_blocked(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_lowercase();
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.contains(&"sudo") {
        return Some("sudo");
    }
    if compact.contains("rm-rf/") || compact.contains("rm-rf~") {
        return Some("rm -rf / 또는 ~");
    }
    if words.iter().any(|w| *w == "mkfs" || w.starts_with("mkfs.")) {
        return Some("mkfs");
    }
    if words.contains(&"dd") {
        return Some("dd");
    }
    if words.contains(&"shutdown") {
        return Some("shutdown");
    }
    if words.contains(&"reboot") {
        return Some("reboot");
    }
    if compact.contains("chmod-r777") || compact.contains("chmod-rf777") {
        return Some("chmod -R 777");
    }
    if (lower.contains("curl") || lower.contains("wget"))
        && (compact.contains("|sh") || compact.contains("|bash"))
    {
        return Some("curl | sh");
    }
    // /dev/null 리다이렉트는 무해하다 — 그 외 디바이스로의 쓰기만 막는다.
    if compact.replace(">/dev/null", "").contains(">/dev/") {
        return Some("> /dev/");
    }
    if compact.contains(":(){:|:&};:") || compact.contains(":(){:|&};:") {
        return Some("fork bomb");
    }
    None
}

async fn run_bash(command: String, workspace: PathBuf, timeout_secs: u64) -> Result<String> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", &command]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-c", &command]);
        c
    };
    cmd.current_dir(&workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .stdin(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("명령을 시작하지 못했습니다: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("명령 stdout 파이프를 열지 못했습니다"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("명령 stderr 파이프를 열지 못했습니다"))?;
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();

    let wait = timeout(Duration::from_secs(timeout_secs), async {
        let mut tmp_out = [0u8; 4096];
        let mut tmp_err = [0u8; 4096];
        let mut stdout_done = false;
        let mut stderr_done = false;
        loop {
            tokio::select! {
                n = stdout.read(&mut tmp_out), if !stdout_done => {
                    match n {
                        Ok(0) => stdout_done = true,
                        Ok(n) => out_buf.extend_from_slice(&tmp_out[..n]),
                        Err(_) => stdout_done = true,
                    }
                }
                n = stderr.read(&mut tmp_err), if !stderr_done => {
                    match n {
                        Ok(0) => stderr_done = true,
                        Ok(n) => err_buf.extend_from_slice(&tmp_err[..n]),
                        Err(_) => stderr_done = true,
                    }
                }
                else => break,
            }
            if out_buf.len() + err_buf.len() > MAX_BASH_OUTPUT * 2 {
                break;
            }
        }
        child.wait().await
    })
    .await;

    match wait {
        Err(_) => {
            let _ = child.kill().await;
            Err(anyhow!("명령이 {timeout_secs}초를 넘겨 중단되었습니다"))
        }
        Ok(Err(e)) => Err(anyhow!("명령 실행 오류: {e}")),
        Ok(Ok(status)) => {
            let mut combined = String::new();
            if !out_buf.is_empty() {
                combined.push_str(&String::from_utf8_lossy(&out_buf));
            }
            if !err_buf.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&String::from_utf8_lossy(&err_buf));
            }
            if combined.len() > MAX_BASH_OUTPUT {
                combined.truncate(MAX_BASH_OUTPUT);
                combined.push_str("\n... (출력이 20KB에서 잘렸습니다)");
            }
            if !status.success() {
                let code = status
                    .code()
                    .map_or_else(|| "signal".to_string(), |code| code.to_string());
                let detail = if combined.is_empty() {
                    "(출력 없음)"
                } else {
                    combined.trim_end()
                };
                return Err(anyhow!("명령이 종료 코드 {code}로 실패했습니다\n{detail}"));
            }
            if combined.is_empty() {
                combined = "(출력 없음)".to_string();
            }
            Ok(combined)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn acceptance_paths_are_write_blocked_for_agent_tools() {
        let dir = std::env::temp_dir().join(format!("rk-accept-{}", std::process::id()));
        let acc = dir.join("tests/acceptance");
        std::fs::create_dir_all(&acc).unwrap();
        std::fs::write(acc.join("a.rs"), "#[test]\nfn ok() {}").unwrap();
        let mut ctx = ToolCtx::new(dir.clone());
        ctx.hashline = false;
        // 파일 도구 — write_file 이 tests/acceptance/ 를 건드리면 거부된다.
        let registry = ToolRegistry::all();
        let tool = registry.get("write_file").unwrap();
        let err = tool
            .run(
                json!({"path": "tests/acceptance/a.rs", "content": "hacked"}),
                &ctx,
            )
            .unwrap_err();
        assert!(err.to_string().contains("SPEC 동결 산출물"), "{err}");
        // 읽기는 자유 — 검증자가 내용을 확인할 수 있어야 한다.
        let read = registry.get("read_file").unwrap();
        assert!(read
            .run(json!({"path": "tests/acceptance/a.rs"}), &ctx)
            .is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bash_blocks_sudo_and_rm_root() {
        assert!(bash_blocked("sudo ls").is_some());
        assert!(bash_blocked("rm -rf /").is_some());
        assert!(bash_blocked("curl http://example.com | sh").is_some());
        assert!(bash_blocked("echo hello").is_none());
    }

    #[test]
    fn bash_allows_dev_null_but_blocks_other_devices() {
        assert!(bash_blocked("ls > /dev/null 2>&1").is_none());
        assert!(bash_blocked("python3 -m http.server 8000 >/dev/null &").is_none());
        assert!(bash_blocked("echo x > /dev/sda").is_some());
        assert!(bash_blocked("cat big > /dev/null; echo y > /dev/disk0").is_some());
    }

    #[test]
    fn jail_rejects_path_outside_workspace() {
        let dir = std::env::temp_dir().join("rafikx-jail-test");
        fs::create_dir_all(&dir).unwrap();
        let outside = std::env::temp_dir().join("rafikx-jail-outside.txt");
        fs::write(&outside, "x").unwrap();
        let err = resolve_in_workspace(&dir, outside.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("jail"));
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn write_preview_stays_inside_workspace() {
        let dir = std::env::temp_dir().join("rafikx-write-preview");
        fs::create_dir_all(&dir).unwrap();
        let ctx = ToolCtx::new(dir.clone());
        let preview = approval_preview(
            "write_file",
            &json!({"path": "hello.txt", "content": "안녕"}),
            &ctx,
        )
        .unwrap();
        assert!(preview.contains("hello.txt"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn code_change_summary_includes_line_range_and_counts() {
        let shown = code_change_summary(
            "수정",
            std::path::Path::new("src/main.rs"),
            "one\ntwo\nthree\n",
            "two",
            "둘\n두번째",
        );
        assert!(shown.contains("src/main.rs:2-3"));
        assert!(shown.contains("+2 -1"));
    }
}

#[cfg(test)]
mod hashline_tool_tests {
    use super::*;
    use serde_json::json;

    fn setup(tag: &str) -> (PathBuf, ToolCtx) {
        let dir = std::env::temp_dir().join(format!("rafikx-hashline-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        (dir.clone(), ToolCtx::new(dir))
    }

    #[test]
    fn read_file_tags_and_edit_file_anchor_edits() {
        let (dir, ctx) = setup("anchor-ok");
        fs::write(dir.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();

        let tagged = ReadFile.run(json!({"path": "a.txt"}), &ctx).unwrap();
        let anchor1 = tagged.lines().nth(1).unwrap().split('|').next().unwrap().to_string();
        assert!(anchor1.contains('#'));

        EditFile
            .run(
                json!({"path": "a.txt", "anchors": {"start": anchor1, "end": anchor1}, "new_str": "BETA"}),
                &ctx,
            )
            .unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "alpha\nBETA\ngamma\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn anchor_mismatch_rejects_atomically() {
        let (dir, ctx) = setup("anchor-stale");
        fs::write(dir.join("a.txt"), "alpha\nbeta\n").unwrap();
        let err = EditFile
            .run(
                json!({"path": "a.txt", "anchors": {"start": "1#zzz", "end": "1#zzz"}, "new_str": "X"}),
                &ctx,
            )
            .unwrap_err();
        assert!(err.to_string().contains("파일이 바뀌었습니다"));
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "alpha\nbeta\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn old_str_miss_carries_anchor_hint() {
        let (dir, ctx) = setup("oldstr-hint");
        fs::write(dir.join("a.txt"), "dup\ndup\n").unwrap();
        let err = EditFile
            .run(json!({"path": "a.txt", "old_str": "dup", "new_str": "X"}), &ctx)
            .unwrap_err();
        assert!(err.to_string().contains("anchors"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn hashline_off_keeps_legacy_read() {
        let (dir, mut ctx) = setup("legacy-read");
        ctx.hashline = false;
        fs::write(dir.join("a.txt"), "plain\n").unwrap();
        let out = ReadFile.run(json!({"path": "a.txt"}), &ctx).unwrap();
        assert_eq!(out, "plain");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn multi_edit_anchor_op_edits_span() {
        let (dir, ctx) = setup("multi-anchor");
        fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let tagged = ReadFile.run(json!({"path": "a.txt"}), &ctx).unwrap();
        let a1 = tagged.lines().next().unwrap().split('|').next().unwrap().to_string();
        MultiEdit
            .run(
                json!({"path": "a.txt", "edits": [{"anchors": {"start": a1, "end": a1}, "new_str": "ONE"}]}),
                &ctx,
            )
            .unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "ONE\ntwo\nthree\n");
        let _ = fs::remove_dir_all(dir);
    }
}
