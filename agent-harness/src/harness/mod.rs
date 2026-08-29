//! Harness 실행 계층 — v1.0.1 까지 단일 파일(harness.rs, 약 4,900라인)이던 것을
//! Phase C3 에서 역할별 모듈로 분리. 동작 변경 없음(순수 이동).
//!
//! - classify: 작업 난이도 분류(규칙 + LLM 보조)
//! - profiles: 내장 전문가 프로파일 조회
//! - binding:  프로파일·모델·계정 바인딩, 폴 fallback/핀, 프로바이더 호출
//! - team:     팀 모드 플래그와 위임 계약 블록
//! - runner:   시스템 프롬프트, 실행 파이프라인, 검증·리뷰 게이트, 그래프 실행

mod binding;
mod classify;
mod profiles;
mod runner;
mod team;

pub use binding::*;
pub use classify::*;
pub use profiles::*;
pub use runner::*;
pub use team::*;

pub(crate) use std::io::{self, Write};
pub(crate) use std::sync::Arc;
pub(crate) use std::time::Duration;

pub(crate) use anyhow::{Result, anyhow};

pub(crate) use crate::agent::{self, AgentOutcome, AgentRun};
pub(crate) use crate::config::{Config, ProviderConfig};
pub(crate) use crate::db::Db;
pub(crate) use crate::provider::{
    AnthropicProvider, ChatRequest, ChatResponse, ContentBlock, DynProvider, Message,
    OpenAiCompatProvider, StopReason, StreamEvent, emitted_chars, is_rate_limited, is_retryable,
};
pub(crate) use crate::run::{RunContext, RunId, TerminalState};
pub(crate) use crate::tools::{self, ToolCtx, ToolRegistry};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_simple_hello() {
        assert_eq!(classify_rules("안녕", false), TaskClass::Simple);
    }

    #[test]
    fn korean_file_work_is_never_simple() {
        // 실사례: simple(quick, 도구 0개)로 떨어져 모델이 tool call 을 텍스트로
        // 흉내 내던 질문 — 이제 도구 있는 클래스로 분류되어야 한다.
        let q = "그럼 니가 잘 하는 걸 더 잘 하게 만들 수 있게 마크다운 파일을 업그레이드 하면 좋지 않을 까?";
        assert_ne!(classify_rules(q, false), TaskClass::Simple);
        assert_eq!(
            classify_rules("AGENTS.md 업그레이드해줘", false),
            TaskClass::Dev
        );
        assert_ne!(
            classify_rules("워크스페이스에 뭐가 있어?", false),
            TaskClass::Simple
        );
    }

    #[test]
    fn leaked_tool_call_detects_text_tool_syntax() {
        assert!(leaked_tool_call(
            "워크스페이스부터 확인하겠습니다.]<]minimax[>[<tool_call> { \"name\": \"run_command\""
        ));
        assert!(leaked_tool_call(
            "<tool_call>{\"name\":\"x\",\"arguments\":{}}</tool_call>"
        ));
        assert!(!leaked_tool_call("마크다운을 이렇게 고치면 됩니다."));
        assert!(!leaked_tool_call("파일 이름은 name 필드에 적습니다."));
    }

    #[test]
    fn classifies_dev_from_extension() {
        assert_eq!(
            classify_rules("buggy.py 만들어서 고쳐줘", false),
            TaskClass::Dev
        );
    }

    #[test]
    fn classifies_advanced_from_keyword() {
        assert_eq!(
            classify_rules("이 저장소 구조 분석해서 개선 전략 보고서 써줘", false),
            TaskClass::Advanced
        );
    }

    #[test]
    fn obsidian_flag_alone_stays_simple() {
        assert_eq!(classify_rules("안녕", true), TaskClass::Simple);
        assert_eq!(classify_rules("안녕", false), TaskClass::Simple);
        // 노트 관련 키워드는 여전히 medium
        assert_eq!(classify_rules("내 노트 찾아줘", true), TaskClass::Medium);
    }

    #[test]
    fn maps_design_verify_debug_to_existing_classes() {
        assert_eq!(
            classify_rules("시스템 구성안을 설계해줘", false),
            TaskClass::Advanced
        );
        assert_eq!(classify_rules("이 코드 검증해줘", false), TaskClass::Dev);
        assert_eq!(classify_rules("디버깅 좀 도와줘", false), TaskClass::Dev);
    }

    #[test]
    fn auto_pick_falls_back_to_registered_only() {
        let table = crate::ranks::bundled();
        let regs = vec![
            crate::auth::RegisteredModel {
                provider: "grok".into(),
                id: "grok-3".into(),
                small: false,
            },
            crate::auth::RegisteredModel {
                provider: "anthropic".into(),
                id: "claude-haiku-4-5".into(),
                small: true,
            },
        ];
        let hit = pick_strongest(&regs, &table).expect("ranked");
        assert!(!hit.id.contains("opus-5"));
        assert!(!hit.id.contains("gpt-5.6"));
        assert_eq!(hit.id, "claude-haiku-4-5");

        let only_grok = vec![crate::auth::RegisteredModel {
            provider: "grok".into(),
            id: "grok-3".into(),
            small: false,
        }];
        let hit = pick_strongest(&only_grok, &table).expect("grok");
        assert_eq!(hit.provider, "grok");
        assert_eq!(hit.id, "grok-3");

        let flagships = vec![
            crate::auth::RegisteredModel {
                provider: "anthropic".into(),
                id: "claude-opus-5".into(),
                small: false,
            },
            crate::auth::RegisteredModel {
                provider: "openai".into(),
                id: "gpt-5.6".into(),
                small: false,
            },
        ];
        let cheap = pick_cheap(&flagships, &table, "anthropic").expect("ok flagship");
        assert!(cheap.id.contains("opus") || cheap.id.contains("gpt-5.6"));
    }

    #[test]
    fn goal_continues_only_while_open_todos_make_progress() {
        assert!(goal_should_continue(1, 3, 0, 0, 8));
        assert!(goal_should_continue(2, 3, 1, 1, 8));
        assert!(!goal_should_continue(3, 3, 1, 2, 8));
        assert!(!goal_should_continue(1, 1, 0, 0, 8));
        // 한도는 엔진 사양(EngineSpec.max_continuations)이 정한다.
        assert!(!goal_should_continue(1, 3, 0, 3, 3));
        assert!(goal_should_continue(1, 3, 0, 3, 4));
    }

    #[test]
    fn retry_after_dev_tool_activity_stays_dev() {
        let history = vec![Message {
            role: crate::provider::Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool-1".into(),
                name: "write_file".into(),
                input: serde_json::json!({"path": "index.html"}),
            }],
        }];

        assert_eq!(
            continuation_class("다시 해줘", TaskClass::Simple, &history),
            TaskClass::Dev
        );
        assert_eq!(
            continuation_class("원리를 설명해줘", TaskClass::Simple, &history),
            TaskClass::Simple
        );
    }

    #[test]
    fn judge_cannot_remove_tools_from_a_clear_dev_request() {
        assert_eq!(
            apply_tool_floor(TaskClass::Dev, TaskClass::Simple),
            TaskClass::Dev
        );
        assert_eq!(
            apply_tool_floor(TaskClass::Medium, TaskClass::Simple),
            TaskClass::Simple
        );
    }

    #[test]
    fn turn_iteration_budget_caps_all_continuations() {
        assert_eq!(turn_iteration_budget(25), 50);
        assert_eq!(turn_iteration_budget(50), agent::HARD_CAP);
        assert_eq!(remaining_iteration_budget(50, 49, 25), 1);
        assert_eq!(remaining_iteration_budget(50, 50, 25), 0);
    }

    #[test]
    fn goal_is_not_complete_before_verification_finishes() {
        assert_eq!(goal_persist_status("ok", 0, 0, false), "active");
        assert_eq!(goal_persist_status("ok", 2, 2, true), "complete");
        assert_eq!(goal_persist_status("fail", 2, 2, true), "failed");
        assert_eq!(goal_persist_status("incomplete", 1, 2, true), "blocked");
    }

    #[test]
    fn contract_plan_prompt_keeps_main_system_context() {
        // 회귀 방지: 계획 호출이 system 을 통째로 교체하면 lessons·system_extra·
        // 프로젝트 규칙이 계획에서 사라진다 (v1 결함).
        let dir = std::env::temp_dir().join(format!("rafikx-plan-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let cfg = Config::load(Some(&dir.join("config.toml"))).expect("config");
        let extra = "신중한 시니어 개발자다. 최소 diff 로 고친다.";
        let lessons = "[프로젝트 교훈]\n- 회귀 테스트 없이 고치지 말 것";
        let system = system_prompt(&cfg, extra, lessons);

        let contract = plan_system_prompt(&system, crate::engine::PlanDepth::Contract, "");
        assert!(contract.contains(extra), "system_extra 유실");
        assert!(contract.contains("[프로젝트 교훈]"), "lessons 유실");
        assert!(contract.contains("[계획 모드]"));
        // Contract 는 3부 산출물을 강제한다.
        assert!(contract.contains("[해석]"));
        assert!(contract.contains("[완료 기준]"));
        assert!(contract.contains("[작업 분해]"));

        let brief = plan_system_prompt(&system, crate::engine::PlanDepth::Brief, "");
        assert!(brief.contains(extra), "system_extra 유실");
        assert!(brief.contains("[프로젝트 교훈]"), "lessons 유실");
        assert!(brief.contains("3~7개 항목"));
        assert!(!brief.contains("[완료 기준]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn self_harness_plan_instruction_is_plan_only() {
        let dir = std::env::temp_dir().join(format!("rafikx-planface-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let cfg = Config::load(Some(&dir.join("config.toml"))).expect("config");
        let system = system_prompt(&cfg, "", "");

        // 메타 레이어가 꺼져 있으면(빈 문자열) 계획 프롬프트에 아무것도 붙지 않는다.
        let off = plan_system_prompt(&system, crate::engine::PlanDepth::Brief, "");
        assert!(!off.contains("[Self-Harness 계획 지침]"));

        let on = plan_system_prompt(
            &system,
            crate::engine::PlanDepth::Contract,
            "  완료 기준을 검증 가능한 형태로 쓴다.  ",
        );
        assert!(on.contains("[Self-Harness 계획 지침] 완료 기준을 검증 가능한 형태로 쓴다."));
        // 계획 전용 면이므로 메인 시스템 프롬프트에는 없어야 한다.
        assert!(!system.contains("[Self-Harness 계획 지침]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_line_shows_only_non_default_axes() {
        let dir = std::env::temp_dir().join(format!("rafikx-suffix-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut cfg = Config::load(Some(&dir.join("config.toml"))).expect("config");

        // 기본값(rafikx · harness)이면 표시를 늘리지 않는다.
        assert!(engine_suffix(&cfg).is_empty());

        cfg.file.general.engine = "claude".into();
        assert_eq!(engine_suffix(&cfg), "  ·  engine=claude");

        cfg.file.general.discipline = "graph".into();
        assert_eq!(engine_suffix(&cfg), "  ·  engine=claude  ·  graph");

        // legacy engine="self" 는 rafikx 로 정규화되므로 엔진 표시가 빠진다.
        cfg.file.general.engine = "self".into();
        cfg.file.general.discipline = "loop".into();
        assert_eq!(engine_suffix(&cfg), "  ·  loop");

        // 팀 모드는 multi 일 때만 붙는다.
        assert_eq!(team_mode(&cfg), crate::engine::TeamMode::Single);
        cfg.file.harness.team = "multi".into();
        assert_eq!(engine_suffix(&cfg), "  ·  loop  ·  team");
        cfg.file.general.discipline = "harness".into();
        cfg.file.general.engine = "rafikx".into();
        assert_eq!(engine_suffix(&cfg), "  ·  team");
        // 오타는 single 로 흡수되어 표시가 늘지 않는다.
        cfg.file.harness.team = "멀티".into();
        assert!(engine_suffix(&cfg).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// auth 를 타지 않는(auth="none") 합성 프로바이더 — 테스트가 실제 로그인 상태에
    /// 의존하지 않게 한다.
    fn fake_provider(model: &str, small: &str) -> ProviderConfig {
        ProviderConfig {
            models_url: None,
            kind: "openai_compat".into(),
            auth: "none".into(),
            api_key_env: String::new(),
            model: model.into(),
            small_model: Some(small.into()),
            base_url: Some("http://localhost:9/v1".into()),
            supports_tools: true,
            model_auto: false,
            context_window: None,
            enabled: true,
        }
    }

    #[test]
    fn profile_model_resolves_provider_and_yields_only_to_explicit_model() {
        // 등록 모델 조회는 주입 — 실제 계정 상태에 기대지 않는다.
        let lookup = |m: &str| (m == "등록된모델").then(|| "찾은프로바이더".to_string());

        // 값이 없으면 기존 선택 규칙(manual·single·auto)을 그대로 쓴다.
        assert_eq!(decide_profile_model(None, "anthropic", None, lookup), None);
        assert_eq!(
            decide_profile_model(Some("   "), "anthropic", None, lookup),
            None
        );

        // "provider:model" — 앞부분이 프로바이더.
        assert_eq!(
            decide_profile_model(Some("minimax:MiniMax-M2"), "anthropic", None, lookup),
            Some(("minimax".into(), "MiniMax-M2".into()))
        );

        // 모델 ID 단독 — 등록 모델 조회가 성공하면 그 프로바이더.
        assert_eq!(
            decide_profile_model(Some("등록된모델"), "anthropic", None, lookup),
            Some(("찾은프로바이더".into(), "등록된모델".into()))
        );
        // 조회에 실패하면 프로파일의 provider 로 떨어진다.
        assert_eq!(
            decide_profile_model(Some("  낯선모델  "), "anthropic", None, lookup),
            Some(("anthropic".into(), "낯선모델".into()))
        );

        // pin 재바인딩(apply_engine_pin 이 provider_override 로 pin 을 넘긴다):
        // 프로바이더는 pin 이 이기고 모델 ID 만 존중한다.
        assert_eq!(
            decide_profile_model(Some("minimax:MiniMax-M2"), "anthropic", Some("glm"), lookup),
            Some(("glm".into(), "MiniMax-M2".into()))
        );
        assert_eq!(
            decide_profile_model(Some("등록된모델"), "anthropic", Some("glm"), lookup),
            Some(("glm".into(), "등록된모델".into()))
        );
        // 공백뿐인 오버라이드는 지정이 아니다.
        assert_eq!(
            decide_profile_model(Some("minimax:MiniMax-M2"), "anthropic", Some(" "), lookup),
            Some(("minimax".into(), "MiniMax-M2".into()))
        );
    }

    #[test]
    fn profile_model_survives_engine_pin_rebinding() {
        let dir =
            std::env::temp_dir().join(format!("rafikx-teammodel-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut cfg = Config::load(Some(&dir.join("config.toml"))).expect("config");
        cfg.file
            .providers
            .insert("fake_a".into(), fake_provider("a-main", "a-small"));
        cfg.file
            .providers
            .insert("fake_b".into(), fake_provider("b-main", "b-small"));

        // 역할에 모델을 배정한 프로파일 (팀 모드의 분업).
        let mut role = crate::config::builtin_profile("backend").expect("preset");
        role.provider = "fake_a".into();
        role.tools = vec!["read_file".into()];
        role.model = Some("role-only-model".into());
        cfg.file.subagents.insert("backend".into(), role.clone());

        // 엔진 고정을 fake_b 로 건다 — 실행 경로 프로바이더는 고정이 이겨야 한다.
        cfg.file.general.engine = "claude".into();
        cfg.file.engines.insert(
            "claude".into(),
            crate::engine::EngineOverride {
                pin_provider: Some("fake_b".into()),
                ..Default::default()
            },
        );

        let mut binding = bind_profile(&cfg, TaskClass::Dev, Some("backend"), None, None)
            .expect("프로파일 모델 바인딩");
        assert_eq!(binding.provider_name, "fake_a");
        assert_eq!(binding.model, "role-only-model");

        // pin 재바인딩이 프로파일 모델을 지우지 않는다: provider 만 고정으로 바뀐다.
        assert_eq!(apply_engine_pin(&cfg, &mut binding, None, None), None);
        assert_eq!(binding.provider_name, "fake_b");
        assert_eq!(binding.model, "role-only-model");

        // 모델 ID 가 비면 고정 프로바이더의 model_role 규칙으로 되돌아간다.
        let mut plain = role.clone();
        plain.model = None;
        cfg.file.subagents.insert("backend".into(), plain);
        let mut binding = bind_profile(&cfg, TaskClass::Dev, Some("backend"), Some("fake_b"), None)
            .expect("model_role 바인딩");
        assert_eq!(binding.model, "b-main");

        // 사용자가 모델을 직접 지정하면 프로파일 모델을 이긴다.
        cfg.file.subagents.insert("backend".into(), role);
        binding = bind_profile(
            &cfg,
            TaskClass::Dev,
            Some("backend"),
            Some("fake_b"),
            Some("user-picked"),
        )
        .expect("명시 모델 바인딩");
        assert_eq!(binding.model, "user-picked");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn team_block_only_for_multi_dev_advanced_outside_graph() {
        use crate::engine::TeamMode;
        for class in [TaskClass::Dev, TaskClass::Advanced] {
            assert!(team_block_active(TeamMode::Multi, class, true, false));
            // graph 분야와 상호 배타 — 그래프가 이미 분해 실행을 담당한다.
            assert!(!team_block_active(TeamMode::Multi, class, true, true));
            // 도구가 없으면 위임 자체가 불가능하다.
            assert!(!team_block_active(TeamMode::Multi, class, false, false));
            // single 은 현행 그대로.
            assert!(!team_block_active(TeamMode::Single, class, true, false));
        }
        for class in [TaskClass::Simple, TaskClass::Medium] {
            assert!(!team_block_active(TeamMode::Multi, class, true, false));
        }
        // 지침 본문은 role 목록과 병렬 호출 방법을 함께 알려야 한다.
        for key in ["planner", "frontend", "backend", "reviewer", "병렬"] {
            assert!(TEAM_MULTI_BLOCK.contains(key), "{key} 지시 없음");
        }
        assert!(TEAM_MULTI_BLOCK.contains("자체 완결"));
    }

    #[test]
    fn bind_falls_back_to_builtin_expert_profiles() {
        let dir = std::env::temp_dir().join(format!("rafikx-profile-{}", crate::db::Db::new_id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut cfg = Config::load(Some(&dir.join("config.toml"))).expect("config");

        // 기본 config 에는 전문가 프로파일이 없다 — 내장 프리셋으로 폴백해야 한다.
        assert!(!cfg.file.subagents.contains_key("planner"));
        let planner = resolve_profile(&cfg, "planner").expect("planner 프리셋");
        assert!(planner.tools.iter().any(|t| t == "todo_write"));
        assert!(!planner.tools.iter().any(|t| t == "write_file"));
        assert!(!planner.plan_first && !planner.verify);
        assert!(planner.system_extra.contains("[완료 기준]"));

        let reviewer = resolve_profile(&cfg, "reviewer").expect("reviewer 프리셋");
        assert_eq!(reviewer.max_iterations, REVIEW_GATE_MAX_ITER);
        assert!(reviewer.tools.iter().any(|t| t == "bash"));
        assert!(!reviewer.tools.iter().any(|t| t == "write_file"));
        assert!(reviewer.system_extra.contains("[판정]"));
        assert!(reviewer.system_extra.contains("[미충족 항목]"));

        for role in ["frontend", "backend"] {
            let sub = resolve_profile(&cfg, role).expect(role);
            assert_eq!(sub.tools, vec!["*".to_string()]);
            assert!(sub.plan_first && sub.verify);
            assert!(sub.system_extra.contains("[변경 요약]"));
        }

        // config 정의가 있으면 사용자 정의가 이긴다.
        let mut custom = crate::config::builtin_profile("planner").expect("preset");
        custom.system_extra = "사내 기획 규칙만 따른다".into();
        custom.max_iterations = 3;
        cfg.file.subagents.insert("planner".into(), custom);
        let planner = resolve_profile(&cfg, "planner").expect("사용자 정의 planner");
        assert_eq!(planner.system_extra, "사내 기획 규칙만 따른다");
        assert_eq!(planner.max_iterations, 3);

        // 등록되지 않은 이름은 폴백 대상이 아니다.
        assert!(resolve_profile(&cfg, "없는프로파일").is_none());
        assert!(resolve_profile(&cfg, "  ").is_none());
        assert!(profile_exists(&cfg, "coder")); // config 정의
        assert!(profile_exists(&cfg, "frontend")); // 내장 프리셋
        assert!(!profile_exists(&cfg, "없는역할"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_verdict_parsing_and_gate_transitions() {
        // 통과 판정.
        assert_eq!(
            parse_review_verdict("[판정] pass\n[미충족 항목] 없음\n[결함] 없음"),
            ReviewVerdict::Pass
        );
        assert_eq!(parse_review_verdict("[판정] 통과"), ReviewVerdict::Pass);
        // 판정 줄이 없으면 통과가 아니라 판정 불능이다 ("신호 없음 = 통과" 금지).
        assert_eq!(
            parse_review_verdict("리뷰 도중 파일을 열지 못했습니다."),
            ReviewVerdict::Indeterminate
        );
        assert_eq!(parse_review_verdict(""), ReviewVerdict::Indeterminate);
        // 판정이 여러 개면 마지막이 결론이다 — 1회차의 "판정하겠다" 류 오탐 제거.
        assert_eq!(
            parse_review_verdict("[판정] 은 마지막에 fail 여부로 내겠다\n…확인 완료\n[판정] pass"),
            ReviewVerdict::Pass
        );
        assert!(matches!(
            parse_review_verdict("[판정] pass 로 보인다\n추가 확인 결과\n[판정] fail\n[결함] 누락"),
            ReviewVerdict::Fail { .. }
        ));

        // 미통과 — 미충족 항목과 결함이 사유로 모인다.
        let text = "[판정] fail\n\
                    [미충족 항목]\n\
                    2. 캐시 히트율 로그 — 로그 출력이 없음\n\
                    [결함]\n\
                    src/cache.rs:42 — unwrap 사용 — 빈 키에서 패닉";
        let ReviewVerdict::Fail { summary } = parse_review_verdict(text) else {
            panic!("fail 판정이어야 한다");
        };
        assert!(summary.contains("캐시 히트율"));
        assert!(summary.contains("src/cache.rs:42"));
        assert!(!summary.contains("[판정]"));
        // '미통과'는 '통과'를 부분 문자열로 포함한다 — 실패 표지를 먼저 봐야 한다.
        assert!(matches!(
            parse_review_verdict("[판정] 미통과\n[결함] 테스트 없음"),
            ReviewVerdict::Fail { .. }
        ));
        // 부연의 부정어 오탐 방지 (실측 2026-08-26): 먼저 나온 판정어가 결론이다.
        assert_eq!(
            parse_review_verdict("[판정] pass — 실패 요인 없음, 모두 충족"),
            ReviewVerdict::Pass
        );
        assert_eq!(
            parse_review_verdict("[판정] 통과 (미충족 항목 없음)"),
            ReviewVerdict::Pass
        );
        // 판정어가 없는 판정 줄은 결론이 아니다 — 단독이면 판정 불능.
        assert_eq!(
            parse_review_verdict("[판정] 아직 근거가 부족하다"),
            ReviewVerdict::Indeterminate
        );
        // 구조를 지키지 않은 fail 출력은 본문 전체를 사유로 쓴다.
        let ReviewVerdict::Fail { summary } = parse_review_verdict("[판정] fail\n그냥 부족합니다")
        else {
            panic!("fail 판정이어야 한다");
        };
        assert!(summary.contains("그냥 부족합니다"));

        // 상태 전이: 1회차 미통과 → 재개, 재개 후에도 미통과 → 보고 후 종료.
        let fail = || ReviewVerdict::Fail {
            summary: "[결함] 테스트 없음".into(),
        };
        assert_eq!(
            gate_action(ReviewVerdict::Pass, 0, false),
            GateAction::Accept
        );
        assert_eq!(
            gate_action(ReviewVerdict::Pass, 1, true),
            GateAction::Accept
        );
        assert_eq!(
            gate_action(fail(), 0, false),
            GateAction::Resume("[결함] 테스트 없음".into())
        );
        assert_eq!(
            gate_action(fail(), 1, false),
            GateAction::Report("[결함] 테스트 없음".into())
        );
        // 판정 불능: 재질의는 실행당 1회, 그 뒤에도 불능이면 통과가 아니라 실패 보고다 (M2: G11).
        assert_eq!(
            gate_action(ReviewVerdict::Indeterminate, 0, false),
            GateAction::Requery
        );
        assert_eq!(
            gate_action(ReviewVerdict::Indeterminate, 0, true),
            GateAction::Report("판정 불능 — 검증자가 판정하지 못했다".into())
        );
        assert_eq!(
            verdict_headline("[결함] 테스트 없음\n둘째 줄"),
            "[결함] 테스트 없음"
        );
        assert_eq!(verdict_headline("   "), "사유 미상");
    }

    #[test]
    fn review_prompt_sends_dod_and_files_but_not_diffs() {
        let changed = vec!["src/cache.rs".to_string(), "src/main.rs".to_string()];
        let p = review_prompt("캐시를 추가하라", "1. cargo test 통과", "", &changed);
        assert!(p.contains("캐시를 추가하라"));
        assert!(p.contains("[완료 기준]\n1. cargo test 통과"));
        assert!(p.contains("- src/cache.rs"));
        assert!(p.contains("- src/main.rs"));
        // 신선한 시각: diff 를 첨부하지 않고 리뷰어가 직접 읽게 한다.
        assert!(p.contains("read_file"));

        // DoD 가 없으면 그 절은 통째로 빠진다.
        let p = review_prompt("캐시를 추가하라", "   ", "  ", &[]);
        assert!(!p.contains("[완료 기준]"));
        assert!(!p.contains("[계획이 지목한 최대 위험]"));
        assert!(p.contains("변경 파일 없음"));
    }

    #[test]
    fn rebuttal_reaches_the_verifier_gate_prompt() {
        let plan = "[해석] 캐시 계층을 추가한다.\n\
                    [완료 기준]\n\
                    1. cargo test 통과 — `cargo test` 실행\n\
                    [작업 분해]\n\
                    1. 인터페이스 정의\n\
                    [반박] 위험: 동시 쓰기에서 캐시가 낡는다.\n\
                    최소 테스트: 두 스레드로 같은 키를 갱신하고 값을 재조회한다.";
        let rebuttal = extract_plan_section(plan, "[반박]");
        assert!(rebuttal.starts_with("위험: 동시 쓰기에서"));
        assert!(rebuttal.contains("최소 테스트: 두 스레드로"));
        // [반박] 이 뒤에 붙어도 앞 절들은 그대로 잘린다.
        assert!(!extract_plan_section(plan, "[작업 분해]").contains("반박"));

        let p = review_prompt("캐시를 추가하라", "1. cargo test 통과", &rebuttal, &[]);
        assert!(p.contains("[계획이 지목한 최대 위험]"));
        assert!(p.contains("동시 쓰기에서 캐시가 낡는다"));
        // 계획 지시 자체가 [반박] 절을 요구해야 이 경로가 채워진다.
        assert!(PLAN_CONTRACT_INSTRUCTION.contains("[반박]"));
    }

    #[test]
    fn plan_sections_are_extracted_for_the_verifier_gate() {
        let plan = "[해석] 캐시 계층을 추가한다.\n\
                    가정: 기존 저장소는 그대로 둔다.\n\
                    [완료 기준]\n\
                    1. cargo test 통과 — `cargo test` 실행\n\
                    2. 캐시 히트율 로그 — 실행 후 로그 확인\n\
                    [작업 분해]\n\
                    1. 인터페이스 정의\n\
                    2. 구현";
        let dod = extract_plan_section(plan, "[완료 기준]");
        assert!(dod.starts_with("1. cargo test 통과"));
        assert!(dod.contains("캐시 히트율"));
        assert!(!dod.contains("작업 분해"));
        assert!(!dod.contains("캐시 계층을 추가한다"));

        let interp = extract_plan_section(plan, "[해석]");
        assert!(interp.starts_with("캐시 계층을 추가한다."));
        assert!(interp.contains("가정: 기존 저장소는"));

        assert!(extract_plan_section(plan, "[없는 절]").is_empty());
        assert!(extract_plan_section("자유 형식 계획 3줄", "[완료 기준]").is_empty());
    }

    fn dag(spec: &[(&str, &[&str])]) -> Vec<DagNode> {
        spec.iter()
            .map(|(id, deps)| DagNode {
                id: (*id).into(),
                goal: format!("{id} 목표"),
                deps: deps.iter().map(|d| (*d).to_string()).collect(),
                produces: String::new(),
            })
            .collect()
    }

    #[test]
    fn parse_dag_reads_fenced_and_bare_json() {
        let fenced = "[완료 기준]\n1. cargo test 통과 — `cargo test`\n\n\
             ```json\n\
             {\"nodes\":[{\"id\":\"n1\",\"goal\":\"스키마 정의\",\"deps\":[],\"produces\":\"타입 3종\"},\
             {\"id\":\"n2\",\"goal\":\"구현\",\"deps\":[\"n1\"],\"produces\":\"모듈\"}]}\n\
             ```";
        let nodes = parse_dag(fenced).expect("펜스 JSON");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].id, "n1");
        assert_eq!(nodes[0].goal, "스키마 정의");
        assert_eq!(nodes[1].deps, vec!["n1".to_string()]);
        // 완료 기준은 JSON 앞 산문에서만 뽑는다 — JSON 을 빨아들이지 않는다.
        let dod = extract_plan_section(plan_prose(fenced), "[완료 기준]");
        assert_eq!(dod, "1. cargo test 통과 — `cargo test`");
        assert!(!dod.contains("nodes"));

        // 펜스가 없어도 첫 `{` 부터 균형 매칭으로 찾는다. 뒤에 산문이 붙어도 된다.
        let bare = "계획입니다.\n{\"nodes\":[{\"id\":\"a\",\"goal\":\"조사 {중괄호} 포함\",\
                    \"deps\":[]}]}\n이상.";
        let nodes = parse_dag(bare).expect("맨 JSON");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].goal, "조사 {중괄호} 포함");
        assert!(nodes[0].deps.is_empty());
        assert!(nodes[0].produces.is_empty());
    }

    #[test]
    fn parse_dag_rejects_malformed_plans() {
        // JSON 자체가 없다 → harness 폴백.
        assert!(parse_dag("1. 스키마\n2. 구현\n3. 테스트").is_none());
        // 깨진 JSON.
        assert!(parse_dag("{\"nodes\":[{\"id\":\"n1\",").is_none());
        // nodes 키가 없다.
        assert!(parse_dag("{\"steps\":[{\"id\":\"n1\",\"goal\":\"x\"}]}").is_none());
        // 빈 목록.
        assert!(parse_dag("{\"nodes\":[]}").is_none());
        // id·goal 이 비었거나 id 가 중복이면 실행 순서를 정의할 수 없다.
        assert!(parse_dag("{\"nodes\":[{\"id\":\" \",\"goal\":\"x\"}]}").is_none());
        assert!(parse_dag("{\"nodes\":[{\"id\":\"n1\",\"goal\":\"  \"}]}").is_none());
        assert!(
            parse_dag(
                "{\"nodes\":[{\"id\":\"n1\",\"goal\":\"a\"},{\"id\":\"n1\",\"goal\":\"b\"}]}"
            )
            .is_none()
        );
    }

    #[test]
    fn topo_order_sorts_dependencies_and_detects_cycles() {
        // n3 ← n2 ← n1 역순으로 적혀 있어도 위상순으로 되돌린다.
        let nodes = dag(&[("n3", &["n2"]), ("n1", &[]), ("n2", &["n1"])]);
        let order = topo_order(&nodes).expect("정렬");
        let ids: Vec<&str> = order.iter().map(|i| nodes[*i].id.as_str()).collect();
        assert_eq!(ids, vec!["n1", "n2", "n3"]);

        // 병렬 가지도 전부 한 번씩 나온다 (순차 실행이므로 순서만 유효하면 된다).
        let nodes = dag(&[("a", &[]), ("b", &["a"]), ("c", &["a"]), ("d", &["b", "c"])]);
        let order = topo_order(&nodes).expect("정렬");
        assert_eq!(order.len(), 4);
        let pos = |id: &str| {
            order
                .iter()
                .position(|i| nodes[*i].id == id)
                .expect("노드 위치")
        };
        assert!(pos("a") < pos("b") && pos("a") < pos("c"));
        assert!(pos("b") < pos("d") && pos("c") < pos("d"));

        // 같은 deps 가 두 번 적혀도 진입차수를 중복으로 세지 않는다.
        let nodes = dag(&[("a", &[]), ("b", &["a", "a"])]);
        assert_eq!(topo_order(&nodes).expect("중복 deps").len(), 2);

        // 모르는 id 를 가리키는 deps 는 간선이 없다 (계획의 오타로 실행이 막히지 않게).
        let nodes = dag(&[("a", &["없음"])]);
        assert_eq!(topo_order(&nodes).expect("미상 deps"), vec![0]);

        // 순환·자기참조는 폴백 사유로 보고한다.
        let cycle = topo_order(&dag(&[("n1", &["n2"]), ("n2", &["n1"])])).expect_err("순환");
        assert_eq!(cycle.remaining, vec!["n1".to_string(), "n2".to_string()]);
        assert!(cycle.to_string().contains("순환"));
        assert!(topo_order(&dag(&[("n1", &["n1"])])).is_err());
    }

    #[test]
    fn graph_node_prompts_carry_only_dependency_conclusions() {
        let nodes = dag(&[("n1", &[]), ("n2", &[]), ("n3", &["n1"])]);
        let produced = vec![
            ("n1".to_string(), "타입 3종을 정의했다".to_string()),
            ("n2".to_string(), "무관한 노드 산출물".to_string()),
        ];
        let system = graph_node_system("메인 시스템", 3, 3, &nodes[2], &produced);
        assert!(system.starts_with("메인 시스템"));
        assert!(system.contains("[그래프 노드 3/3] 목표: n3 목표"));
        assert!(system.contains("[선행 산출물 n1]\n타입 3종을 정의했다"));
        // deps 에 없는 노드의 산출물은 넘기지 않는다 (컨텍스트 격리).
        assert!(!system.contains("무관한 노드 산출물"));

        let prompt = graph_node_prompt("전체 작업", &nodes[2]);
        assert!(prompt.starts_with("전체 작업"));
        assert!(prompt.contains("[이번 노드] n3 목표"));
        assert!(prompt.contains("이 노드의 목표만 수행하라"));
    }

    #[test]
    fn graph_node_success_rescues_completed_todo_limit() {
        assert!(graph_node_ok("ok", 0, 0));
        // 등록한 todo 를 모두 끝내고 반복 상한에 닿았으면 성공으로 구제한다.
        assert!(graph_node_ok("limit", 3, 3));
        assert!(!graph_node_ok("limit", 2, 3));
        assert!(!graph_node_ok("limit", 0, 0));
        assert!(!graph_node_ok("fail", 3, 3));
        assert!(!graph_node_ok("incomplete", 1, 2));
    }

    #[test]
    fn graph_node_summary_keeps_conclusion_and_changed_files() {
        let mut outcome = AgentOutcome {
            changed_files: vec!["src/a.rs".into()],
            ..Default::default()
        };
        outcome
            .messages
            .push(Message::user_text("무시할 사용자 글"));
        outcome.messages.push(Message {
            role: crate::provider::Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "앞 노드 답변".into(),
            }],
        });
        outcome.messages.push(Message {
            role: crate::provider::Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "마".repeat(700),
            }],
        });
        let s = graph_node_summary(&outcome);
        // 마지막 답변만, 500자까지.
        assert!(!s.contains("앞 노드 답변"));
        assert!(!s.contains("무시할 사용자 글"));
        assert_eq!(s.matches('마').count(), GRAPH_SUMMARY_CAP);
        assert!(s.contains("변경 파일: src/a.rs"));

        let empty = graph_node_summary(&AgentOutcome::default());
        assert_eq!(empty, "(보고된 산출물 없음)");
    }

    #[test]
    fn loop_discipline_prompts_are_wired_to_the_rule_and_the_switch() {
        // 루프 규율은 종료 조건을, 정체 지시는 접근 전환을 못 박는다.
        assert!(LOOP_DISCIPLINE_RULE.contains("완료를 선언하지 마라"));
        assert!(LOOP_STALE_SWITCH.contains("진전이 없었다"));
        assert!(LOOP_STALE_SWITCH.contains("같은 접근의 반복을 금지한다"));
        // 그래프 계획은 [완료 기준] 절을 JSON 앞에 요구한다 (게이트 입력 보존).
        assert!(PLAN_GRAPH_INSTRUCTION.contains("[완료 기준]"));
        assert!(PLAN_GRAPH_INSTRUCTION.contains("\"nodes\""));
        let sys = plan_system_prompt_with("메인", PLAN_GRAPH_INSTRUCTION, "계획 면");
        assert!(sys.starts_with("메인"));
        assert!(sys.contains(PLAN_MODE_HEADER));
        assert!(sys.contains("[Self-Harness 계획 지침] 계획 면"));
    }

    #[test]
    fn pin_beats_automatic_choice_but_yields_to_explicit_override() {
        // 고정이 없으면 아무것도 하지 않는다.
        assert_eq!(decide_pin(None, "anthropic", None, None), PinDecision::Keep);
        assert_eq!(
            decide_pin(Some("  "), "anthropic", None, None),
            PinDecision::Keep
        );

        // 자동 선택(ranks·manual_*·프로파일 기본)이 고른 프로바이더는 고정에 진다.
        assert_eq!(
            decide_pin(Some("minimax"), "anthropic", None, None),
            PinDecision::Apply("minimax".into())
        );
        // sticky 재사용도 "직접 지정"이 아니다 — bind 에는 값이 들어오지만 명시 인자는 비어 있다.
        assert_eq!(
            decide_pin(Some("minimax"), "openai", None, None),
            PinDecision::Apply("minimax".into())
        );

        // 명시 --provider 는 고정을 이긴다 (경고 한 줄).
        assert_eq!(
            decide_pin(Some("minimax"), "anthropic", Some("anthropic"), None),
            PinDecision::Yield {
                pin: "minimax".into(),
                explicit: "provider=anthropic".into()
            }
        );
        // 모델만 직접 고른 경우도 사용자 의지 — 그 모델이 없는 프로바이더로 끌고 가지 않는다.
        assert_eq!(
            decide_pin(Some("minimax"), "openai", None, Some("gpt-5")),
            PinDecision::Yield {
                pin: "minimax".into(),
                explicit: "model=gpt-5".into()
            }
        );

        // 이미 고정 프로바이더면 명시가 있든 없든 조용히 통과한다.
        assert_eq!(
            decide_pin(Some("minimax"), "minimax", None, None),
            PinDecision::Keep
        );
        assert_eq!(
            decide_pin(
                Some("minimax"),
                "MiniMax",
                Some("minimax"),
                Some("minimax-m3")
            ),
            PinDecision::Keep
        );
    }

    #[test]
    fn pinned_fallback_order_prefers_pin_then_keeps_rest() {
        let order = || {
            vec![
                "anthropic".to_string(),
                "minimax".to_string(),
                "glm".to_string(),
            ]
        };
        // 기본(pin_strict=false): 고정이 선두, 나머지는 후순위로 살아남는다.
        // 고정 프로바이더 전면 장애에도 실행이 이어져야 한다 (§15.2).
        assert_eq!(
            limit_order_to_pin(Some("minimax"), false, None, order()),
            vec![
                "minimax".to_string(),
                "anthropic".to_string(),
                "glm".to_string()
            ]
        );
        // pin_strict = true 면 예전처럼 고정 하나로 제한한다 (계정 순회는 안쪽이 담당).
        assert_eq!(
            limit_order_to_pin(Some("minimax"), true, None, order()),
            vec!["minimax".to_string()]
        );
        // 고정이 없으면 원래 순서 그대로.
        assert_eq!(limit_order_to_pin(None, false, None, order()), order());
        assert_eq!(limit_order_to_pin(Some(""), false, None, order()), order());
        // 명시 --provider 가 있으면 고정을 양보한다.
        assert_eq!(
            limit_order_to_pin(Some("minimax"), false, Some("anthropic"), order()),
            order()
        );
        assert_eq!(
            limit_order_to_pin(Some("minimax"), true, Some("anthropic"), order()),
            order()
        );
        // 고정 프로바이더가 순서에 없으면(미연결) 가용성을 우선해 원래 순서를 지킨다.
        assert_eq!(
            limit_order_to_pin(Some("moonshot"), false, None, order()),
            order()
        );
        assert_eq!(
            limit_order_to_pin(Some("moonshot"), true, None, order()),
            order()
        );
    }

    #[test]
    fn contract_plan_seeds_todo_with_the_step_body() {
        let steps = "1. 인터페이스 정의 — src/cache.rs\n2. 저장소 연결\n3. 테스트 추가";
        let seeded = contract_seed_task("캐시를 추가하라", steps);
        // 착수 지시가 단계 본문을 그대로 실어야 한다 (원거리 참조 금지, §15.3).
        assert!(seeded.contains("[착수 지시]"));
        assert!(seeded.contains("[작업 분해]"));
        assert!(seeded.contains("1. 인터페이스 정의 — src/cache.rs"));
        assert!(seeded.contains("3. 테스트 추가"));
        assert!(seeded.starts_with("캐시를 추가하라"));
        // 계획이 없으면 원 작업 그대로 — 빈 지시를 붙이지 않는다.
        assert_eq!(
            contract_seed_task("캐시를 추가하라", "  \n "),
            "캐시를 추가하라"
        );

        // staged 블록과 시드 지시는 단계 수를 두고 경쟁하지 않는다.
        assert!(!staged_block(true).contains("2~6개"));
        assert!(staged_block(true).contains("[작업 분해]"));
        assert!(staged_block(false).contains("2~6개"));
        assert!(!staged_block(false).contains("[작업 분해]"));

        // 계획 단계 수 추정 — 관측 경고의 입력.
        assert_eq!(plan_step_count(steps), 3);
        assert_eq!(plan_step_count("1) 첫째\n2) 둘째\n- 메모"), 2);
        assert_eq!(plan_step_count("단계 없음"), 0);
    }

    #[test]
    fn graph_retry_prompt_carries_first_attempt_changes() {
        let changed = vec!["src/board.rs".to_string(), "src/main.rs".to_string()];
        let p = graph_retry_prompt("노드 프롬프트", "status=limit", &changed);
        assert!(p.contains("노드 프롬프트"));
        assert!(p.contains("[직전 시도 실패] status=limit"));
        // resume 없는 재시도의 이중 편집 방지 (§15.5).
        assert!(p.contains("- src/board.rs"));
        assert!(p.contains("- src/main.rs"));
        assert!(p.contains("현재 상태를 읽고 이어가라"));
        // 바뀐 파일이 없으면 그 절은 통째로 빠진다.
        let p = graph_retry_prompt("노드 프롬프트", "status=fail", &[]);
        assert!(!p.contains("[첫 시도가 이미 바꾼 파일]"));

        // 노드 시스템 프롬프트도 선행 노드의 산출물 위에서 일하라고 못 박는다.
        let node = DagNode {
            id: "n2".into(),
            goal: "렌더링".into(),
            deps: vec![],
            produces: "화면".into(),
        };
        let sys = graph_node_system("BASE", 2, 3, &node, &[]);
        assert!(sys.contains("선행 노드가 바꾼 파일은 읽고"));
    }

    #[test]
    fn verify_retry_success_clears_final_failure_but_keeps_evidence() {
        let mut outcome = AgentOutcome {
            verify_fail: Some("cargo check 실패".into()),
            ..AgentOutcome::default()
        };
        mark_verify_recovered(&mut outcome);
        // 재시도로 통과한 실행은 최종 실패가 아니다 — Self-Harness 가 실패로 세면 안 된다.
        assert_eq!(outcome.verify_fail, None);
        // 그래도 회복 증거는 남아 교훈 수집 경로를 지킨다.
        assert_eq!(
            outcome.verify_recovered.as_deref(),
            Some("cargo check 실패")
        );

        // 애초에 실패가 없었으면 아무것도 만들지 않는다.
        let mut clean = AgentOutcome::default();
        mark_verify_recovered(&mut clean);
        assert_eq!(clean.verify_fail, None);
        assert_eq!(clean.verify_recovered, None);
    }

    #[test]
    fn repair_outcome_preserves_prior_execution_evidence() {
        let previous = AgentOutcome {
            status: "fail".into(),
            iterations: 20,
            input_tokens: 100,
            output_tokens: 10,
            cached_tokens: 30,
            changed_files: vec!["index.html".into()],
            tool_errors: vec!["first failure".into()],
            verify_fail: Some("syntax".into()),
            ..AgentOutcome::default()
        };
        let next = AgentOutcome {
            status: "ok".into(),
            iterations: 4,
            input_tokens: 40,
            output_tokens: 8,
            cached_tokens: 5,
            changed_files: vec!["index.html".into(), "game.js".into()],
            ..AgentOutcome::default()
        };

        let merged = merge_agent_outcomes(previous, next);
        assert_eq!(merged.status, "ok");
        assert_eq!(merged.iterations, 24);
        assert_eq!(merged.input_tokens, 140);
        assert_eq!(merged.output_tokens, 18);
        assert_eq!(merged.cached_tokens, 35);
        assert_eq!(merged.changed_files, vec!["index.html", "game.js"]);
        assert_eq!(merged.tool_errors, vec!["first failure"]);
        assert_eq!(merged.verify_fail.as_deref(), Some("syntax"));
    }

    #[test]
    fn harness_strategy_accepts_single_and_multi_only() {
        assert_eq!(
            HarnessStrategy::parse("single"),
            Some(HarnessStrategy::Single)
        );
        assert_eq!(
            HarnessStrategy::parse("multi"),
            Some(HarnessStrategy::Multi)
        );
        assert_eq!(HarnessStrategy::parse("manual"), None);
    }
}


#[cfg(test)]
mod committee_tests {
    #[test]
    fn committee_verdicts_require_all_five_groups() {
        use crate::harness::parse_committee_verdicts;
        let full = "[판정-정확성] pass\n[판정-보안] pass\n[판정-성능] pass\n[판정-가독성] pass\n[판정-API설계] pass";
        let (ok, fails) = parse_committee_verdicts(full);
        assert!(ok, "전원 통과: {fails:?}");

        let one_fail = "[판정-정확성] pass\n[판정-보안] pass\n[판정-성능] pass\n[판정-가독성] fail — src/a.rs:12 중복\n[판정-API설계] pass";
        let (ok, fails) = parse_committee_verdicts(one_fail);
        assert!(!ok);
        assert!(fails[0].contains("가독성"));

        // 그룹 누락도 실패다 — 판정 없음은 통과가 아니다.
        let missing = "[판정-정확성] pass\n[판정-보안] pass";
        let (ok, fails) = parse_committee_verdicts(missing);
        assert!(!ok);
        assert_eq!(fails.len(), 3, "누락 그룹만큼 사유: {fails:?}");
    }
}
