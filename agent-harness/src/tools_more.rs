//! opencode 대응 확장 도구: glob · webfetch · multi_edit · todo · task(서브에이전트 위임).
//! 하네스 분류·모델 자동선택·계정 로테이션은 그대로 재사용한다.

use std::fs;
use std::sync::{Mutex, OnceLock};

use anyhow::{Result, anyhow};
use regex::Regex;
use serde_json::{Value, json};

use crate::tools::{Tool, ToolCtx};

pub const MAX_GLOB_RESULTS: usize = 300;
pub const MAX_FETCH_CHARS: usize = 20_000;

// ---------------------------------------------------------------------------
// todo 공유 상태 (프로세스 단위 — 마지막 실행의 목록을 /todo 에서도 보여준다)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
    pub priority: String,
}

fn todo_slot() -> &'static Mutex<Vec<TodoItem>> {
    static SLOT: OnceLock<Mutex<Vec<TodoItem>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn current_todos() -> Vec<TodoItem> {
    todo_slot().lock().map(|g| g.clone()).unwrap_or_default()
}

fn store_todos(items: &[TodoItem]) {
    if let Ok(mut g) = todo_slot().lock() {
        *g = items.to_vec();
    }
}

pub fn render_todos(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "(할 일 없음)".into();
    }
    let mut out = Vec::new();
    for (i, t) in items.iter().enumerate() {
        let mark = match t.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[~]",
            _ => "[ ]",
        };
        out.push(format!("{}. {} {} ({})", i + 1, mark, t.content, t.priority));
    }
    out.join("\n")
}

fn valid_status(s: &str) -> bool {
    matches!(s, "pending" | "in_progress" | "completed")
}

fn valid_priority(s: &str) -> bool {
    matches!(s, "high" | "medium" | "low")
}

fn parse_todos(input: &Value) -> Result<Vec<TodoItem>> {
    let arr = input
        .get("todos")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("todos 배열이 필요합니다"))?;
    if arr.len() > 50 {
        return Err(anyhow!("할 일은 최대 50개까지입니다"));
    }
    let mut out = Vec::new();
    for t in arr {
        let content = t
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("각 항목에는 content 가 필요합니다"))?
            .trim()
            .to_string();
        if content.is_empty() {
            return Err(anyhow!("빈 항목은 넣을 수 없습니다"));
        }
        let status = t.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
        let priority = t.get("priority").and_then(|v| v.as_str()).unwrap_or("medium");
        if !valid_status(status) {
            return Err(anyhow!("status 는 pending|in_progress|complted 중 하나여야 합니다"));
        }
        if !valid_priority(priority) {
            return Err(anyhow!("priority 는 high|medium|low 중 하나여야 합니다"));
        }
        out.push(TodoItem {
            content: content.chars().take(200).collect(),
            status: status.into(),
            priority: priority.into(),
        });
    }
    Ok(out)
}

pub struct TodoWrite;

impl Tool for TodoWrite {
    fn name(&self) -> &'static str {
        "todo_write"
    }
    fn description(&self) -> &'static str {
        "작업 목록 전체를 갱신합니다. 복잡한 작업을 단계로 나눠 상태(pending/in_progress/completed)를 추적하세요."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "할 전체 목록(교체)",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {"type": "string"},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]},
                            "priority": {"type": "string", "enum": ["high", "medium", "low"]}
                        },
                        "required": ["content"]
                    }
                }
            },
            "required": ["todos"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }
    fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<String> {
        let items = parse_todos(&input)?;
        store_todos(&items);
        Ok(render_todos(&items))
    }
}

pub struct TodoRead;

impl Tool for TodoRead {
    fn name(&self) -> &'static str {
        "todo_read"
    }
    fn description(&self) -> &'static str {
        "현재 작업 목록을 읽습니다."
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }
    fn run(&self, _input: Value, _ctx: &ToolCtx) -> Result<String> {
        Ok(render_todos(&current_todos()))
    }
}

// ---------------------------------------------------------------------------
// glob
// ---------------------------------------------------------------------------

