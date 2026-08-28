//! facts 메모리 도구 3종 — 에이전트가 대화 중 지속 사실을 능동 기록·조회·삭제.
//!
//! remember/recall 은 읽기·기록이라 승인 없이 실행되고, forget 은 삭제라
//! mutation 으로 분류해 기존 승인 게이트를 탄다.

use anyhow::Result;
use serde_json::{Value, json};

use crate::db::Db;
use crate::tools::{Tool, ToolCtx};

fn open_db() -> Result<Db> {
    Db::open(&Db::db_path()?)
}

pub struct Remember;

impl Tool for Remember {
    fn name(&self) -> &'static str {
        "remember"
    }

    fn description(&self) -> &'static str {
        "사용자·프로젝트의 지속 사실(스택, 선호, 관습, 환경)을 기억한다. 사용자가 알려준 지속 사실이나 프로젝트의 스택/관습을 발견하면 즉시 호출한다. 일회성 작업 지시·임시 상태는 기록하지 않는다. 같은 키는 값이 갱신된다."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "key": {"type": "string", "description": "사실의 이름 (예: package-manager, 답변-언어)"},
                "value": {"type": "string", "description": "사실의 내용 (예: pnpm, 한국어)"},
                "kind": {"type": "string", "enum": ["stack", "preference", "convention", "env", "goal", "other"], "description": "분류 (기본 other)"},
                "global": {"type": "boolean", "description": "true면 모든 프로젝트 공통 사실로 기록 (기본 false=현재 프로젝트)"}
            },
            "required": ["key", "value"]
        })
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let key = input.get("key").and_then(Value::as_str).unwrap_or("").trim();
        let value = input.get("value").and_then(Value::as_str).unwrap_or("").trim();
        if key.is_empty() || value.is_empty() {
            return Ok("key와 value가 필요합니다. 기록하지 않았습니다.".into());
        }
        let kind = input.get("kind").and_then(Value::as_str).unwrap_or("other");
        let global = input.get("global").and_then(Value::as_bool).unwrap_or(false);
        let db = open_db()?;
        let scope: Option<&std::path::Path> = if global { None } else { Some(&ctx.workspace) };
        let write = db.upsert_fact(scope, kind, key, value, "agent")?;
        let verb = match write {
            crate::db::FactWrite::Inserted { .. } => "기록했습니다",
            crate::db::FactWrite::Updated { .. } => "갱신했습니다",
        };
        let scope_label = if global { "전역" } else { "프로젝트" };
        Ok(format!("{verb} ({scope_label} 사실): {key} = {value}"))
    }
}

pub struct Recall;

impl Tool for Recall {
    fn name(&self) -> &'static str {
        "recall"
    }

    fn description(&self) -> &'static str {
        "기억한 지속 사실을 검색한다. 사용자의 선호·프로젝트 스택·관습이 기억나지 않을 때, 또는 이전에 기록했는지 확인할 때 호출한다."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "검색어 (비우면 전체 목록)"},
                "kind": {"type": "string", "enum": ["stack", "preference", "convention", "env", "goal", "other"], "description": "분류 필터"}
            }
        })
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let query = input.get("query").and_then(Value::as_str).unwrap_or("");
        let kind = input.get("kind").and_then(Value::as_str);
        let db = open_db()?;
        let rows = db.recall_facts(Some(&ctx.workspace), query, kind, 10)?;
        if rows.is_empty() {
            return Ok("기억하는 사실이 없습니다.".into());
        }
        let mut out = String::from("[기억 검색 결과]\n");
        for r in rows {
            let scope = if r.project_id.is_empty() { "전역" } else { "프로젝트" };
            out.push_str(&format!("- ({kind}·{scope}) {key}: {value}\n", kind = r.kind, scope = scope, key = r.key, value = r.value));
        }
        Ok(out)
    }
}

pub struct Forget;

impl Tool for Forget {
    fn name(&self) -> &'static str {
        "forget"
    }

    fn description(&self) -> &'static str {
        "더 이상 맞지 않는 지속 사실을 지운다. 삭제 전에 기존 값을 보여주고, 사용자 승인 후 실행된다."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "key": {"type": "string", "description": "지울 사실의 이름"},
                "global": {"type": "boolean", "description": "true면 전역 사실에서 삭제 (기본 false=현재 프로젝트)"}
            },
            "required": ["key"]
        })
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        true
    }

    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let key = input.get("key").and_then(Value::as_str).unwrap_or("").trim();
        if key.is_empty() {
            return Ok("key가 필요합니다.".into());
        }
        let global = input.get("global").and_then(Value::as_bool).unwrap_or(false);
        let db = open_db()?;
        let scope: Option<&std::path::Path> = if global { None } else { Some(&ctx.workspace) };
        match db.forget_fact(scope, key)? {
            Some(row) => Ok(format!("삭제했습니다: {} = {} ({}·{})", row.key, row.value, row.kind, row.source)),
            None => Ok(format!("해당 키를 찾지 못했습니다: {key}")),
        }
    }
}
