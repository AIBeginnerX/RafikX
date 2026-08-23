use std::fs;
use std::io::BufRead;
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
}

impl ToolCtx {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            vault: None,
            db_path: PathBuf::from("."),
        }
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
                Box::new(EditFile),
                Box::new(WriteFile),
                Box::new(Bash),
                Box::new(ObsidianSearch),
            ],
        }
    }

    pub fn with_names(names: &[String]) -> Self {
        if names.iter().any(|n| n == "*") {
            return Self::all();
        }
        let all = Self::all();
        Self {
            tools: all
                .tools
                .into_iter()
                .filter(|t| names.iter().any(|n| n == t.name()))
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
        self.tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
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

pub fn resolve_tool_path(ctx: &ToolCtx, user_path: &str) -> Result<PathBuf> {
    let resolved = resolve_in_workspace(&ctx.workspace, user_path)?;
    reject_vault(&resolved, ctx.vault.as_deref())?;
    Ok(resolved)
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
        "bash" => {
            let command = str_field(input, "command")?;
            if let Some(why) = bash_blocked(command) {
                return Err(anyhow!("차단된 명령입니다 ({why})"));
            }
            Ok(format!("[승인] bash\ncommand: {command}"))
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
        "워크스페이스 안의 파일을 읽습니다. 큰 파일은 offset/limit(줄 단위)로 범위를 지정하세요."
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
        let resolved = resolve_tool_path(ctx, path)?;
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
        Ok(slice.join("\n"))
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
                "glob": {"type": "string", "description": "파일 이름 글롭. 예: *.rs. 선택"}
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
        let mut hits = Vec::new();
        let walker = ignore::WalkBuilder::new(&root).hidden(false).git_ignore(true).build();
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
            let Ok(file) = fs::File::open(path) else { continue };
            let reader = std::io::BufReader::new(file);
            for (i, line) in reader.lines().enumerate() {
                if hits.len() >= MAX_GREP_LINES {
                    break;
                }
                let Ok(line) = line else { continue };
                if re.is_match(&line) {
                    let rel = path.strip_prefix(&ctx.workspace).unwrap_or(path);
                    hits.push(format!("{}:{}:{line}", rel.display(), i + 1));
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
        "파일에서 old_str 를 new_str 로 바꿉니다. old_str 는 파일 안에서 정확히 한 번만 나타나야 합니다."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_str": {"type": "string"},
                "new_str": {"type": "string"}
            },
            "required": ["path", "old_str", "new_str"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        true
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let path = str_field(&input, "path")?;
        let old_str = str_field(&input, "old_str")?;
        let new_str = str_field(&input, "new_str")?;
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
        fs::write(&resolved, updated)?;
        Ok(format!("수정 완료: {}", resolved.display()))
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
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&resolved, content)?;
        Ok(format!("저장 완료: {}", resolved.display()))
    }
}

impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn description(&self) -> &'static str {
        "워크스페이스에서 명령을 실행합니다. 타임아웃 60초. 위험 명령은 차단됩니다."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"}
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
        let workspace = ctx.workspace.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(run_bash(command, workspace))
        })
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
    if words.iter().any(|w| *w == "sudo") {
        return Some("sudo");
    }
    if compact.contains("rm-rf/") || compact.contains("rm-rf~") {
        return Some("rm -rf / 또는 ~");
    }
    if words.iter().any(|w| *w == "mkfs" || w.starts_with("mkfs.")) {
        return Some("mkfs");
    }
    if words.iter().any(|w| *w == "dd") {
        return Some("dd");
    }
    if words.iter().any(|w| *w == "shutdown") {
        return Some("shutdown");
    }
    if words.iter().any(|w| *w == "reboot") {
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
    if compact.contains(">/dev/") {
        return Some("> /dev/");
    }
    if compact.contains(":(){:|:&};:") || compact.contains(":(){:|&};:") {
        return Some("fork bomb");
    }
    None
}

async fn run_bash(command: String, workspace: PathBuf) -> Result<String> {
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

    let mut child = cmd.spawn().map_err(|e| anyhow!("명령을 시작하지 못했습니다: {e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();

    let wait = timeout(Duration::from_secs(BASH_TIMEOUT_SECS), async {
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
            Err(anyhow!("명령이 60초를 넘겨 중단되었습니다"))
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
                combined.push_str(&format!("\n[exit {}]", status.code().unwrap_or(-1)));
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
    fn bash_blocks_sudo_and_rm_root() {
        assert!(bash_blocked("sudo ls").is_some());
        assert!(bash_blocked("rm -rf /").is_some());
        assert!(bash_blocked("curl http://example.com | sh").is_some());
        assert!(bash_blocked("echo hello").is_none());
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
}