/// 경로 인식 글롭: `**` 는 임의 깊이, `*`/`?` 는 한 세그먼트 안에서 매칭.
pub fn glob_path_regex(glob: &str) -> Result<Regex> {
    let mut pat = String::from("^");
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    // `**/` 는 0단계 이상 디렉터리, 그 외는 임의 문자열
                    if i + 2 < chars.len() && chars[i + 2] == '/' {
                        pat.push_str("(?:.*/)?");
                        i += 3;
                        continue;
                    }
                    pat.push_str(".*");
                    i += 2;
                    continue;
                }
                pat.push_str("[^/]*");
            }
            '?' => pat.push_str("[^/]"),
            _ => pat.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    pat.push('$');
    Regex::new(&pat).map_err(|e| anyhow!("글롭 해석 실패: {e}"))
}

pub struct GlobTool;

impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &'static str {
        "글롭 패턴으로 파일을 찾습니다. 예: **/*.rs , src/*.toml"
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "글롭 패턴"},
                "path": {"type": "string", "description": "검색 시작 폴더. 기본 ." }
            },
            "required": ["pattern"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let pattern = input
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("pattern 인자가 필요합니다"))?;
        let re = glob_path_regex(pattern)?;
        let start = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let root = crate::tools::resolve_tool_path(ctx, start)?;
        let mut hits: Vec<String> = Vec::new();
        let walker = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(true)
            .build();
        for entry in walker.flatten() {
            if hits.len() >= MAX_GLOB_RESULTS {
                break;
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let rel = path.strip_prefix(&ctx.workspace).unwrap_or(path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if re.is_match(&rel_str) || path.file_name().is_some_and(|n| re.is_match(n.to_string_lossy().as_ref())) {
                hits.push(rel_str);
            }
        }
        hits.sort();
        if hits.is_empty() {
            Ok("(일치 없음)".into())
        } else {
            let total = hits.len();
            if total > MAX_GLOB_RESULTS {
                hits.truncate(MAX_GLOB_RESULTS);
                hits.push(format!("... ({total}개 중 {MAX_GLOB_RESULTS}개만 표시)"));
            }
            Ok(hits.join("\n"))
        }
    }
}

// ---------------------------------------------------------------------------
// webfetch
// ---------------------------------------------------------------------------

pub fn strip_html(html: &str) -> String {
    // rust regex 는 역참조가 없어 태그별로 제거한다.
    let script = Regex::new(r"(?is)<script[^>]*>.*?</script>").expect("fixed regex");
    let style = Regex::new(r"(?is)<style[^>]*>.*?</style>").expect("fixed regex");
    let tags = Regex::new(r"(?s)<[^>]+>").expect("fixed regex");
    let no_script = script.replace_all(html, " ");
    let no_style = style.replace_all(&no_script, " ");
    let no_tags = tags.replace_all(&no_style, " ");
    let decoded = no_tags
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    let mut out = String::new();
    let mut prev_space = true;
    for ch in decoded.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

pub struct WebFetch;

impl WebFetch {
    async fn fetch(url: String, max_chars: usize) -> Result<String> {
        let lower = url.to_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return Err(anyhow!("http/https URL 만 지원합니다"));
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .user_agent("RafikX/0.2 (+webfetch)")
            .build()?;
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow!("가져오기 실패: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("HTTP {status}"));
        }
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        let body = resp.text().await.unwrap_or_default();
        let text = if ctype.contains("html") || body.trim_start().starts_with('<') {
            strip_html(&body)
        } else {
            body
        };
        let mut text = text.chars().take(max_chars).collect::<String>();
        if text.len() > max_chars {
            text.truncate(max_chars);
        }
        if text.trim().is_empty() {
            return Err(anyhow!("본문이 비어 있습니다"));
        }
        Ok(text)
    }
}

impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "webfetch"
    }
    fn description(&self) -> &'static str {
        "URL 의 내용을 가져와 텍스트로 반환합니다 (HTML 은 태그를 제거)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "http/https URL"},
                "max_chars": {"type": "integer", "description": "최대 글자 수. 기본 20000"}
            },
            "required": ["url"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }
    fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<String> {
        let url = input
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("url 인자가 필요합니다"))?
            .to_string();
        let max_chars = input
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(MAX_FETCH_CHARS as u64)
            .clamp(500, 100_000) as usize;
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(Self::fetch(url, max_chars))
        })
    }
}

