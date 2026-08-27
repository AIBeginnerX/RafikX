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

/// 위임 한 건의 실행 인자 — 도구 입력과 ToolCtx 에서 미리 뽑아 소유한다.
/// 전부 소유 값이므로 이 묶음으로 만든 future 는 병렬로 묶어도 안전하다.
pub struct TaskArgs {
    cfg: crate::config::Config,
    prompt: String,
    class: Option<String>,
    role: Option<String>,
    model: Option<String>,
    parent: Option<RunContext>,
    remote: Option<crate::agent::RemoteApproval>,
    local_ask: Option<crate::agent::LocalAsk>,
}

impl TaskTool {
    pub const NAME: &'static str = "task";

    pub(crate) fn resolve_class(prompt: &str, class: Option<&str>) -> crate::harness::TaskClass {
        class
            .and_then(crate::harness::TaskClass::parse)
            .unwrap_or_else(|| crate::harness::classify_rules(prompt, false))
    }

    /// 도구 입력을 소유 인자로 옮긴다. 동기 run() 과 병렬 경로가 같은 파싱을 쓴다.
    pub fn parse_args(input: &Value, ctx: &ToolCtx) -> Result<TaskArgs> {
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
        Ok(TaskArgs {
            cfg,
            prompt,
            class: string("class"),
            role: string("role"),
            model: string("model"),
            parent: ctx.run.clone(),
            remote: ctx.remote.clone(),
            local_ask: ctx.local_ask.clone(),
        })
    }

    /// 병렬 구간이 쓰는 async 진입점 — 동기 run() 이 block_on 으로 감싸는 바로 그 실행이다.
    /// 다른 도구의 실행 방식(&self 동기 run)은 건드리지 않는다.
    pub async fn run_async(args: TaskArgs) -> Result<String> {
        Self::delegate(args).await
    }

    async fn delegate(args: TaskArgs) -> Result<String> {
        let TaskArgs {
            cfg,
            prompt,
            class,
            role,
            model,
            parent,
            remote,
            local_ask,
        } = args;
        let task_class = Self::resolve_class(&prompt, class.as_deref());
        // role 이 유효한 프로파일 이름(config 정의 또는 내장 전문가 프리셋)이면 그 프로파일로
        // 바인딩한다. 아니면 예전처럼 화면 표시용 라벨로만 쓴다.
        let profile = role
            .as_deref()
            .map(str::trim)
            .filter(|r| crate::harness::profile_exists(&cfg, r));
        let mut binding =
            crate::harness::bind_profile(&cfg, task_class, profile, None, model.as_deref())?;
        // 위임 서브에이전트도 실행 경로다 — 엔진 고정을 같은 규칙으로 적용한다.
        if let Some(w) =
            crate::harness::apply_engine_pin(&cfg, &mut binding, None, model.as_deref())
        {
            crate::ui::live_warn(&w);
        }
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
        // 팀 모드에서는 자식들의 라이브 출력이 섞이므로 시작·종료 줄에 역할 프리픽스를 붙인다.
        let team_tag = if crate::harness::team_mode(&cfg) == crate::engine::TeamMode::Multi {
            format!("[팀:{role}] ")
        } else {
            String::new()
        };
        // working 패널에서도 팀 모드면 역할이 팀 소속임을 드러낸다.
        let worker_role = if team_tag.is_empty() {
            role.clone()
        } else {
            format!("팀:{role}")
        };
        let worker_model = format!("{}/{}", binding.provider_name, binding.model);
        crate::ui::live_worker_in(
            &child,
            &agent_id.to_string(),
            &worker_role,
            &worker_model,
            "시작",
            false,
        );
        crate::ui::live_line_in(
            &child,
            &format!(
                "{team_tag}[task] {} · {} → {} ({})",
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
                if !team_tag.is_empty() {
                    crate::ui::live_line_in(&child, &format!("{team_tag}[task] 실패 · {error}"));
                }
                crate::ui::live_worker_in(
                    &child,
                    &agent_id.to_string(),
                    &worker_role,
                    &worker_model,
                    "실패",
                    true,
                );
                return Err(error);
            }
        };
        if let Some(parent) = &parent {
            parent.record_committed_paths(child.committed_paths());
        }
        finish_parent(&parent, &child, outcome.input_tokens, outcome.output_tokens);
        if !team_tag.is_empty() {
            crate::ui::live_line_in(
                &child,
                &format!("{team_tag}[task] 종료 · status={}", outcome.status),
            );
        }
        crate::ui::live_worker_in(
            &child,
            &agent_id.to_string(),
            &worker_role,
            &worker_model,
            &outcome.status,
            true,
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
                "role": {"type": "string", "description": "전문가 역할. planner(스펙·완료기준·작업분해) | frontend | backend | reviewer(DoD 대조 리뷰) 중 하나면 해당 전문가 프로파일(도구·품질 기준)로 실행된다. 그 밖의 값은 화면 표시용 라벨"},
                "model": {"type": "string", "description": "등록된 모델 ID. 생략하면 Harness가 능력과 비용에 따라 선택"}
            },
            "required": ["prompt"]
        })
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn run(&self, input: Value, ctx: &ToolCtx) -> Result<String> {
        let args = Self::parse_args(&input, ctx)?;
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(Self::delegate(args))
        })
    }
}
