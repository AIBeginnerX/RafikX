use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::{Value, json};

use crate::lifecycle::{LifecycleEventData, LifecycleState};
use crate::run::{AgentId, RunContext, RunId};

use super::{Tool, ToolCtx};

pub struct TaskTool;

#[derive(Debug, Clone, Serialize)]
pub struct TaskResult {
    pub run_id: RunId,
    pub parent_run_id: Option<RunId>,
    pub agent_id: AgentId,
    pub class: String,
    pub profile: String,
    pub model: String,
    pub status: String,
    pub state: LifecycleState,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl TaskTool {
    pub const NAME: &'static str = "task";

    pub(crate) fn resolve_class(prompt: &str, class: Option<&str>) -> crate::harness::TaskClass {
        class
            .and_then(crate::harness::TaskClass::parse)
            .unwrap_or_else(|| crate::harness::classify_rules(prompt, false))
    }

    async fn delegate(
        cfg: crate::config::Config,
        prompt: String,
        class: Option<String>,
        role: Option<String>,
        model: Option<String>,
        parent: Option<RunContext>,
        remote: Option<crate::agent::RemoteApproval>,
        local_ask: Option<crate::agent::LocalAsk>,
    ) -> Result<String> {
        let task_class = Self::resolve_class(&prompt, class.as_deref());
        let binding = crate::harness::bind(&cfg, task_class, None, model.as_deref())?;
        let nonce = crate::db::Db::new_id();
        let agent_id = AgentId::new(format!("agent-{nonce}"));
        let child_run_id = parent
            .as_ref()
            .map(|run| RunId::new(format!("{}:child-{nonce}", run.run_id())))
            .unwrap_or_else(|| RunId::new(format!("task-{nonce}")));
        let role = role.unwrap_or_else(|| binding.profile_name.clone());
        let child = parent
            .as_ref()
            .map(|run| run.child(child_run_id.clone(), agent_id.clone()))
            .unwrap_or_else(|| RunContext::isolated(child_run_id.clone(), cfg.workspace.clone()));
        if let Some(parent) = &parent {
            let _ = parent.transition_lifecycle(LifecycleEventData::ChildStarted {
                child_run_id: child_run_id.clone(),
                agent_id: agent_id.clone(),
            });
        }
        crate::ui::live_agent_in(
            &child,
            crate::ui::AgentProgress {
                id: agent_id.to_string(),
                role: role.clone(),
                model: binding.model.clone(),
                status: "running".into(),
            },
        );
        crate::ui::live_line_in(
            &child,
            &format!(
                "[task] {} · {} → {} ({})",
                agent_id,
                binding.class.as_str(),
                role,
                binding.model
            ),
        );
        let tools = binding
            .tools
            .iter()
            .filter(|tool| tool.as_str() != Self::NAME)
            .cloned()
            .collect();
        let binding = crate::harness::Binding {
            tools,
            ..binding.clone()
        };
        let result = crate::harness::run_pipeline_with_context(
            &cfg,
            &binding,
            &prompt,
            child.run_tree_approved(),
            None,
            None,
            remote,
            local_ask,
            child.clone(),
        )
        .await;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                finish_parent(&parent, &child, 0, 0);
                crate::ui::live_agent_in(
                    &child,
                    crate::ui::AgentProgress {
                        id: agent_id.to_string(),
                        role,
                        model: binding.model.clone(),
                        status: "failed".into(),
                    },
                );
                return Err(error);
            }
        };
        if let Some(parent) = &parent {
            parent.record_committed_paths(child.committed_paths());
        }
        finish_parent(&parent, &child, outcome.input_tokens, outcome.output_tokens);
        crate::ui::live_agent_in(
            &child,
            crate::ui::AgentProgress {
                id: agent_id.to_string(),
                role,
                model: binding.model.clone(),
                status: outcome.status.clone(),
            },
        );
        let state = child.lifecycle_state().unwrap_or(LifecycleState::Failed);
        let metadata = TaskResult {
            run_id: child_run_id,
            parent_run_id: parent.as_ref().map(|run| run.run_id().clone()),
            agent_id,
            class: binding.class.as_str().into(),
            profile: binding.profile_name.clone(),
            model: binding.model.clone(),
            status: outcome.status.clone(),
            state,
            input_tokens: outcome.input_tokens,
            output_tokens: outcome.output_tokens,
        };
        let summary = crate::agent::assistant_text(&outcome.messages);
        Ok(format!(
            "[task 결과] class={} profile={} model={} status={}\n[task 메타] {}\n{summary}",
            metadata.class,
            metadata.profile,
            metadata.model,
            metadata.status,
            serde_json::to_string(&metadata)?
        ))
    }
}

fn finish_parent(parent: &Option<RunContext>, child: &RunContext, input: u32, output: u32) {
    if let Some(parent) = parent {
        let _ = parent.transition_lifecycle(LifecycleEventData::ChildFinished {
            child_run_id: child.run_id().clone(),
            state: child.lifecycle_state().unwrap_or(LifecycleState::Failed),
            input_tokens: input,
            output_tokens: output,
        });
    }
}

impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn description(&self) -> &'static str {
        "독립된 작업만 서브에이전트에게 위임합니다. role과 필요한 경우 model을 명시하세요. 불필요한 위임은 토큰을 낭비합니다."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {"type": "string", "description": "위임할 작업 지시"},
                "class": {"type": "string", "enum": ["simple", "medium", "advanced", "dev"], "description": "강제 분류. 생략 시 규칙 분류"},
                "role": {"type": "string", "description": "화면에 표시할 짧은 역할 이름"},
                "model": {"type": "string", "description": "등록된 모델 ID. 생략하면 하네스가 능력과 비용에 따라 선택"}
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
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("prompt 인자가 필요합니다"))?
            .trim()
            .to_string();
        if prompt.is_empty() {
            return Err(anyhow!("prompt 가 비어 있습니다"));
        }
        let string = |key| input.get(key).and_then(Value::as_str).map(str::to_string);
        let cfg = ctx
            .run
            .as_ref()
            .and_then(RunContext::config)
            .map(|config| config.as_ref().clone())
            .map(Ok)
            .unwrap_or_else(|| crate::config::Config::load(None))?;
        let parent = ctx.run.clone();
        let remote = ctx.remote.clone();
        let local_ask = ctx.local_ask.clone();
        let class = string("class");
        let role = string("role");
        let model = string("model");
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                Self::delegate(cfg, prompt, class, role, model, parent, remote, local_ask).await
            })
        })
    }
}