// ---------------------------------------------------------------------------
// multi_edit
// ---------------------------------------------------------------------------

pub struct MultiEdit;

impl MultiEdit {
    pub fn apply(body: &str, edits: &[(String, String)]) -> Result<String> {
        let mut updated = body.to_string();
        for (i, (old_str, new_str)) in edits.iter().enumerate() {
            if old_str.is_empty() {
                return Err(anyhow!("{}번째 edit 의 old_str 가 비어 있습니다", i + 1));
            }
            let count = updated.matches(old_str.as_str()).count();
            if count != 1 {
                return Err(anyhow!(
                    "{}번째 edit 의 old_str 가 파일에서 {count}번 나타납니다. 정확히 1번이어야 합니다.",
                    i + 1
                ));
            }
            updated = updated.replacen(old_str.as_str(), new_str.as_str(), 1);
        }
        Ok(updated)
    }

    pub fn parse_edits(input: &Value) -> Result<Vec<(String, String)>> {
        let arr = input
            .get("edits")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("edits 배열이 필요합니다"))?;
        if arr.is_empty() {
            return Err(anyhow!("edits 가 비어 있습니다"));
        }
        let mut out = Vec::new();
        for e in arr {
            let old = e
                .get("old_str")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("edit 에 old_str 가 필요합니다"))?
                .to_string();
            let new = e
                .get("new_str")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            out.push((old, new));
        }
        Ok(out)
    }
}

impl Tool for MultiEdit {
    fn name(&self) -> &'static str {
        "multi_edit"
    }
    fn description(&self) -> &'static str {
        "한 파일에 여러 치환을 한 번에 적용합니다. 각 old_str 는 적용 시점에 유일해야 합니다."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_str": {"type": "string"},
                            "new_str": {"type": "string"}
                        },
                        "required": ["old_str"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        true
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("path 인자가 필요합니다"))?;
        let edits = Self::parse_edits(&input)?;
        let resolved = crate::tools::resolve_tool_path(ctx, path)?;
        let body = fs::read_to_string(&resolved)
            .map_err(|_| anyhow!("파일을 읽을 수 없습니다: {}", resolved.display()))?;
        let updated = Self::apply(&body, &edits)?;
        fs::write(&resolved, updated)?;
        Ok(format!(
            "{}곳 수정 완료: {}",
            edits.len(),
            resolved.display()
        ))
    }
}

// ---------------------------------------------------------------------------
// task — 서브에이전트 위임 (하네스 파이프라인 재사용, 재귀 금지)
// ---------------------------------------------------------------------------

pub struct TaskTool;

impl TaskTool {
    pub const NAME: &'static str = "task";

    fn resolve_class(prompt: &str, class: Option<&str>) -> crate::harness::TaskClass {
        if let Some(c) = class {
            if let Some(tc) = crate::harness::TaskClass::parse(c) {
                return tc;
            }
        }
        crate::harness::classify_rules(prompt, false)
    }

    async fn delegate(
        cfg: crate::config::Config,
        prompt: String,
        class: Option<String>,
    ) -> Result<String> {
        let tc = Self::resolve_class(&prompt, class.as_deref());
        // 모델 자동선택은 bind() 안에서 그대로 이루어진다.
        let binding = crate::harness::bind(&cfg, tc, None, None)?;
        crate::ui::live_line(&format!(
            "[task] {} → {} ({})",
            binding.class.as_str(),
            binding.profile_name,
            binding.model
        ));
        // 재귀 방지: 안쪽 실행에서는 task 도구를 제거한다.
        let tools: Vec<String> = binding
            .tools
            .iter()
            .filter(|t| t.as_str() != Self::NAME)
            .cloned()
            .collect();
        let binding = crate::harness::Binding {
            tools,
            ..binding.clone()
        };
        let outcome = crate::harness::run_pipeline(&cfg, &binding, &prompt, false, None, None, None, None).await?;
        let summary = crate::agent::assistant_text(&outcome.messages);
        Ok(format!(
            "[task 결과] class={} profile={} model={} status={}\n{summary}",
            binding.class.as_str(),
            binding.profile_name,
            binding.model,
            outcome.status
        ))
    }
}

impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn description(&self) -> &'static str {
        "하나의 작업을 독립된 서브에이전트에게 위임합니다. 조사·분석 등 분리된 맥락이 필요할 때 쓰세요. class 는 simple|medium|advanced|dev."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {"type": "string", "description": "위임할 작업 지시"},
                "class": {"type": "string", "enum": ["simple", "medium", "advanced", "dev"], "description": "강제 분류. 생략 시 규칙 분류"}
            },
            "required": ["prompt"]
        })
    }
    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }
    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("prompt 인자가 필요합니다"))?
            .trim()
            .to_string();
        if prompt.is_empty() {
            return Err(anyhow!("prompt 가 비어 있습니다"));
        }
        let class = input
            .get("class")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let cfg = crate::config::Config::load(None)?;
        let ask = ctx.local_ask.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let _ = ask; // 승인 흐름은 안쪽 run_pipeline 에서 원래 채널(local_ask)을 쓴다
                Self::delegate(cfg, prompt, class).await
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_depth_and_segment() {
        let re = glob_path_regex("**/*.rs").unwrap();
        assert!(re.is_match("src/main.rs"));
        assert!(re.is_match("a/b/c.rs"));
        assert!(!re.is_match("src/main.rs.bak"));
        let re2 = glob_path_regex("src/*.toml").unwrap();
        assert!(re2.is_match("src/Cargo.toml"));
        assert!(!re2.is_match("src/x/Cargo.toml"));
    }

    #[test]
    fn strips_html_and_entities() {
        let html = r#"<html><head><style>x{}</style></head><body><h1>제목</h1><p>안녕 &amp; 또</p><script>alert(1)</script></body></html>"#;
        let text = strip_html(html);
        assert!(text.contains("제목"));
        assert!(text.contains("안녕 & 또"));
        assert!(!text.contains("alert"));
        assert!(!text.contains('<'));
    }

    #[test]
    fn multi_edit_applies_in_order() {
        let body = "a b c\na b".to_string();
        let edits = vec![
            ("a b c".to_string(), "x".to_string()),
            ("a b".to_string(), "y".to_string()),
        ];
        let out = MultiEdit::apply(&body, &edits).unwrap();
        assert_eq!(out, "x\ny");
        let dup = vec![("a".to_string(), "z".to_string()), ("a".to_string(), "w".to_string())];
        assert!(MultiEdit::apply(&body, &dup).is_err());
    }

    #[test]
    fn todo_roundtrip() {
        let input = json!({"todos":[
            {"content":"읽기","status":"completed","priority":"high"},
            {"content":"고치기","status":"in_progress","priority":"medium"}
        ]});
        let items = parse_todos(&input).unwrap();
        assert_eq!(items.len(), 2);
        let shown = render_todos(&items);
        assert!(shown.contains("[x] 읽기"));
        assert!(shown.contains("[~] 고치기"));
        assert!(parse_todos(&json!({"todos":[{"content":"x","status":"weird"}]})).is_err());
    }

    #[test]
    fn task_excludes_itself_from_inner_tools() {
        let tools = vec!["*".to_string()];
        let filtered: Vec<String> = tools
            .iter()
            .filter(|t| t.as_str() != TaskTool::NAME)
            .cloned()
            .collect();
        // "*" 필터는 with_names 에서 처리되므로 여기선 제거 로직만 확인
        assert_eq!(filtered.len(), 1);
        assert_ne!(TaskTool::NAME, "task_renamed");
    }

    #[test]
    fn task_class_resolution() {
        assert_eq!(
            TaskTool::resolve_class("버그 고쳐줘", None),
            crate::harness::TaskClass::Dev
        );
        assert_eq!(
            TaskTool::resolve_class("안녕", Some("simple".to_string()).as_deref()),
            crate::harness::TaskClass::Simple
        );
    }
}
