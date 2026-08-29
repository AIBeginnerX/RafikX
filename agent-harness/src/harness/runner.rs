use super::*;

/// oh-my-pi (github.com/can1357/oh-my-pi) 의 시스템 프롬프트 구성을 RafikX 에
/// 이식한 것: RFC 2119 규약 → 엔지니어링 원칙 → 증거 우선 간결 페르소나 →
/// 6단계 워크플로 → 전달 계약 → Critical. 답변 끝 행동 선택 메뉴는 금지한다.
pub fn system_prompt(cfg: &Config, extra: &str, lessons: &str) -> String {
    let mut s = format!(
        "You are RafikX, a coding agent for the terminal.\n\
         Workspace: {}\n\
         If the user writes in Korean, reply in Korean.\n\
         \n\
         [언어 정책]\n\
         - 작업 진행 설명은 영어로 쓴다(MUST): 작업 중 생각·중간 점검·단계 서술·도구 호출 앞뒤 안내는 전부 영문으로 출력한다.\n\
         - 다만 핵심 내용·진행 상황의 제목(헤더·섹션 제목)은 한국어로 쓴다.\n\
         - 최종 답변(작업 결과 브리핑)은 한국어로 쓴다(MUST).\n\
         \n\
         [규약]\n\
         RFC 2119 키워드를 쓴다: MUST(반드시), NEVER(절대 금지), SHOULD(권장), AVOID(지양), MAY(허용).\n\
         \n\
         [엔지니어링]\n\
         - 정확성이 먼저다. 그다음이 6개월 뒤의 유지보수성이다.\n\
         - 취향을 적용한다: 무게 없는 코드는 지우고, 불필요한 추상화는 거부하고, 지루한(boring) 해법을 선호한다. 설계는 철저하고 우아하게.\n\
         - 예상 밖의 저장소 변경은 사용자의 작업이다. 적응한다.\n\
         - 사용자의 말이 최우선이다: 사용자가 보고한 상태(오류·실패·관찰)는 ground truth 다. 그대로 근거로 행동하고, 이미 보고된 사실을 재확인하려고 검사를 다시 돌리지 않는다(NEVER).\n\
         - 같은 파일에서 edit_file/apply_patch 가 2회 연속 실패하면 부분 패치를 멈추고 그 파일을 읽어 전체를 write_file 로 재작성하라.\n\
         \n\
         [말투 — 증거 우선 간결 엔지니어]\n\
         - 모든 문장은 사실·결정·리스크 중 하나다. 의례·헤징·자기요약·필러·과장은 금지(NEVER).\n\
         - 명확하다면 완전한 문장 대신 조각 표현도 허용(MAY). 기술 독자를 가정하고, 뻔한 단계를 서술하거나 기초를 과잉 설명하지 않는다.\n\
         - 구체적으로: 정확한 파일·심볼·API·상태 필드·엣지케이스·검증 방법을 적는다.\n\
         - 결론 먼저, 증거 다음. 추론은 사실→제약→트레이드오프→결정→검증 순으로 압축한다.\n\
         - 불확실하면 주장하는 그 자리에서 밝히고, 트레이드오프에 이름을 붙이고, 안전한(boring) 쪽을 고른다.\n\
         - 형식은 요청에 맞춘다(MUST). 산문은 짧게, 증거·검증·차단 사유는 완전하게.\n\
         - 관측하지 않은 주장에는 [INFERENCE] 를 붙인다.\n\
         - 답변 끝에 다음 행동 선택지 목록이나 번호 메뉴를 붙이지 않는다(NEVER). 후속 제안이 정말 필요하면 한 문장으로 끝낸다.\n\
         - 리스크를 숨긴 계획이나 틀린 주장에는 반박한다: 리스크를 명명하고 증거를 보이고 대안을 제시한다. 사용자가 기각하면 그 결정을 실행하고 재논쟁하지 않는다.\n\
         \n\
         [워크플로]\n\
         1 Scope — 요청을 파악한다. 다중 파일 작업은 파일보다 계획이 먼저다.\n\
         2 Research — 편집 전에 조각이 아니라 구획을 읽는다. 기존 패턴을 재사용한다(MUST); 기존 관례 옆에 두 번째 관례를 만드는 것은 금지다. 도구 실패나 읽은 뒤 바뀐 파일은 다시 읽고 행동한다.\n\
         3 Decompose — todo 를 갱신한다. Harness가 단계별 실행(todo)을 지시하면 반드시 따른다(MUST); 그 지시가 없는 사소한 요청만 건너뛴다.\n\
         4 Implement — 원인을 고친다. 요청 없이 증상 억제·입력 특수케이스 처리 금지(NEVER). 이관은 깨끗하게: 모든 호출부를 옮기고 낡은 코드·별칭·재수출·주석을 제거한다. 새 파일보다 기존 파일 갱신을 선호한다.\n\
         5 Verify — 검증 없는 산출물 전달 금지(NEVER). 버그 수정은 재현→수정→재현 소멸 확인. 스모크는 테스트 파일이 아니라 실제 실행이다: 실행하고, 바뀐 경로를 통과시키고, 결과를 관찰한다.\n\
         6 Cleanup — 스모크가 작업을 증명한 뒤의 마지막 단계다. 실험·일회성 조사에는 테스트·문서 정리를 만들지 않는다.\n\
         \n\
         [의도 게이트]
         매 요청 시작에서 의도를 분류해 라우팅한다(MUST): 조사·설명 / 구현·생성 /
         조사보고 / 평가·제안 / 수정·디버깅 / 리팩터·개선 / 모호. 조사류는 파일을 읽고 답하고 편집하지 않는다.
         모호하면 맥락으로 해석 가능한 한 결정하고, 남은 분기점만 짧게 물어본다.
         진행 중 사용자 정정이 오면 옛 계획을 버리고 새 방향부터 다시 잡는다.
         
         [선임 엔지니어 사고관]
         복잡한 과제는 아래 6단계 역할 순환으로 풀고, 각 단계 성립을 확인하고 다음으로 넘어간다(MUST):
         1 업무분석 — 문제의 실체와 성공 기준을 한 문장으로 고정한다.
         2 설계 — 해법 구조와 트레이드오프를 결정하고, 더 우아한 대안이 없었는지 자문한다.
         3 작업 — 설계 그대로 최소 변화로 구현한다.
         4 리뷰 — 내 산출물을 타인 시각으로 비판한다: 누수·경계값·오류처리·보안·단순성.
         5 검증 — 실제 표면으로 직접 실행/테스트해 동작을 증명한다.
         6 최종판단 — 모든 수락 기준 충족 여부를 스스로 심사하고, 미통과면 돌아가 고친다.
         타입 에러·lint 경고·실패 테스트를 억제해서 통과시키는 것은 금지(NEVER)다. 검증 등급으로 정직하게 보고한다:
         V1=단일파일 비동작 변경(진단), V2=도메인 동작 변경(진단+관련 테스트+엔트리포인트 1회 실행),
         V3=복수파일·횡단 변경(진단+빌드+테스트+사용자 표면 수동 훈련).
         '통과할 것이다'는 검증이 아니다(NEVER): 실제 출력과 종료 코드로 증명한다.
         
         [증거·지식 신선도]
         검색 정보(web_search·webfetch)를 답변·계획·코드에 쓸 때 MUST:
         (a) 출처 URL을 밝힌다. (b) 발행/갱신 날짜를 확인해 함께 밝힌다.
         (c) 현재 연도와 어긋나는 오래된 정보는 그대로 쓰지 않고 최신 상태를 재검색해 확인한다.
         (d) 같은 사실이 두 개 독립 출처에 겹치면 신뢰도를 올린다. 학습 컷오프 이후 바뀌었을
         가능성이 있는 API·버전·요금·정책 주장은 검색 없이 하지 않는다(NEVER).
         
         [수학적 계획법]
         여러 후보 과제를 배열할 때 감이 아니라 수치로 정렬한다(SHOULD):
         · 우선순위 점수 = I×C×E: 영향도(Impact 1-10) × 확신도(Confidence 0-1) × 용이성(Ease 1-3),
           내림차순으로 todo 분해 순서를 정한다.
         · 리스크 기댓값 = P(실패) × 손실비용 — 높은 순서로 완화책을 먼저 준비한다.
         · 의존성은 DAG로 모델링한다: 순환 의존 금지(NEVER), 위상순서 배치, 동급 항목은 병렬화한다.
         · 자원 배분은 기대효과 순: 토큰·시간 예산을 점수 상위 단계에 두고
           (잔여예산/남은단계) 수렴을 유지하며 소모를 점검한다.
         · 새 증거가 기존 가설을 반박하면 확신도와 계획을 갱신한다.
         
         [스킬·MCP]
         · 스킬: 같은 절차를 2회 이상 반복하게 될 흐름(배포 절차, QA 순서 등)은 save_skill 로
           ~/.rafikx/skills/ 스킬로 저장하고, 이후 같은 절차는 load_skill 로 불러 일관되게 수행한다.
         · MCP 도구(mcp__<server>__<tool> 접두사)는 외부 서비스 연동 대상이다: 적합한 MCP 도구가
           있으면 내장 도구보다 우선 검토하고, 실패하면 근거와 함께 대안을 고른다.
         
         [스펙 우위 — 모호함은 실행 전에 죽인다]\n\
         - 여러 파일에 걸친 변경이나 새 기능 구현은 시작 전에 완료 기준을 검증 가능한 문장으로 합의한다(MUST). 해석 후보가 둘 이상이면 영향도 순으로 질문을 최대 5개까지 한 번에 제시한다.\n\
         - 질문하지 않고 진행한 판단은 '가정: …' 형식으로 명시한다(NEVER 생략). 가정 목록은 답변 마지막에 모은다.\n\
         - 사용자가 확인한 완료 기준을 임의로 바꾸지 않는다(NEVER). 바뀌어야 하는 상황이면 변경 사유를 먼저 밝히고 승인을 받는다.\n\
         [증거 우위 — 거짓 완료 원천 차단]\n\
         - 완료나 통과를 주장하는 문장은 검증 명령의 exit code 없이는 증거가 아니다(NEVER). 실행하지 않은 테스트의 통과를 서술하면 그것은 보고가 아니라 추측이다.\n\
         - 테스트를 약화시키지 않는다(NEVER): #[ignore] 추가, 테스트 함수·어서션 삭제, 기대값 완화는 금지. 깨지는 테스트는 고치거나, 고칠 수 없으면 실패 사실과 원인을 정직히 보고한다. 실패 보고는 완료 주장보다 가치가 높다.\n\
         - 파일을 변경하지 않고 작업을 완료했다고 말하지 않는다(NEVER). 변경이 불필요하다는 결론 자체가 산출물이면 그 근거를 명시한다.\n\
         - 작게 만들고 즉시 검증한다(MUST): 검증되지 않은 변경을 쌓지 않는다. 한 변경 = 하나의 검증 가능한 단위.\n\
         [전달 계약]\n\
         - 완전한 산출물 전에 멈추지 않는다(NEVER). 단계 경계·todo 전환·중간 단계는 멈출 이유가 아니다: 같은 턴에서 계속한다.\n\
         - 출력을 지어내지 않는다(NEVER): 코드·도구·테스트·문서에 대한 주장은 근거가 있어야 한다.\n\
         - 더 쉬운 문제로 바꿔치기하지 않는다(NEVER): 요청에 없는 재시도·검증·텔레메트리·추상화를 멋대로 더하지 않고, 증상만 가리지 않는다. 실제 요청만 푼다.\n\
         - '완료' = 명세된 end-to-end 동작 + 모든 수락 기준. 스텁·플레이스홀더·mock·no-op·'TODO: implement'·'일단 MVP' 는 미완이다(NEVER). 범위 축소는 이 대화에서 사용자의 명시적 승인이 있을 때만.\n\
         - 도구·저장소·파일로 알 수 있는 정보를 사용자에게 묻지 않는다(NEVER). 반쯤 푼 작업을 떠넘기지 않는다.\n\
         - blocked 선언 전에 도구와 컨텍스트로 정말 확인 불가한지 먼저 확인한다. 검사 1회 실패는 blocked 가 아니다. 도달 가능한 작업은 끝내고, 정확히 무엇이 없고 무엇을 시도했는지 밝힌다.\n\
         \n\
         [안전]\n\
         - 커밋과 배포는 사용자가 명시적으로 요청하기 전에는 실행하지 않는다(NEVER).\n\
         - 내가 만들지 않은 무관한 코드 삭제·파괴적 git 명령 전에는 확인을 받는다. 이관이 낡게 만든 코드는 범위 안이다.\n\
         - task 위임은 사용자가 병렬을 요청했거나 진짜 독립 슬라이스가 있을 때만 쓴다. 최상위 계획을 위임하지 않는다.\n\
         \n\
         [Harness 표기]\n\
         - 수치 비교는 ```chart 블록(한 줄에 '라벨: 수치')으로 — 터미널이 실제 막대그래프로 렌더링한다. ASCII 아트 도표(+---+, ->, 문자 박스)는 금지.\n\
         - 항목 나열은 마크다운 표를 쓴다.\n\
         {extra}",
        cfg.workspace.display()
    );
    // OMO 의 Rules Injection 수용: 워크스페이스 규칙 파일을 자동 주입 (경량 상한 8K).
    for fname in ["AGENTS.md", "RAFIKX.md"] {
        let p = cfg.workspace.join(fname);
        if let Ok(body) = std::fs::read_to_string(&p) {
            let trimmed: String = body.trim().chars().take(8000).collect();
            if !trimmed.is_empty() {
                s.push_str(&format!("\n\n[프로젝트 규칙 — {fname}]\n{trimmed}"));
            }
            break;
        }
    }
    if !lessons.trim().is_empty() {
        s.push('\n');
        s.push_str(lessons.trim_end());
    }
    // 스킬 인젝션: 반복 절차의 재사용 목록(없으면 생략).
    if let Some(sec) = crate::skills::prompt_section(&cfg.workspace) {
        s.push_str("\n\n");
        s.push_str(&sec);
    }
    s
}

#[allow(clippy::too_many_arguments)] // 공개 파이프라인 API: 호출부 호환을 위해 시그니처 유지
pub async fn run_pipeline(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    yes: bool,
    cli_provider: Option<&str>,
    resume: Option<Vec<Message>>,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
) -> Result<AgentOutcome> {
    let run_id =
        crate::graph::current_run().unwrap_or_else(|| format!("run-{}", crate::db::Db::new_id()));
    let context = RunContext::for_config(RunId::new(run_id), Arc::new(cfg.clone()))
        .with_live_sink(crate::ui::current_live_sink());
    run_pipeline_with_context(
        cfg,
        binding,
        task,
        yes,
        cli_provider,
        resume,
        remote,
        local_ask,
        context,
    )
    .await
}

#[allow(clippy::too_many_arguments)] // 공개 파이프라인 API: 호출부 호환을 위해 시그니처 유지
pub async fn run_pipeline_with_context(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    yes: bool,
    cli_provider: Option<&str>,
    resume: Option<Vec<Message>>,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    let start_event = if binding.plan_first {
        crate::lifecycle::LifecycleEventData::PlanningStarted
    } else {
        crate::lifecycle::LifecycleEventData::RunStarted {
            model: Some(binding.model.clone()),
        }
    };
    let _ = run_context.transition_lifecycle(start_event);
    // working 패널의 메인 줄 — 위임 자식(agent_id 보유)은 task.rs 가 자기 역할명으로
    // 발신하므로 여기서는 루트 실행만 연다 (§16.2).
    let worker = run_context
        .agent_id()
        .is_none()
        .then(|| crate::ui::worker_id(&run_context));
    if let Some(id) = &worker {
        crate::ui::live_worker_in(
            &run_context,
            id,
            &binding.profile_name,
            &format!("{}/{}", binding.provider_name, binding.model),
            "시작",
            false,
        );
    }
    let result = run_pipeline_inner(
        cfg,
        binding,
        task,
        yes,
        cli_provider,
        resume,
        remote,
        local_ask,
        run_context.clone(),
    )
    .await;
    match &result {
        Ok(outcome) => {
            let state = match outcome.status.as_str() {
                "ok" => TerminalState::Succeeded,
                "cancelled" => TerminalState::Cancelled,
                "limit" | "incomplete" => TerminalState::Limited,
                _ => TerminalState::Failed,
            };
            run_context.finish_with_error(state, outcome.error.clone());
        }
        Err(error) => {
            run_context.finish_with_error(TerminalState::Failed, Some(error.to_string()));
        }
    }
    if let Some(id) = &worker {
        crate::ui::live_worker_in(&run_context, id, "", "", "", true);
    }
    result
}

/// 계획 호출은 메인 system 을 그대로 이어받고 이 머리말만 덧붙인다.
pub(crate) const PLAN_MODE_HEADER: &str = "\n\n[계획 모드] 지금은 계획만 세운다. 도구는 쓰지 마라.\n";

const PLAN_BRIEF_INSTRUCTION: &str = "작업 계획을 3~7개 항목으로만 출력하라.";

/// PlanDepth::Contract — 산출물을 3부로 강제한다 (dev/advanced 클래스에서만 활성).
pub(crate) const PLAN_CONTRACT_INSTRUCTION: &str = "\
    20년 경력 시니어가 착수 전 검토하듯 계획하라: 기존 코드·파일 구조에 대한 가정을 명시하고, \
    위험(호환성·회귀·엣지케이스)을 한 줄씩 짚는다.\n\
    출력은 반드시 아래 세 부분으로만 구성한다. 머리표는 대괄호 그대로 쓴다.\n\
    [해석] 요구사항을 한 문단으로 재진술하고, 모호한 점과 그중 채택한 해석을 밝힌다.\n\
    [완료 기준] 검증 가능한 체크리스트 3~10항목. 각 항목은 '무엇이 충족되어야 하는가'와 \
    '어떻게 확인하는가(명령·파일·관찰 대상)'를 함께 적는다.\n\
    [작업 분해] 실행 순서 3~9단계. 각 단계는 한 줄로 적고 결과물을 명시한다.\n\
    [매핑] 완료 기준(AC) 항목마다 어떤 단계가 충족하는지 표시하라 — 매핑되지 않은 기준이 하나라도 있으면 계획은 불완전하다(MUST).
    [반박] 이 계획을 실패시킬 가장 유력한 위험 1개와 그것을 조기 확인할 최소 테스트 1개.
    [질문] 실행을 바꿀 수 있는 분기점이 남아 있을 때만 한두 개로 모은다. 사소한 모호함은     [해석]의 채택 해석으로 처리하고 묻지 않는다(확인 연극 금지). 분기점이 없으면 '없음'이라고만 적는다.";

/// discipline = loop — 종료 조건을 시스템 프롬프트에 못 박는다.
pub(crate) const LOOP_DISCIPLINE_RULE: &str =
    "\n\n[루프 규율] 모든 todo 완료 + 검증 통과 전에는 완료를 선언하지 마라.";

/// discipline = loop — 정체를 감지한 사이클의 continuation 에 덧붙이는 전략 전환 지시.
pub(crate) const LOOP_STALE_SWITCH: &str = "\n직전 사이클에서 진전이 없었다. 현재 todo를 더 작은 단위로 \
     쪼개거나 다른 도구/경로로 전환하라. 같은 접근의 반복을 금지한다.";

/// discipline = graph — 계획이 완료 기준과 노드 DAG 를 함께 산출한다.
/// JSON 은 산문 뒤에 와야 한다: [완료 기준] 추출이 첫 `{` 앞까지만 훑기 때문이다.
pub(crate) const PLAN_GRAPH_INSTRUCTION: &str = "\
    작업을 상태 그래프로 분해하라. 출력은 아래 두 부분으로만, 이 순서로 구성한다.\n\
    먼저 [완료 기준] 절: 검증 가능한 체크리스트 3~10항목. 각 항목에 '어떻게 확인하는가'를 함께 적는다.\n\
    그다음 노드 DAG 를 JSON 한 덩어리로 적는다. JSON 뒤에는 아무 설명도 붙이지 않는다.\n\
    {\"nodes\":[{\"id\":\"n1\",\"goal\":\"이 노드에서 끝낼 일\",\"deps\":[],\"produces\":\"산출물 한 줄\"}]}\n\
    - 노드는 3~7개. id 는 짧게, goal 과 produces 는 한국어로 쓴다.\n\
    - deps 에는 먼저 끝나야 하는 노드의 id 만 넣는다. 순환은 금지(NEVER)다.\n\
    - 각 노드는 신선한 컨텍스트에서 따로 실행된다. goal 만 읽고도 무엇을 할지 알 수 있게 쓴다.";

/// 계획 호출용 시스템 프롬프트 — 메인 system 조립 결과를 그대로 이어받고 계획 모드
/// 지시만 덧붙인다. lessons·system_extra·프로젝트 규칙(AGENTS.md)·엔진 블록이
/// 계획에도 반영되어야 하므로 절대 통째로 교체하지 않는다.
/// `plan_extra` 는 Self-Harness 의 계획 전용 면(plan_instruction) — 메타 레이어가
/// 켜졌을 때만 채워지고, decorate_system 이 아니라 여기서만 붙는다.
pub(crate) fn plan_system_prompt(system: &str, depth: crate::engine::PlanDepth, plan_extra: &str) -> String {
    plan_system_prompt_with(
        system,
        if depth == crate::engine::PlanDepth::Contract {
            PLAN_CONTRACT_INSTRUCTION
        } else {
            PLAN_BRIEF_INSTRUCTION
        },
        plan_extra,
    )
}

/// 계획 지시만 갈아끼우는 공통 조립 — depth 별 지시와 graph 분야 지시가 함께 쓴다.
pub(crate) fn plan_system_prompt_with(system: &str, instruction: &str, plan_extra: &str) -> String {
    let mut s = String::with_capacity(system.len() + instruction.len() + 64);
    s.push_str(system);
    s.push_str(PLAN_MODE_HEADER);
    s.push_str(instruction);
    let extra = plan_extra.trim();
    if !extra.is_empty() {
        s.push_str("\n\n[Self-Harness 계획 지침] ");
        s.push_str(extra);
    }
    s
}

/// 단계별 처리 지시 (순수 함수). 계약형 계획이면 단계 수를 계획의 [작업 분해]가
/// 지배하므로 "2~6개" 같은 경쟁 지시를 내지 않는다 (설계 §15.3).
pub(crate) fn staged_block(contract_plan: bool) -> &'static str {
    if contract_plan {
        "\n\n[실행 방식 — 단계별 처리]\n\
         이 작업은 여러 단계가 필요하다. 다른 도구를 쓰기 전에 먼저 todo_write 로 계획의 [작업 분해] 단계들을 등록하고, \
         한 단계를 마칠 때마다 todo_write 로 상태를 갱신하라. \
         모든 단계가 끝나면 단계별 핵심 결과를 짧게 요약해 답을 마친다."
    } else {
        "\n\n[실행 방식 — 단계별 처리]\n\
         이 작업은 여러 단계가 필요하다. 다른 도구를 쓰기 전에 먼저 todo_write 로 2~6개의 실행 단계를 등록하고, \
         한 단계를 마칠 때마다 todo_write 로 상태를 갱신하라. \
         모든 단계가 끝나면 단계별 핵심 결과를 짧게 요약해 답을 마친다."
    }
}

/// 첫 사용자 메시지 조립 (순수 함수) — 계약형 계획의 [작업 분해] 본문을 그대로 싣는다.
/// system 안 [실행 계획]을 가리키는 원거리 참조는 실측에서 시드가 통째로 누락되는
/// 원인이었다 (설계 §15.3).
pub(crate) fn contract_seed_task(task: &str, plan_steps: &str) -> String {
    let steps = plan_steps.trim();
    if steps.is_empty() {
        return task.to_string();
    }
    format!(
        "{task}\n\n[착수 지시] 아래 [작업 분해]의 단계들을 먼저 todo_write 로 등록한 뒤 \
         첫 항목부터 실행하라. 항목을 마칠 때마다 상태를 갱신한다.\n\n[작업 분해]\n{steps}"
    )
}

/// `[작업 분해]` 절의 단계 수 추정 (순수 함수) — "1." 처럼 숫자로 시작하는 줄을 센다.
/// 계획 단계 수와 실제 등록된 todo 수가 어긋났는지 관측하는 데만 쓴다(강제 없음).
pub(crate) fn plan_step_count(steps: &str) -> usize {
    steps
        .lines()
        .filter(|line| {
            let t = line.trim_start();
            let digits = t.chars().take_while(char::is_ascii_digit).count();
            digits > 0 && t[digits..].starts_with(['.', ')'])
        })
        .count()
}

/// 계획 텍스트에서 `[머리표]` 절만 잘라낸다 — 다음 `[`로 시작하는 줄 직전까지.
pub(crate) fn extract_plan_section(plan: &str, header: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in plan.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(header) {
            inside = true;
            let rest = rest.trim();
            if !rest.is_empty() {
                out.push(rest);
            }
            continue;
        }
        if inside {
            if trimmed.starts_with('[') {
                break;
            }
            out.push(line);
        }
    }
    out.join("\n").trim().to_string()
}

#[allow(clippy::too_many_arguments)] // 공개 파이프라인 API: 호출부 호환을 위해 시그니처 유지
async fn run_pipeline_inner(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    yes: bool,
    cli_provider: Option<&str>,
    resume: Option<Vec<Message>>,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    run_context
        .metrics()
        .set_context_window(binding.context_window);
    if run_context.is_cancelled() {
        return Ok(cancelled_outcome());
    }
    let role = cfg
        .file
        .subagents
        .get(&binding.profile_name)
        .map(|s| s.model_role.as_str())
        .unwrap_or("main");
    let order = if binding.combo_chain.is_empty() {
        fallback_order_pinned(cfg, &binding.provider_name, cli_provider)
    } else {
        let mut v: Vec<String> = Vec::new();
        for (p, _) in &binding.combo_chain {
            if !v.contains(p) {
                v.push(p.clone());
            }
        }
        v
    };
    let lessons_block = if cfg.file.memory.enabled {
        Db::open(&Db::db_path()?)
            .ok()
            .map(|db| {
                crate::lessons::inject_block_for_project(
                    &db,
                    &cfg.workspace,
                    task,
                    cfg.file.memory.inject_limit_chars as usize,
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    if !lessons_block.is_empty() {
        crate::applog::info(&format!("lessons inject:\n{lessons_block}"));
        crate::graph::node_in(
            &run_context,
            "pre_step",
            "lessons",
            "injected",
            Some("bind"),
        );
    } else {
        crate::graph::node_in(&run_context, "pre_step", "lessons", "none", Some("bind"));
    }
    let facts_block = if cfg.file.memory.enabled {
        Db::open(&Db::db_path()?)
            .ok()
            .map(|db| {
                crate::facts::inject_block(
                    &db,
                    &cfg.workspace,
                    cfg.file.memory.inject_limit_chars as usize,
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    // 프로젝트 규칙 주입 (F7) — AGENTS.md·.rafikx/rules 를 매 요청 싣는다.
    let rules_block = crate::rules::collect_rules(&cfg.workspace);
    if !rules_block.is_empty() {
        crate::graph::node_in(
            &run_context,
            "pre_step",
            "rules",
            &format!("프로젝트 규칙 주입 ({}자)", rules_block.chars().count()),
            None,
        );
    }
    let memory_block = format!("{lessons_block}{facts_block}{rules_block}");
    let mut system = system_prompt(cfg, &binding.system_extra, &memory_block);
    crate::context::record_system_sources(&run_context, cfg, &system, &lessons_block);
    system.push_str(&format!(
        "\n\n[현재 실행 정보]\nProvider: {}\nModel: {}\nContext window: {} tokens\nHarness: {}\n\
         사용자가 현재 provider·model·context window·엔진·팀 모드·분야 등 Harness 설정을 물으면 \
         도구를 쓰지 말고 이 값을 그대로 답한다.",
        binding.provider_name,
        binding.model,
        binding.context_window,
        mode_line(cfg)
    ));

    // 난이도 기반 단계별 실행 (dsh ctx.goals 영향 수용):
    // 단순 업무는 즉답, medium 이상은 todo 스테이징. force_staged 엔진(deepseek)은
    // 모든 도구 작업에 적용. 엔진 차이는 EngineSpec 데이터로만 표현한다.
    let (engine_name, _legacy_self) = crate::engine::normalize(&cfg.file.general.engine);
    let spec = crate::engine::resolve_with(&cfg.file.engines, &engine_name);
    // Self-Harness 는 엔진 위에 겹치는 메타 레이어다 — legacy engine="self" 또는
    // [self_harness] meta = true. 관찰 경로와 같은 판정을 쓴다.
    let self_meta_on = crate::self_harness::meta_active(cfg);
    // 실행 분야 — 엔진(품질 장치)과 직교하는 축이다. 제어 전략만 바꾼다.
    let discipline = crate::engine::normalize_discipline(&cfg.file.general.discipline);
    // 그래프 분야는 도구를 쓰는 설계·개발 작업에서만 발동한다. 그 밖은 harness 와 동일.
    let graph_mode = discipline == crate::engine::Discipline::Graph
        && matches!(binding.class, TaskClass::Dev | TaskClass::Advanced)
        && !binding.tools.is_empty();
    // 그래프는 노드가 단계 역할을 하므로 전역 todo 스테이징을 강제하지 않는다.
    let staged = !binding.tools.is_empty()
        && (spec.force_staged || binding.class != crate::harness::TaskClass::Simple)
        && !graph_mode;
    // 계획 깊이는 staged 블록 문구보다 먼저 정해져야 한다 — 계약형 계획이면 단계 수를
    // 계획의 [작업 분해]가 지배하므로 여기서 "2~6개" 같은 경쟁 지시를 내지 않는다(§15.3).
    // Contract 깊이는 dev/advanced 클래스에서만 활성하고 그 밖은 Brief 로 낮춘다.
    let plan_depth = match spec.plan_depth {
        crate::engine::PlanDepth::Contract
            if !matches!(binding.class, TaskClass::Dev | TaskClass::Advanced) =>
        {
            crate::engine::PlanDepth::Brief
        }
        other => other,
    };
    let contract_plan = plan_depth == crate::engine::PlanDepth::Contract;
    if staged {
        // 배너 없이 조용히 — 단계 진행은 Todo 패널이 보여준다 (pi 저소음).
        crate::tools_more::clear_todos_in(&run_context);
        crate::graph::node_in(
            &run_context,
            "pre_step",
            "staging",
            if spec.force_staged {
                spec.name.as_ref()
            } else {
                "auto"
            },
            Some("bind"),
        );
        system.push_str(staged_block(contract_plan));
    }
    // 엔진 프롬프트 블록 — 각 Harness의 품질 장치를 한 지점에서 주입한다.
    if !spec.prompt_block.is_empty() {
        system.push_str(&spec.prompt_block);
    }
    // loop 분야 — 종료 조건을 명시해 조기 완료 선언을 막는다 (Ralph 루프 계열).
    if discipline == crate::engine::Discipline::Loop {
        system.push_str(LOOP_DISCIPLINE_RULE);
    }
    // 팀 모드 — 독립 단계를 역할 서브에이전트로 위임하게 한다 (graph 와 상호 배타).
    let team = team_mode(cfg);
    if team_block_active(team, binding.class, !binding.tools.is_empty(), graph_mode) {
        system.push_str(TEAM_MULTI_BLOCK);
        crate::graph::node_in(
            &run_context,
            "pre_step",
            "team",
            team.as_str(),
            Some("bind"),
        );
    }
    // Self-Harness (arXiv:2606.09498) — 자기개선 루프가 유지하는 Harness 상태를
    // 시스템 프롬프트와 런타임 제어에 반영한다. 상태는 에피소드 관찰이 갱신한다.
    // 엔진 지시 뒤에 붙어 학습된 지시가 우선하게 한다.
    let mut effective_max_iter = binding.max_iterations;
    // 계획 호출 전용 면 — 메타 레이어가 켜졌을 때만 채워져 plan_system_prompt 로 넘어간다.
    let mut sh_plan_instruction = String::new();
    if self_meta_on {
        let sh = crate::self_harness::SelfHarnessState::load();
        crate::ui::live_line_in(
            &run_context,
            &format!(
                "[Harness] self-harness v{} — 자기개선 루프 활성{}",
                sh.version,
                sh.trial
                    .as_ref()
                    .map(|t| format!(" · trial #{} 검증 중", t.candidate_id))
                    .unwrap_or_default()
            ),
        );
        crate::graph::node_in(
            &run_context,
            "pre_step",
            "self_harness",
            &format!("v{}", sh.version),
            Some("bind"),
        );
        sh.decorate_system(&mut system);
        sh_plan_instruction = sh.plan_instruction();
        if let Some(cap) = sh.effective_iter_cap() {
            effective_max_iter = effective_max_iter.min(cap).max(1);
        }
    }

    // 계획 단계 — 메인 system 을 그대로 이어받아 lessons·system_extra·프로젝트 규칙·
    // 엔진 지시가 계획에도 반영되게 한다 (system 을 통째로 교체하던 결함 수정).
    // 계획이 산출한 DoD 체크리스트 — 독립 검증자 게이트(§5)의 입력.
    let mut dod_checklist = String::new();
    // 계획을 죽일 가장 유력한 위험(paperthin hate) — DoD 와 함께 게이트로 넘어간다.
    let mut rebuttal = String::new();
    // Contract 계획의 [작업 분해] 본문 — 비어 있지 않으면 첫 사용자 메시지에 그대로
    // 복사해 넣는다 (system 안 [실행 계획]을 가리키는 원거리 참조 제거, §15.3).
    let mut plan_steps = String::new();
    // graph 분야가 실행할 노드 DAG — 계획이 형식을 지켰을 때만 채워진다.
    let mut dag: Option<(Vec<DagNode>, Vec<usize>)> = None;
    // 그래프는 계획 산출물(DAG) 없이는 성립하지 않으므로 계획 호출을 반드시 지난다.
    if (binding.plan_first || graph_mode) && plan_depth != crate::engine::PlanDepth::Off {
        let plan_budget: u32 = if contract_plan || graph_mode {
            2048
        } else {
            1024
        };
        let req = ChatRequest {
            model: binding.model.clone(),
            system: if graph_mode {
                plan_system_prompt_with(&system, PLAN_GRAPH_INSTRUCTION, &sh_plan_instruction)
            } else {
                plan_system_prompt(&system, plan_depth, &sh_plan_instruction)
            },
            messages: vec![Message::user_text(task)],
            tools: vec![],
            max_tokens: plan_budget,
            // 계획은 수십 초가 걸린다 — 비스트리밍이면 그 구간이 통째로 침묵한다.
            stream: true,
        };
        crate::ui::live_line_in(&run_context, &format!("[계획 수립 중 · {}]", binding.model));
        let mut plan_streamed = false;
        let on_plan_event = |ev: StreamEvent| match ev {
            StreamEvent::Text(piece) => {
                plan_streamed = true;
                crate::ui::live_chunk_in(&run_context, piece);
            }
            StreamEvent::ToolArgs { name, total_bytes } => {
                crate::ui::live_status_in(&run_context, &tool_args_label(name, total_bytes))
            }
        };
        let plan_call = if binding.combo_chain.is_empty() {
            stream_with_fallback(cfg, &order, role, req, on_plan_event).await
        } else {
            stream_with_fallback_combo(cfg, &binding.combo_chain, role, req, on_plan_event).await
        };
        match plan_call {
            Ok((_n, resp)) => {
                crate::graph::node_in(&run_context, "plan", "plan_first", "", Some("pre_step"));
                if plan_streamed {
                    // 이미 흘러나간 계획을 다시 찍지 않는다 — 줄바꿈으로만 끊는다.
                    crate::ui::live_chunk_in(&run_context, "\n");
                    crate::ui::live_line_in(&run_context, "[계획] 수립 완료");
                } else {
                    crate::ui::live_line_in(&run_context, "[계획]");
                    for b in &resp.content {
                        if let ContentBlock::Text { text } = b {
                            crate::ui::live_assistant_in(
                                &run_context,
                                &format!("[모델 작업]\n{text}\n[/모델 작업]"),
                            );
                        }
                    }
                }
                let plan = resp
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.trim()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !plan.is_empty() {
                    crate::context::record_plan(&run_context, &plan, plan_budget);
                    run_context.emit(
                        crate::run::RunEventKind::Plan,
                        serde_json::json!({"plan": plan}),
                    );
                    system.push_str("\n\n[실행 계획]\n");
                    system.push_str(&plan);
                    system.push_str(
                        "\n이 계획을 실행 상태의 기준으로 사용하되, 새 증거가 생기면 안전하게 조정하라.",
                    );
                    if graph_mode {
                        // DAG JSON 과 별개로 [완료 기준] 절도 요구한다 — 게이트의 입력.
                        dod_checklist = extract_plan_section(plan_prose(&plan), "[완료 기준]");
                        if dod_checklist.trim().is_empty() {
                            // 조용한 약화 방지(§15.5): 게이트가 원 작업만으로 판정하게 된다.
                            crate::ui::live_warn_in(
                                &run_context,
                                "그래프 계획에서 [완료 기준]을 읽지 못했습니다 — 검증자는 원 작업만으로 대조합니다.",
                            );
                        }
                        dag = match parse_dag(&plan) {
                            Some(nodes) => match topo_order(&nodes) {
                                Ok(order) => Some((nodes, order)),
                                Err(cycle) => {
                                    crate::ui::live_warn_in(
                                        &run_context,
                                        &format!(
                                            "그래프 폴백: {cycle} — 기본 파이프라인으로 진행합니다."
                                        ),
                                    );
                                    None
                                }
                            },
                            None => {
                                crate::ui::live_warn_in(
                                    &run_context,
                                    "그래프 폴백: 계획에서 노드 DAG 를 읽지 못했습니다 — 기본 파이프라인으로 진행합니다.",
                                );
                                None
                            }
                        };
                    } else if contract_plan {
                        dod_checklist = extract_plan_section(&plan, "[완료 기준]");
                        rebuttal = extract_plan_section(&plan, "[반박]");
                        plan_steps = extract_plan_section(&plan, "[작업 분해]");
                        let intent_q = extract_plan_section(&plan, "[질문]");
                        if !intent_q.is_empty() && intent_q.trim() != "없음" {
                            crate::graph::node_in(
                                &run_context,
                                "pre_step",
                                "intent_gate",
                                &format!("실행 전 확인이 필요한 분기점: {intent_q}"),
                                None,
                            );
                        }
                    }
                }
            }
            Err(e) => {
                crate::ui::live_warn_in(&run_context, &format!("계획 단계 실패(계속 진행): {e}"))
            }
        }
        let _ =
            run_context.transition_lifecycle(crate::lifecycle::LifecycleEventData::RunStarted {
                model: Some(binding.model.clone()),
            });
    }

    let use_tools = !binding.tools.is_empty();
    if !use_tools {
        let mut messages = resume.unwrap_or_else(|| vec![Message::user_text(task)]);
        messages = crate::packer::pack_messages(
            &messages,
            &system,
            &[],
            binding.context_window,
            cfg.file.general.max_tokens,
            cfg.file.general.max_context_chars,
        );
        let req = ChatRequest {
            model: binding.model.clone(),
            system,
            messages: messages.clone(),
            tools: vec![],
            max_tokens: cfg.file.general.max_tokens,
            stream: true,
        };
        let on_main_event = |ev: StreamEvent| match ev {
            StreamEvent::Text(piece) => crate::ui::live_chunk_in(&run_context, piece),
            StreamEvent::ToolArgs { name, total_bytes } => {
                crate::ui::live_status_in(&run_context, &tool_args_label(name, total_bytes))
            }
        };
        type MainFuture<'a> = std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(String, ChatResponse)>> + Send + 'a>,
        >;
        let response: MainFuture<'_> = if binding.combo_chain.is_empty() {
            Box::pin(stream_with_fallback(cfg, &order, role, req, on_main_event))
        } else {
            Box::pin(stream_with_fallback_combo(cfg, &binding.combo_chain, role, req, on_main_event))
        };
        tokio::pin!(response);
        let (_name, resp) = tokio::select! {
            result = &mut response => result?,
            _ = run_context.cancelled_reason() => return Ok(cancelled_outcome()),
        };
        let _ = run_context.transition_lifecycle(crate::lifecycle::LifecycleEventData::Tokens {
            input: resp.input_tokens,
            output: resp.output_tokens,
            cached: resp.cached_tokens,
        });
        crate::graph::node_in(
            &run_context,
            "request",
            &binding.model,
            &format!("in={} out={}", resp.input_tokens, resp.output_tokens),
            Some("pre_step"),
        );
        crate::ui::live_chunk_in(&run_context, "\n");
        crate::ui::live_status_in(
            &run_context,
            &format!(
                "[tokens] in={} out={} stop={:?}",
                resp.input_tokens, resp.output_tokens, resp.stop_reason
            ),
        );
        messages.push(Message {
            role: crate::provider::Role::Assistant,
            content: resp.content.clone(),
        });
        // 도구 없는 프로파일의 응답에 tool-call 문법이 텍스트로 새어 있으면 —
        // 분류가 낮게 잡혔지만 실제로는 도구가 필요한 작업이다. 오염 응답을 걷어내고
        // coder 로 1회 승격해 다시 실행한다. (아래 본 경로의 승격과 같은 규칙 —
        // 이 조기 반환 경로는 그 검사에 도달하지 못해 v1.0 부터 누출이 그대로
        // 화면에 노출됐다: 2026-08-27 실측.)
        if leaked_tool_call(&agent::assistant_text(&messages)) {
            crate::ui::live_line_in(
                &run_context,
                "도구가 필요한 작업으로 판단 — coder 로 승격해 다시 실행합니다.",
            );
            if let Ok(dev) = bind(cfg, TaskClass::Dev, cli_provider, None)
                && !dev.tools.is_empty()
            {
                let mut clean = messages.clone();
                while matches!(clean.last(), Some(m) if m.role == crate::provider::Role::Assistant)
                {
                    clean.pop();
                }
                return Box::pin(run_pipeline_inner(
                    cfg,
                    &dev,
                    task,
                    yes,
                    cli_provider,
                    Some(clean),
                    remote,
                    local_ask,
                    run_context.clone(),
                ))
                .await;
            }
        }
        let _ =
            run_context.transition_lifecycle(crate::lifecycle::LifecycleEventData::AnswerStarted);
        let hit_token_limit = resp.stop_reason == StopReason::MaxTokens;
        return Ok(AgentOutcome {
            status: if hit_token_limit {
                "incomplete".into()
            } else {
                "ok".into()
            },
            iterations: 1,
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            context_tokens: resp.input_tokens,
            cached_tokens: resp.cached_tokens,
            cache_reported: resp.cache_reported,
            error: hit_token_limit.then(|| "모델 출력 토큰 상한에 도달했습니다.".into()),
            messages,
            changed_files: vec![],
            tool_errors: vec![],
            deny_reasons: vec![],
            verify_fail: None,
            verify_recovered: None,
        });
    }

    if !cfg.workspace.exists() {
        std::fs::create_dir_all(&cfg.workspace)?;
        crate::ui::live_line_in(
            &run_context,
            &format!(
                "워크스페이스 폴더를 만들었습니다: {}",
                cfg.workspace.display()
            ),
        );
    }

    // Contract 계획의 [작업 분해]를 todo 로 옮겨 staged goal continuation 과 결합한다.
    // 첫 사용자 메시지에만 붙는다 (이어하기에는 resume 이 우선).
    let agent_task = contract_seed_task(task, &plan_steps);

    // 그래프 분야 — 위상순 노드 실행이 전역 goal continuation 루프를 대체한다.
    // 검증·게이트는 그래프 전체가 끝난 뒤 합산 outcome 위에서 1회만 돈다.
    if let Some((nodes, node_order)) = dag {
        crate::graph::node_in(
            &run_context,
            "plan",
            "graph",
            &format!("{}개 노드", nodes.len()),
            Some("plan_first"),
        );
        let mut outcome = run_graph_discipline(
            cfg,
            binding,
            task,
            &nodes,
            &node_order,
            &system,
            yes,
            remote.clone(),
            local_ask.clone(),
            run_context.clone(),
        )
        .await?;
        outcome = finish_verification(
            cfg,
            binding,
            &spec,
            task,
            &dod_checklist,
            &rebuttal,
            yes,
            &system,
            outcome,
            remote,
            local_ask,
            run_context,
        )
        .await?;
        if self_meta_on {
            crate::self_harness::maybe_observe(cfg, task, &outcome);
        }
        return Ok(outcome);
    }

    // loop 분야는 계속 실행 한도를 엔진 값 +4(상한 12)로 늘린다.
    let max_continuations =
        crate::engine::max_continuations_for(discipline, spec.max_continuations);
    // 계획이 제시한 단계 수 — 첫 goal 판정에서 todo 총계와 대조만 한다(§15.3 관측).
    let plan_step_total = plan_step_count(&plan_steps);
    let mut next_resume = resume;
    let mut continuations = 0u8;
    // 폴 fallback 아키텍트 예산 — 턴당 최대 1회 (무한 루프 금지, F6).
    let mut fallback_budget: u8 = if cfg.file.fallback.enabled { 1 } else { 0 };
    let mut stale_rounds = 0u8;
    let mut previous_progress: Option<(usize, usize)> = None;
    let mut total_input = 0u32;
    let mut total_output = 0u32;
    let mut total_iterations = 0u32;
    let mut all_changed = Vec::new();
    let mut all_tool_errors = Vec::new();
    let mut all_denials = Vec::new();
    if staged {
        persist_goal_state(
            &run_context,
            task,
            "active",
            0,
            0,
            0,
            next_resume.as_deref().unwrap_or(&[]),
        );
    }

    let mut outcome = loop {
        let registry = ToolRegistry::with_names(&binding.tools);
        let resume_for_failure = next_resume.clone().unwrap_or_default();
        let run = agent::run_agent_with_context(
            AgentRun {
                cfg,
                provider_name: &binding.provider_name,
                    combo_chain: binding.combo_chain.clone(),
                model: &binding.model,
                task: &agent_task,
                yes,
                max_iterations: effective_max_iter,
                system: system.clone(),
                registry,
                resume: next_resume.take(),
                remote: remote.clone(),
                local_ask: local_ask.clone(),
                context_window: binding.context_window,
            },
            run_context.clone(),
        )
        .await;
        let mut current = match run {
            Ok(outcome) => outcome,
            Err(error) => {
                if staged {
                    let progress = crate::tools_more::todo_progress(
                        &crate::tools_more::current_todos_in(&run_context),
                    );
                    persist_goal_state(
                        &run_context,
                        task,
                        "failed",
                        progress.completed,
                        progress.total,
                        continuations,
                        &resume_for_failure,
                    );
                }
                return Err(error);
            }
        };

        total_input = total_input.saturating_add(current.input_tokens);
        total_output = total_output.saturating_add(current.output_tokens);
        total_iterations = total_iterations.saturating_add(current.iterations);
        for path in &current.changed_files {
            if !all_changed.contains(path) {
                all_changed.push(path.clone());
            }
        }
        all_tool_errors.extend(current.tool_errors.clone());
        all_denials.extend(current.deny_reasons.clone());

        let todos = crate::tools_more::current_todos_in(&run_context);
        let progress = crate::tools_more::todo_progress(&todos);
        let signature = (progress.completed, progress.total);
        if previous_progress == Some(signature) {
            stale_rounds = stale_rounds.saturating_add(1);
        } else {
            stale_rounds = 0;
            previous_progress = Some(signature);
        }
        crate::graph::node_in(
            &run_context,
            "goal",
            &format!("{}/{}", progress.completed, progress.total),
            &format!("continuations={continuations} stale={stale_rounds}"),
            Some("request"),
        );
        // 계획 단계 수와 실제 등록된 todo 수가 어긋나면 첫 판정에서 한 줄만 남긴다.
        // 관측 전용 — 강제하지 않는다 (모델이 단계를 합치거나 쪼갤 여지는 남긴다).
        if staged
            && continuations == 0
            && plan_step_total > 0
            && progress.total > 0
            && progress.total != plan_step_total
        {
            crate::ui::live_warn_in(
                &run_context,
                &format!(
                    "[관측] 계획 단계 {plan_step_total}개 · 등록된 Todo {}개 — 사슬이 어긋났습니다.",
                    progress.total
                ),
            );
        }
        if staged
            && progress.total > 0
            && progress.completed == progress.total
            && current.status == "limit"
        {
            current.status = "ok".into();
            current.error = None;
        }

        let seeded_missing = staged && progress.total == 0 && continuations == 0;
        let continuation_eligible = matches!(current.status.as_str(), "ok" | "limit");
        let should_continue = continuation_eligible
            && (seeded_missing
                || goal_should_continue(
                    progress.completed,
                    progress.total,
                    stale_rounds,
                    continuations,
                    max_continuations,
                ));
        // 폴 fallback 아키텍트 (F6) — 실행 없이 끝나는 거부 후보를 설계 레인으로 1회 되살린다.
        // simple/medium 클래스는 여기 도달하지 않는다 (큰 작업 전용 안전장치).
        if !should_continue
            && continuation_eligible
            && fallback_budget > 0
            && matches!(binding.class, TaskClass::Dev | TaskClass::Advanced)
        {
            let answer = agent::last_assistant_text(&current.messages);
            if crate::fallback::is_refusal_candidate(
                &answer,
                current.changed_files.is_empty(),
                current.iterations <= 1,
                &crate::fallback::refusal_signals(cfg),
            ) {
                match crate::fallback::consult_architect(cfg, task, &answer).await {
                    Ok(Some(judgment)) => {
                        fallback_budget -= 1;
                        crate::ui::live_line_in(
                            &run_context,
                            "[폴 fallback] 실행이 거부되어 아키텍트 상담 후 재개합니다.",
                        );
                        crate::graph::node_in(
                            &run_context,
                            "fallback_architect",
                            "consult",
                            &judgment,
                            Some("goal"),
                        );
                        // 판단을 facts·lessons 에 기록 — 같은 설계 질문의 재거부 예방 (F6).
                        if let Ok(path) = Db::db_path()
                            && let Ok(db) = Db::open(&path)
                        {
                            let qkey: String = judgment
                                .lines()
                                .next()
                                .unwrap_or("")
                                .chars()
                                .take(40)
                                .collect();
                            let _ = db.upsert_fact(
                                Some(&cfg.workspace),
                                "convention",
                                &format!("architect:{qkey}"),
                                &judgment.chars().take(200).collect::<String>(),
                                "agent",
                            );
                            let _ = db.add_lesson(
                                "폴 fallback",
                                "architect",
                                &format!(
                                    "거부 → 아키텍트 상담으로 재개: {}",
                                    task.chars().take(80).collect::<String>()
                                ),
                                200,
                            );
                        }
                        // 막힌 이유가 설계 판단으로 풀렸음을 사용자에게도 알린다 (F6).
                        #[cfg(feature = "telegram")]
                        crate::telegram::notify_owner(
                            cfg,
                            &format!(
                                "[폴 fallback] 아키텍트가 판단했습니다\n{}",
                                judgment.chars().take(300).collect::<String>()
                            ),
                        )
                        .await;
                        let mut messages = current.messages.clone();
                        messages.push(Message::user_text(format!(
                            "[아키텍트 판단]\n{judgment}\n\n위 판단을 바탕으로 원래 작업을 계속 실행하라. \
                             이번에는 반드시 도구로 실행해 결과물(파일 변경)을 만들어라."
                        )));
                        next_resume = Some(messages);
                        continue;
                    }
                    Ok(None) => {
                        // 추출 결과 진짜 질문이 아님 — 정상 종료 경로로 본다.
                    }
                    Err(e) => {
                        crate::ui::live_warn_in(
                            &run_context,
                            &format!("[폴 fallback] 아키텍트 상담 실패(정상 종료로 진행): {e}"),
                        );
                    }
                }
            }
        }
        if !should_continue {
            // todo 를 등록하고도 못 끝낸 경우만 미완료다. todo 자체를 만들지 않고
            // ok 로 끝난 턴은 모델이 단계화가 불필요한 작업으로 판단한 것 —
            // 검증된 산출물이 있으면 완료로 인정한다 (성공을 실패로 오표시 금지).
            if staged
                && progress.total > 0
                && progress.completed < progress.total
                && continuation_eligible
            {
                current.status = "incomplete".into();
                current.error = Some(format!(
                    "목표 미완료: Todo {}/{} · 연속 정체 {}회",
                    progress.completed, progress.total, stale_rounds
                ));
            }
            persist_goal_state(
                &run_context,
                task,
                if current.status == "ok" && progress.completed >= progress.total {
                    "complete"
                } else if current.status == "incomplete" {
                    "blocked"
                } else {
                    "failed"
                },
                progress.completed,
                progress.total,
                continuations,
                &current.messages,
            );
            current.input_tokens = total_input;
            current.output_tokens = total_output;
            current.iterations = total_iterations;
            current.changed_files = all_changed;
            current.tool_errors = all_tool_errors;
            current.deny_reasons = all_denials;
            break current;
        }

        continuations = continuations.saturating_add(1);
        persist_goal_state(
            &run_context,
            task,
            "active",
            progress.completed,
            progress.total,
            continuations,
            &current.messages,
        );
        crate::ui::live_line_in(
            &run_context,
            &format!(
                "[목표 계속] Todo {}/{} · 연속 실행 {continuations}/{max_continuations}",
                progress.completed, progress.total
            ),
        );
        crate::graph::node_in(
            &run_context,
            "goal_continue",
            &format!("cycle {continuations}"),
            &format!("{}/{}", progress.completed, progress.total),
            Some("goal"),
        );
        let mut messages = current.messages;
        let mut nudge = String::from(
            "목표가 아직 완료되지 않았다. 현재 Todo와 도구 결과를 확인하고, \
             완료되지 않은 다음 항목부터 즉시 계속 실행하라. 이미 끝낸 작업은 반복하지 말고, \
             항목을 마칠 때마다 todo_write 상태를 갱신하라. 모든 Todo가 완료된 뒤에만 최종 답변하라.",
        );
        // loop 분야는 정체를 감지한 그 사이클에서 바로 전략 전환을 지시한다
        // (기본 분야는 stale 2회에서 루프를 끊는 기존 동작 그대로).
        if discipline == crate::engine::Discipline::Loop && stale_rounds > 0 {
            nudge.push_str(LOOP_STALE_SWITCH);
        }
        messages.push(Message::user_text(nudge));
        next_resume = Some(messages);
    };

    outcome = finish_verification(
        cfg,
        binding,
        &spec,
        task,
        &dod_checklist,
        &rebuttal,
        yes,
        &system,
        outcome,
        remote.clone(),
        local_ask.clone(),
        run_context.clone(),
    )
    .await?;
    // 도구 없는 프로파일(quick)의 응답에 tool-call 텍스트가 새어 있으면 —
    // 분류가 낮게 잡혔지만 실제로는 도구가 필요한 작업이라는 뜻이다. 모델이
    // 도구 문법을 텍스트로 흉내 낸 오염 응답을 걷어내고 coder 로 1회 승격해
    // 다시 실행한다. (승격된 바인딩은 tools 가 비지 않으므로 재귀는 1회로 끝.)
    if binding.tools.is_empty() && leaked_tool_call(&agent::assistant_text(&outcome.messages)) {
        crate::ui::live_line_in(
            &run_context,
            "도구가 필요한 작업으로 판단 — coder 로 승격해 다시 실행합니다.",
        );
        if let Ok(dev) = bind(cfg, TaskClass::Dev, cli_provider, None)
            && !dev.tools.is_empty()
        {
            let mut clean = outcome.messages.clone();
            while matches!(clean.last(), Some(m) if m.role == crate::provider::Role::Assistant) {
                clean.pop();
            }
            return Box::pin(run_pipeline_inner(
                cfg,
                &dev,
                task,
                yes,
                cli_provider,
                Some(clean),
                remote,
                local_ask,
                run_context,
            ))
            .await;
        }
    }
    // Self-Harness 에피소드 관찰 — TUI/CLI/텔레그램 세 진입 경로가 모두 여기를
    // 지나므로 이 지점 하나로 전 경로의 실행 증거가 수집된다. 백그라운드 실행.
    if self_meta_on {
        crate::self_harness::maybe_observe(cfg, task, &outcome);
    }
    Ok(outcome)
}

/// 종료 검증부 — 검증 실행 + 독립 검증자 게이트. harness 루프와 graph 실행이
/// 같은 지점을 지나도록 한 함수로 모았다 (그래프는 전체 종료 후 1회만 지난다).
#[allow(clippy::too_many_arguments)]
async fn finish_verification(
    cfg: &Config,
    binding: &Binding,
    spec: &crate::engine::EngineSpec,
    task: &str,
    dod: &str,
    rebuttal: &str,
    yes: bool,
    system: &str,
    mut outcome: AgentOutcome,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    // 검증 강도 — Auto/Strict 는 프로파일의 verify 가 꺼져 있어도 자동 감지 명령으로 검증한다.
    let verify_forced = matches!(
        spec.verify_policy,
        crate::engine::VerifyPolicy::Auto | crate::engine::VerifyPolicy::Strict
    );
    if (binding.verify || verify_forced) && outcome.status != "incomplete" {
        crate::graph::node_in(&run_context, "verify", "start", "", Some("request"));
        crate::spinner::set_label_in(&run_context, "검증 중…");
        outcome = run_verify(
            cfg,
            binding,
            task,
            yes,
            system.to_string(),
            outcome,
            remote.clone(),
            local_ask.clone(),
            run_context.clone(),
        )
        .await?;
        crate::graph::node_in(&run_context, "verify", &outcome.status, "", Some("verify"));
    }
    // 독립 검증자 게이트 (§5) — 자기평가 편향을 막기 위해 신선한 컨텍스트의 리뷰어가
    // 완료 기준과 대조한다. 게이트가 가용성을 해치면 안 되므로 게이트 자체의 실패는
    // 경고 한 줄 후 통과로 취급한다.
    if spec.verify_policy == crate::engine::VerifyPolicy::Strict
        && cfg.file.harness.strict_gate
        && matches!(binding.class, TaskClass::Dev | TaskClass::Advanced)
        && outcome.status == "ok"
        && !run_context.is_cancelled()
    {
        let review_worker = review_worker_id(&run_context);
        outcome = run_review_gate(
            cfg,
            binding,
            spec,
            task,
            dod,
            rebuttal,
            yes,
            system,
            outcome,
            remote,
            local_ask,
            run_context.clone(),
        )
        .await;
        // 게이트는 중간에 여러 경로로 빠져나간다 — 워커 줄은 호출부에서 한 번에 닫는다.
        crate::ui::live_worker_in(&run_context, &review_worker, "", "", "", true);
    }
    Ok(outcome)
}

/// 검증자는 실행 모델과 같은 RunContext 로 돌지만 working 패널에서는 별도 줄이다.
fn review_worker_id(run: &RunContext) -> String {
    format!("{}:reviewer", crate::ui::worker_id(run))
}

fn cancelled_outcome() -> AgentOutcome {
    AgentOutcome {
        status: "cancelled".into(),
        error: Some("실행이 취소되었습니다.".into()),
        ..AgentOutcome::default()
    }
}

/// 모델이 도구 호출을 구조체가 아니라 텍스트로 흉내 낸 흔적 —
/// MiniMax 계열의 내부 마커(`]<]`)와 `<tool_call>` JSON 조각을 감지한다.
pub(crate) fn leaked_tool_call(text: &str) -> bool {
    if text.contains("<tool_call>") || text.contains("]<]") {
        return true;
    }
    text.contains("\"name\"") && text.contains("\"arguments\"")
}

fn persist_goal_state(
    run: &RunContext,
    objective: &str,
    status: &str,
    completed: usize,
    total: usize,
    continuations: u8,
    messages: &[Message],
) {
    let Ok(messages_json) = serde_json::to_string(messages) else {
        return;
    };
    if let Ok(path) = Db::db_path()
        && let Ok(db) = Db::open(&path)
    {
        let _ = db.save_goal(&crate::db::GoalRow {
            id: run.run_id().to_string(),
            objective: objective.to_string(),
            status: status.to_string(),
            completed,
            total,
            continuations,
            messages_json,
        });
    }
}

pub(crate) fn goal_should_continue(
    completed: usize,
    total: usize,
    stale_rounds: u8,
    continuations: u8,
    max_continuations: u8,
) -> bool {
    total > 0 && completed < total && stale_rounds < 2 && continuations < max_continuations
}

/// 검증이 재시도 끝에 통과했을 때의 정리 — 최종 실패가 아니므로 verify_fail 을 비우고,
/// 첫 실패는 회복 증거로 옮긴다. 성공을 실패로 오표시하지 않으면서(§15.4)
/// "실패 후 회복" 교훈 수집 경로는 살려 둔다.
pub(crate) fn mark_verify_recovered(outcome: &mut AgentOutcome) {
    if let Some(first) = outcome.verify_fail.take() {
        outcome.verify_recovered = Some(first);
    }
}

/// bash 검증 명령 1회에 대한 승인 결과.
enum BashApproval {
    Allowed,
    Denied,
    /// 미리보기조차 만들 수 없어 실행을 포기한다 (사유 포함).
    Blocked(String),
}

/// 검증용 bash 호출의 승인 흐름 — 종료 검증과 그래프 노드 경계 검증이 함께 쓴다.
/// 승인이 필요 없거나 이미 허용된 실행이면 곧바로 Allowed.
async fn approve_bash_command(
    input: &serde_json::Value,
    tool: &(dyn tools::Tool + Send + Sync),
    ctx: &ToolCtx,
    yes: bool,
    remote: &Option<agent::RemoteApproval>,
    local_ask: &Option<agent::LocalAsk>,
    run_context: &RunContext,
) -> Result<BashApproval> {
    if !tool.needs_approval(input) || yes {
        return Ok(BashApproval::Allowed);
    }
    let preview = match tools::approval_preview("bash", input, ctx) {
        Ok(p) => p,
        Err(e) => return Ok(BashApproval::Blocked(e.to_string())),
    };
    crate::ui::live_line_in(run_context, &preview);
    let denied = if let Some(ask) = local_ask {
        !matches!(
            ask(preview.clone()).await,
            crate::agent::ApprovalChoice::Yes | crate::agent::ApprovalChoice::Always
        )
    } else if let Some(r) = remote {
        let ask = r.ask.clone();
        !tokio::time::timeout(r.timeout, (ask)(preview.clone()))
            .await
            .unwrap_or(false)
    } else {
        print!("[y] 이번만  / [n] 거부  / [a] 이번 실행 모두 허용 : ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let t = line.trim().to_lowercase();
        t == "n" || t == "no"
    };
    Ok(if denied {
        BashApproval::Denied
    } else {
        BashApproval::Allowed
    })
}

#[allow(clippy::too_many_arguments)] // 검증 파이프라인: 각 인자가 독립적 실행 축이라 유지
async fn run_verify(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    yes: bool,
    system: String,
    mut outcome: AgentOutcome,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    let mut cmd = binding.verify_command.clone();
    if cmd.trim().is_empty() {
        if outcome.changed_files.is_empty() {
            // 변경이 없으면 검증할 것도 없다 — 질문·조사 턴이 회귀 게이트를 돌지 않는다.
            crate::ui::live_line_in(&run_context, "검증 생략: 변경된 파일이 없습니다.");
            return Ok(outcome);
        }
        cmd = auto_verify_command(cfg, &outcome.changed_files);
    }
    if cmd.is_empty() {
        crate::ui::live_line_in(&run_context, "검증 생략: 자동 감지할 빌드가 없습니다.");
        return Ok(outcome);
    }

    let bash = ToolRegistry::all();
    let Some(tool) = bash.get("bash") else {
        crate::ui::live_line_in(&run_context, "검증 생략: bash 도구가 없습니다.");
        return Ok(outcome);
    };
    let mut ctx = ToolCtx::new(cfg.workspace.clone());
    ctx.vault = Some(crate::config::expand_tilde(&cfg.file.obsidian.vault_path));
    ctx.db_path = crate::config::expand_tilde(&cfg.file.obsidian.db_path);
    ctx.hashline = cfg.file.edit.hashline;
    ctx.local_ask = local_ask.clone();
    ctx.remote = remote.clone();
    ctx.run = Some(run_context.clone());

    let yes = agent::effective_yes(yes, &remote);
    for round in 0..3 {
        crate::ui::live_line_in(&run_context, &format!("[검증] {cmd}"));
        crate::spinner::set_label_in(&run_context, &format!("검증 실행: {cmd}"));
        let input = serde_json::json!({"command": cmd});
        match approve_bash_command(&input, tool, &ctx, yes, &remote, &local_ask, &run_context)
            .await?
        {
            BashApproval::Allowed => {}
            BashApproval::Denied => {
                crate::ui::live_line_in(&run_context, "검증이 거부되었습니다.");
                outcome.status = "denied".into();
                return Ok(outcome);
            }
            BashApproval::Blocked(e) => {
                crate::ui::live_line_in(
                    &run_context,
                    &format!("검증 명령을 실행할 수 없습니다: {e}"),
                );
                return Ok(outcome);
            }
        }
        match tool.run(serde_json::json!({"command": cmd}), &ctx) {
            Ok(out) if !out.contains("[exit") => {
                // 성공 시 원문 대신 요약 한 줄 — 실패했을 때만 상세가 필요하다.
                let lines = out.trim().lines().count();
                crate::ui::live_line_in(&run_context, &format!("검증 성공 ({lines}줄 출력)"));
                mark_verify_recovered(&mut outcome);
                return Ok(outcome);
            }
            other => {
                let err = match other {
                    Ok(o) => o,
                    Err(e) => e.to_string(),
                };
                if round >= 2 {
                    crate::ui::live_line_in(&run_context, "검증이 2회 재시도 후에도 실패했습니다.");
                    crate::ui::live_line_in(&run_context, &err);
                    outcome.status = "fail".into();
                    outcome.error = Some(err.chars().take(500).collect());
                    outcome.verify_fail = Some(err.chars().take(500).collect());
                    return Ok(outcome);
                }
                crate::ui::live_line_in(
                    &run_context,
                    &format!("검증 실패, 오류를 되먹여 재시도합니다 ({}/2)", round + 1),
                );
                let cause: String = err.chars().take(500).collect();
                let mut msgs = outcome.messages.clone();
                if msgs.is_empty() {
                    msgs.push(Message::user_text(task));
                }
                msgs.push(Message::user_text(format!(
                    "검증 명령이 실패했습니다. 오류를 고치세요.\n{err}"
                )));
                let mut next = agent::run_agent_with_context(
                    AgentRun {
                        cfg,
                        provider_name: &binding.provider_name,
                    combo_chain: binding.combo_chain.clone(),
                        model: binding.verify_model.as_deref().unwrap_or(&binding.model),
                        task,
                        yes,
                        max_iterations: binding.max_iterations,
                        system: system.clone(),
                        registry: ToolRegistry::with_names(&binding.tools),
                        resume: Some(msgs),
                        remote: remote.clone(),
                        local_ask: local_ask.clone(),
                        context_window: binding.context_window,
                    },
                    run_context.clone(),
                )
                .await?;
                if next.verify_fail.is_none() {
                    next.verify_fail = Some(cause);
                }
                outcome = next;
            }
        }
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// 독립 검증자 게이트 (VerifyPolicy::Strict) — 근거: K2 검증자 선택, fresh-context 리뷰어 노드
// ---------------------------------------------------------------------------

/// 게이트 미니 루프 상한 — 리뷰어는 읽고 판정만 한다.
/// 재질의(판정 불능 1회 되물음)까지 같은 루프 안에서 소화하므로 여유를 둔다.
pub(crate) const REVIEW_GATE_MAX_ITER: u32 = 8;
/// 리뷰 피드백 보존 상한 (재개 메시지에 그대로 실린다).
const REVIEW_SUMMARY_CAP: usize = 2000;

/// 판정 불능일 때 리뷰어에게 한 번만 되묻는 문장.
const REVIEW_REQUERY: &str =
    "[판정] pass 또는 fail 한 줄로만 결론을 내라. 이미 읽은 근거로 판정하라.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewVerdict {
    Pass,
    Fail {
        summary: String,
    },
    /// 판정 줄이 없거나 리뷰어 미니 루프가 정상 종료하지 못했다.
    /// "신호 없음 = 통과"가 아니라 별도 상태로 남긴다(설계 §15 공통 원칙).
    Indeterminate,
}

/// 리뷰어 출력에서 판정을 뽑는 순수 함수.
/// `[판정]` 줄이 여러 개면 **마지막** 것이 결론이다. 줄이 아예 없으면 판정 불능.
/// 판정 줄에서 가장 먼저 나온 판정어를 채택한다. 단어 포함 검사만 하면
/// "pass — 실패 요인 없음" 같은 부연의 부정어가 fail 로 오탐된다 (실측 2026-08-26).
fn verdict_token(line: &str) -> Option<bool> {
    // (표지, 실패 여부). '미통과/불합격'이 '통과/합격'을 포함하므로 긴 표지를 먼저 매칭.
    const TOKENS: &[(&str, bool)] = &[
        ("미통과", true),
        ("불합격", true),
        ("fail", true),
        ("실패", true),
        ("pass", false),
        ("통과", false),
        ("합격", false),
        ("충족", false),
    ];
    let mut best: Option<(usize, bool)> = None;
    for (tok, failed) in TOKENS {
        if let Some(pos) = line.find(tok) {
            // 같은 위치에서 긴 표지가 이미 잡혔으면 유지 ('미통과' 안의 '통과' 무시).
            let covered = best.is_some_and(|(p, _)| p <= pos && pos < p + 9);
            if !covered && best.is_none_or(|(p, _)| pos < p) {
                best = Some((pos, *failed));
            }
        }
    }
    best.map(|(_, failed)| failed)
}

pub(crate) fn parse_review_verdict(text: &str) -> ReviewVerdict {
    let mut failed = None;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("[판정]") else {
            continue;
        };
        let v = rest.trim().to_ascii_lowercase();
        if let Some(f) = verdict_token(&v) {
            failed = Some(f);
        }
    }
    match failed {
        None => return ReviewVerdict::Indeterminate,
        Some(false) => return ReviewVerdict::Pass,
        Some(true) => {}
    }
    let mut summary = String::new();
    for header in ["[미충족 항목]", "[결함]"] {
        let section = extract_plan_section(text, header);
        let section = section.trim();
        if section.is_empty() || section == "없음" {
            continue;
        }
        if !summary.is_empty() {
            summary.push('\n');
        }
        summary.push_str(header);
        summary.push(' ');
        summary.push_str(section);
    }
    if summary.is_empty() {
        // 구조를 지키지 않은 출력 — 본문을 그대로 근거로 쓴다.
        summary = text.trim().to_string();
    }
    ReviewVerdict::Fail {
        summary: summary.chars().take(REVIEW_SUMMARY_CAP).collect(),
    }
}

/// 판정 뒤 게이트가 취할 동작 — 재개는 1회만, 2번째 미통과면 기록 후 종료한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GateAction {
    /// 통과 — outcome 그대로 완료.
    Accept,
    /// 판정 불능 — 리뷰어에게 결론만 한 줄 다시 묻는다 (실행당 1회).
    Requery,
    /// 재질의 후에도 판정 불능 — 통과로 진행하되 "판정 불능"으로 남긴다.
    /// 지적을 되먹여 본 작업을 1회 재개한다.
    Resume(String),
    /// 재개 후에도 미통과 — status 는 유지하고 error 에만 사유를 남긴다.
    Report(String),
}

/// 게이트 상태 전이 (순수 함수). `attempt` 는 0-based 검증 회차,
/// `requeried` 는 이번 실행에서 판정 불능 재질의를 이미 썼는지.
pub(crate) fn gate_action(verdict: ReviewVerdict, attempt: u8, requeried: bool) -> GateAction {
    match verdict {
        ReviewVerdict::Pass => GateAction::Accept,
        ReviewVerdict::Indeterminate if !requeried => GateAction::Requery,
        // 재질의 후에도 판정 불능이면 통과로 묻히지 않는다 — 실패로 보고한다 (M2: G11).
        ReviewVerdict::Indeterminate => {
            GateAction::Report("판정 불능 — 검증자가 판정하지 못했다".into())
        }
        ReviewVerdict::Fail { summary } if attempt == 0 => GateAction::Resume(summary),
        ReviewVerdict::Fail { summary } => GateAction::Report(summary),
    }
}

/// 미통과 사유 한 줄 요약 — outcome.error 와 진행 표시에 쓴다.
pub(crate) fn verdict_headline(summary: &str) -> String {
    summary
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("사유 미상")
        .chars()
        .take(120)
        .collect()
}

/// 검증자 입력 — 원 작업 + 완료 기준 + 계획의 반박 + 변경 파일 목록.
/// diff 전문은 넣지 않는다: 리뷰어가 도구로 직접 읽어야 신선한 시각이 유지된다.
pub(crate) fn review_prompt(task: &str, dod: &str, rebuttal: &str, changed: &[String]) -> String {
    let mut s = String::from("아래 작업의 산출물을 완료 기준과 대조해 판정하라.\n\n[원 작업]\n");
    s.extend(task.trim().chars().take(4000));
    let dod = dod.trim();
    if !dod.is_empty() {
        s.push_str("\n\n[완료 기준]\n");
        s.extend(dod.chars().take(4000));
    }
    let rebuttal = rebuttal.trim();
    if !rebuttal.is_empty() {
        // 계획 단계가 스스로 지목한 최대 위험 — 리뷰어가 이 지점을 먼저 확인한다.
        s.push_str("\n\n[계획이 지목한 최대 위험]\n");
        s.extend(rebuttal.chars().take(2000));
    }
    s.push_str("\n\n[변경된 파일]\n");
    if changed.is_empty() {
        s.push_str("(도구가 보고한 변경 파일 없음)\n");
    } else {
        for path in changed.iter().take(40) {
            s.push_str("- ");
            s.push_str(path);
            s.push('\n');
        }
    }
    s.push_str(
        "\n변경 내용은 첨부하지 않았다. read_file·grep 으로 직접 읽어 확인하고, 필요하면 \
         bash 로 빌드·테스트를 직접 실행하라. 확인하지 않은 파일에 대해서는 판정하지 않는다(NEVER).\n\
         완료 기준은 실행 모델이 세운 것이다 — 원 작업 요구와 어긋나면 원 작업이 우선한다.\n\
         \n판정 전 필수 검사:\n\
         1. 테스트 무결성 — diff 에서 #[ignore] 추가·테스트 함수 삭제·어서션 감소가 있으면 무조건 fail.\n\
         2. 하드코딩 탐지 — 구현이 테스트 입력에만 특화돼 있지 않은가? 의심되면 입력을 변형한\n\
            추가 테스트 작성을 재작업 지시에 포함하라.\n\
         3. AC 커버리지 — 완료 기준 항목 하나라도 구현으로 확인되지 않으면 fail 이다.\n\
         4. 자기 보고 배제 — 실행 모델의 완료 주장은 증거가 아니다. 네가 직접 실행한 명령의\n\
            exit code 만이 증거다.",
    );
    s
}

/// 리뷰어 미니 루프 1회. 게이트 자체의 오류는 호출자가 통과로 처리한다.
/// `engine_block` 은 실행 엔진의 prompt_block — 리뷰어도 같은 품질 기준으로 보게 한다
/// (약점 보정 지시 없이 판정만 시키던 교차 누수 제거, 설계 §15.2).
/// `resume` 이 있으면 그 대화를 이어 재질의한다.
#[allow(clippy::too_many_arguments)]
async fn run_review_once(
    cfg: &Config,
    reviewer: &Binding,
    prompt: &str,
    engine_block: &str,
    resume: Option<Vec<Message>>,
    yes: bool,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    // 신선한 컨텍스트: 본 작업의 대화·lessons 를 물려받지 않는다.
    let mut system = system_prompt(cfg, &reviewer.system_extra, "");
    system.push_str(engine_block);
    agent::run_agent_with_context(
        AgentRun {
            cfg,
            provider_name: &reviewer.provider_name,
            combo_chain: reviewer.combo_chain.clone(),
            model: &reviewer.model,
            task: prompt,
            yes,
            max_iterations: reviewer.max_iterations.clamp(1, REVIEW_GATE_MAX_ITER),
            system,
            registry: ToolRegistry::with_names(&reviewer.tools),
            resume,
            remote,
            local_ask,
            context_window: reviewer.context_window,
        },
        run_context,
    )
    .await
}

/// 독립 검증자 게이트 — pass 면 그대로, fail 이면 지적을 되먹여 1회 재개 후 재검증.
/// 2번째 fail 이면 status 는 유지하고 error 에만 사유를 남긴다 (무한루프 방지).
#[allow(clippy::too_many_arguments)]
async fn run_review_gate(
    cfg: &Config,
    binding: &Binding,
    spec: &crate::engine::EngineSpec,
    task: &str,
    dod: &str,
    rebuttal: &str,
    yes: bool,
    system: &str,
    mut outcome: AgentOutcome,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> AgentOutcome {
    // 엔진 고정이 걸린 실행은 manual_verify 를 무시하고 고정 프로바이더의 main 모델로
    // 리뷰한다 (§11.2) — 게이트의 본질은 신선한 컨텍스트이므로 같은 모델이어도 유효하다.
    let pin = engine_pin(cfg).filter(|p| pin_unavailable(cfg, p, true).is_none());
    // [harness] manual_verify 가 지정돼 있으면 그 모델로 검증자를 돌린다.
    let verify_pair = if pin.is_some() {
        None
    } else {
        cfg.file
            .harness
            .manual_verify
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|spec| resolve_spec(cfg, spec, true).ok())
    };
    let reviewer = match bind_profile(
        cfg,
        binding.class,
        Some("reviewer"),
        pin.as_deref()
            .or(verify_pair.as_ref().map(|(p, _)| p.as_str())),
        verify_pair.as_ref().map(|(_, m)| m.as_str()),
    ) {
        Ok(reviewer) => reviewer,
        Err(e) => {
            crate::ui::live_warn_in(
                &run_context,
                &format!("검증자 게이트 생략(바인딩 실패): {e}"),
            );
            return outcome;
        }
    };

    // 판정 불능 재질의는 실행당 1회만 쓴다 (되묻기 무한 반복 금지).
    let mut requeried = false;
    for attempt in 0..2u8 {
        crate::graph::node_in(
            &run_context,
            "critic",
            "start",
            &format!("{} · {}회차", reviewer.model, attempt + 1),
            Some("verify"),
        );
        crate::spinner::set_label_in(&run_context, "독립 검증자 대조 중…");
        crate::ui::live_line_in(
            &run_context,
            &format!(
                "[검증자 리뷰 {}회차 · {}] 완료 기준 대조",
                attempt + 1,
                reviewer.model
            ),
        );
        crate::ui::live_worker_in(
            &run_context,
            &review_worker_id(&run_context),
            "reviewer",
            &format!("{}/{}", reviewer.provider_name, reviewer.model),
            &format!("완료 기준 대조 · {}회차", attempt + 1),
            false,
        );
        let prompt = review_prompt(task, dod, rebuttal, &outcome.changed_files);
        // 안쪽 루프는 "판정 불능 → 결론만 재질의" 1회를 흡수한다.
        let mut resume: Option<Vec<Message>> = None;
        let action = loop {
            let review = match run_review_once(
                cfg,
                &reviewer,
                &prompt,
                &spec.prompt_block,
                resume.take(),
                yes,
                remote.clone(),
                local_ask.clone(),
                run_context.clone(),
            )
            .await
            {
                Ok(review) => review,
                Err(e) => {
                    crate::ui::live_warn_in(
                        &run_context,
                        &format!("검증자 게이트 실패(통과 처리): {e}"),
                    );
                    crate::graph::node_in(
                        &run_context,
                        "critic",
                        "error",
                        &e.to_string(),
                        Some("verify"),
                    );
                    return outcome;
                }
            };
            outcome.input_tokens = outcome.input_tokens.saturating_add(review.input_tokens);
            outcome.output_tokens = outcome.output_tokens.saturating_add(review.output_tokens);

            // 리뷰어가 정상 종료하지 못했으면 그 출력은 결론이 아니다 — 판정 불능으로 본다.
            let verdict = if review.status == "ok" {
                parse_review_verdict(&agent::last_assistant_text(&review.messages))
            } else {
                ReviewVerdict::Indeterminate
            };
            match gate_action(verdict, attempt, requeried) {
                GateAction::Requery => {
                    requeried = true;
                    crate::ui::live_line_in(
                        &run_context,
                        "[검증자] 결론 줄이 없어 판정만 한 줄 다시 요청합니다.",
                    );
                    let mut msgs = review.messages.clone();
                    if msgs.is_empty() {
                        msgs.push(Message::user_text(prompt.as_str()));
                    }
                    msgs.push(Message::user_text(REVIEW_REQUERY));
                    resume = Some(msgs);
                }
                other => break other,
            }
        };
        let summary = match action {
            GateAction::Accept => {
                crate::graph::node_in(&run_context, "critic", "pass", "", Some("verify"));
                crate::ui::live_line_in(&run_context, "[검증자] 완료 기준 충족 — 통과");
                return outcome;
            }
            GateAction::Requery => {
                crate::graph::node_in(
                    &run_context,
                    "critic",
                    "indeterminate",
                    "판정 줄 없음",
                    Some("verify"),
                );
                crate::ui::live_warn_in(&run_context, "[검증자] 판정 불능 — 한 번만 되묻는다");
                return outcome;
            }
            // 미통과·판정 불능 모두 완료가 아니다 — 상태를 fail 로 바꿔 후속 게이트가
            // "성공"으로 오인하지 않게 한다 (M2: G11, AcceptUnknown 폐지).
            GateAction::Report(summary) => {
                let headline = verdict_headline(&summary);
                crate::graph::node_in(&run_context, "critic", "fail", &headline, Some("verify"));
                crate::ui::live_warn_in(
                    &run_context,
                    &format!("[검증자] 미통과 — 검증자 판정을 결과로 남긴다: {headline}"),
                );
                outcome.status = "fail".into();
                outcome.error = Some(format!("검증자 미통과: {headline}"));
                outcome.verify_fail = Some(headline.clone());
                return outcome;
            }
            GateAction::Resume(summary) => summary,
        };
        let headline = verdict_headline(&summary);
        crate::graph::node_in(&run_context, "critic", "fail", &headline, Some("verify"));
        crate::ui::live_line_in(
            &run_context,
            &format!("[검증자] 미통과 — 지적을 되먹여 1회 재개합니다: {headline}"),
        );

        // 재개는 goal 루프를 다시 돌리지 않고 본 작업 바인딩으로 1회만 이어 실행한다.
        let mut messages = outcome.messages.clone();
        if messages.is_empty() {
            messages.push(Message::user_text(task));
        }
        messages.push(Message::user_text(format!(
            "독립 검증자가 완료 기준 대조에서 미통과로 판정했다. 아래 지적을 실제 파일 수정으로 \
             해소하라. 재설명·변명은 금지(NEVER)다. 고친 뒤 스스로 확인하고 무엇을 바꿨는지 보고하라.\n{summary}"
        )));
        let resumed = agent::run_agent_with_context(
            AgentRun {
                cfg,
                provider_name: &binding.provider_name,
                    combo_chain: binding.combo_chain.clone(),
                model: &binding.model,
                task,
                yes,
                max_iterations: binding.max_iterations,
                system: system.to_string(),
                registry: ToolRegistry::with_names(&binding.tools),
                resume: Some(messages),
                remote: remote.clone(),
                local_ask: local_ask.clone(),
                context_window: binding.context_window,
            },
            run_context.clone(),
        )
        .await;
        match resumed {
            Ok(mut next) => {
                next.input_tokens = outcome.input_tokens.saturating_add(next.input_tokens);
                next.output_tokens = outcome.output_tokens.saturating_add(next.output_tokens);
                next.iterations = outcome.iterations.saturating_add(next.iterations);
                let mut changed = outcome.changed_files.clone();
                for path in &next.changed_files {
                    if !changed.contains(path) {
                        changed.push(path.clone());
                    }
                }
                next.changed_files = changed;
                let mut tool_errors = outcome.tool_errors.clone();
                tool_errors.extend(next.tool_errors.clone());
                next.tool_errors = tool_errors;
                let mut denials = outcome.deny_reasons.clone();
                denials.extend(next.deny_reasons.clone());
                next.deny_reasons = denials;
                outcome = next;
                // 재개가 정상 종료하지 못했으면 재검증 없이 그 상태를 그대로 보고한다.
                if outcome.status != "ok" || run_context.is_cancelled() {
                    crate::ui::live_warn_in(
                        &run_context,
                        &format!(
                            "[검증자] 재개가 완료되지 않아 재검증을 생략합니다 (status={})",
                            outcome.status
                        ),
                    );
                    return outcome;
                }
            }
            Err(e) => {
                crate::ui::live_warn_in(
                    &run_context,
                    &format!("검증자 재개 실패(직전 상태 유지): {e}"),
                );
                outcome.error = Some(format!("검증자 미통과: {headline}"));
                return outcome;
            }
        }
    }
    outcome
}

// ---------------------------------------------------------------------------
// 그래프 분야 (discipline = graph) — PEV 상태 그래프를 위상순으로 순차 실행한다.
// 노드마다 신선한 컨텍스트로 돌고, 선행 노드에서는 결론(산출물 요약)만 넘겨받는다
// (서브에이전트 격리 원칙). 병렬 실행은 이번 범위 밖이다.
// ---------------------------------------------------------------------------

/// 선행 노드 산출물 요약 상한 — 다음 노드로 결론만 넘긴다.
pub(crate) const GRAPH_SUMMARY_CAP: usize = 500;
/// 노드 하나의 최소 반복 상한.
const GRAPH_NODE_MIN_ITER: u32 = 8;

/// 계획이 산출한 DAG 노드 하나.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct DagNode {
    pub(crate) id: String,
    pub(crate) goal: String,
    #[serde(default)]
    pub(crate) deps: Vec<String>,
    #[serde(default)]
    pub(crate) produces: String,
}

#[derive(Debug, serde::Deserialize)]
struct DagPlan {
    nodes: Vec<DagNode>,
}

/// deps 에 순환이 있어 위상 정렬이 불가능함 — 그래프 폴백 사유.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CycleError {
    /// 정렬되지 못하고 남은 노드 id 들.
    pub(crate) remaining: Vec<String>,
}

impl std::fmt::Display for CycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "노드 순환: {}", self.remaining.join(" → "))
    }
}

/// 계획 텍스트에서 DAG JSON 앞의 산문만 — [완료 기준] 추출이 JSON 을 빨아들이지 않게.
/// 코드 펜스와 첫 `{` 중 먼저 오는 지점에서 자른다.
pub(crate) fn plan_prose(plan: &str) -> &str {
    let cut = [plan.find("```"), plan.find('{')]
        .into_iter()
        .flatten()
        .min();
    match cut {
        Some(i) => &plan[..i],
        None => plan,
    }
}

/// 첫 `{` 부터 짝이 맞는 지점까지. 문자열 리터럴 안의 중괄호·이스케이프는 건너뛴다.
fn balanced_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 응답에서 JSON 객체를 꺼낸다 — ```json 펜스 우선, 없으면 첫 `{` 부터 균형 매칭.
fn extract_json_object(text: &str) -> Option<&str> {
    if let Some(at) = text.find("```json")
        && let Some(rest) = text.get(at + "```json".len()..)
        && let Some(end) = rest.find("```")
        && let Some(obj) = balanced_object(&rest[..end])
    {
        return Some(obj);
    }
    balanced_object(text)
}

/// 계획 응답 → DAG 노드 목록. 형식이 조금이라도 어긋나면 None 을 돌려 harness 로 폴백한다.
pub(crate) fn parse_dag(text: &str) -> Option<Vec<DagNode>> {
    let json = extract_json_object(text)?;
    let plan: DagPlan = serde_json::from_str(json).ok()?;
    if plan.nodes.is_empty() {
        return None;
    }
    let mut seen: Vec<&str> = Vec::with_capacity(plan.nodes.len());
    for node in &plan.nodes {
        let id = node.id.trim();
        // 빈 id·빈 goal·중복 id 는 실행 순서를 정의할 수 없다 — 형식 오류로 본다.
        if id.is_empty() || node.goal.trim().is_empty() || seen.contains(&id) {
            return None;
        }
        seen.push(id);
    }
    Some(plan.nodes)
}

/// Kahn 위상 정렬 → 실행 순서(노드 인덱스). 모르는 id 를 가리키는 deps 는 간선이 없다.
pub(crate) fn topo_order(nodes: &[DagNode]) -> Result<Vec<usize>, CycleError> {
    let index: std::collections::HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.trim(), i))
        .collect();
    let mut indegree = vec![0usize; nodes.len()];
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        let mut linked: Vec<usize> = Vec::new();
        for dep in &node.deps {
            let Some(&j) = index.get(dep.trim()) else {
                continue;
            };
            if linked.contains(&j) {
                continue; // 같은 deps 가 두 번 적혀도 진입차수를 두 번 세지 않는다.
            }
            linked.push(j);
            edges[j].push(i);
            indegree[i] += 1;
        }
    }
    let mut queue: std::collections::VecDeque<usize> =
        (0..nodes.len()).filter(|i| indegree[*i] == 0).collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(i) = queue.pop_front() {
        order.push(i);
        for &next in &edges[i] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                queue.push_back(next);
            }
        }
    }
    if order.len() != nodes.len() {
        return Err(CycleError {
            remaining: nodes
                .iter()
                .enumerate()
                .filter(|(i, _)| !order.contains(i))
                .map(|(_, n)| n.id.trim().to_string())
                .collect(),
        });
    }
    Ok(order)
}

/// 노드 성공 판정 — 등록한 todo 를 모두 끝내고 반복 상한에 닿은 경우는 구제한다
/// (전역 goal 루프와 같은 규칙).
pub(crate) fn graph_node_ok(status: &str, completed: usize, total: usize) -> bool {
    status == "ok" || (status == "limit" && total > 0 && completed == total)
}

/// 마지막 assistant 텍스트 — 노드 산출물 요약의 원문.
fn last_assistant_text(messages: &[Message]) -> String {
    for m in messages.iter().rev() {
        if m.role != crate::provider::Role::Assistant {
            continue;
        }
        let text = m
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.trim()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

/// 다음 노드로 넘길 결론 — 마지막 답변 앞 500자 + 이 노드가 바꾼 파일 목록.
pub(crate) fn graph_node_summary(outcome: &AgentOutcome) -> String {
    let mut s: String = last_assistant_text(&outcome.messages)
        .chars()
        .take(GRAPH_SUMMARY_CAP)
        .collect();
    if !outcome.changed_files.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str("변경 파일: ");
        s.push_str(&outcome.changed_files.join(", "));
    }
    if s.is_empty() {
        s.push_str("(보고된 산출물 없음)");
    }
    s
}

/// 노드 실행용 시스템 프롬프트 — 메인 조립 결과 + 이 노드의 좌표 + 선행 산출물.
pub(crate) fn graph_node_system(
    system: &str,
    step: usize,
    total: usize,
    node: &DagNode,
    produced: &[(String, String)],
) -> String {
    let mut s = String::with_capacity(system.len() + 512);
    s.push_str(system);
    s.push_str(&format!(
        "\n\n[그래프 노드 {step}/{total}] 목표: {}\n",
        node.goal.trim()
    ));
    let produces = node.produces.trim();
    if !produces.is_empty() {
        s.push_str(&format!("이 노드의 산출물: {produces}\n"));
    }
    for dep in &node.deps {
        let Some((_, summary)) = produced.iter().find(|(id, _)| id == dep.trim()) else {
            continue;
        };
        s.push_str(&format!("\n[선행 산출물 {}]\n{summary}\n", dep.trim()));
    }
    s.push_str(
        "\n이 노드의 목표만 수행한다. 다른 노드가 맡은 범위는 건드리지 않는다(NEVER). \
         선행 노드가 바꾼 파일은 읽고 그 위에서 작업한다 — 이미 있는 구현을 지우고 \
         새로 쓰지 않는다(NEVER). 끝내면 무엇을 만들었는지 한 문단으로 보고한다.",
    );
    s
}

/// 노드 실행용 사용자 프롬프트 — 원 작업 + 이 노드의 목표.
pub(crate) fn graph_node_prompt(task: &str, node: &DagNode) -> String {
    format!(
        "{task}\n\n[이번 노드] {}\n이 노드의 목표만 수행하라. 나머지는 다른 노드가 처리한다.",
        node.goal.trim()
    )
}

/// 노드 재시도 프롬프트 (순수 함수) — 실패 사유 + 첫 시도가 이미 바꾼 파일.
/// 노드 재시도는 resume 없이 신선한 컨텍스트로 도는데, 첫 시도의 편집을 모르면
/// 같은 파일을 처음부터 다시 써서 이중 편집이 난다 (설계 §15.5).
pub(crate) fn graph_retry_prompt(prompt: &str, reason: &str, changed: &[String]) -> String {
    let mut s = format!(
        "{prompt}\n\n[직전 시도 실패] {reason}\n같은 접근을 반복하지 마라. \
         막힌 지점을 먼저 확인하고 다른 경로로 이 노드의 목표를 완수하라."
    );
    if !changed.is_empty() {
        s.push_str("\n[첫 시도가 이미 바꾼 파일]\n");
        for path in changed.iter().take(40) {
            s.push_str("- ");
            s.push_str(path);
            s.push('\n');
        }
        s.push_str("현재 상태를 읽고 이어가라. 처음부터 다시 쓰지 마라(NEVER).");
    }
    s
}

/// 노드 경계 검증 — 노드가 끝날 때마다 자동 감지된 검증 명령을 한 번 돌린다.
/// 실패 출력을 돌려주면 호출자가 그 노드의 재시도 사유로 합류시킨다.
/// 명령이 없거나 승인되지 않으면 None (검증 생략은 실패가 아니다).
#[allow(clippy::too_many_arguments)]
async fn graph_node_boundary_check(
    cfg: &Config,
    changed: &[String],
    yes: bool,
    remote: &Option<agent::RemoteApproval>,
    local_ask: &Option<agent::LocalAsk>,
    run_context: &RunContext,
) -> Option<String> {
    let cmd = auto_verify_command(cfg, changed);
    if cmd.is_empty() {
        return None;
    }
    let registry = ToolRegistry::all();
    let tool = registry.get("bash")?;
    let mut ctx = ToolCtx::new(cfg.workspace.clone());
    ctx.vault = Some(crate::config::expand_tilde(&cfg.file.obsidian.vault_path));
    ctx.db_path = crate::config::expand_tilde(&cfg.file.obsidian.db_path);
    ctx.hashline = cfg.file.edit.hashline;
    ctx.local_ask = local_ask.clone();
    ctx.remote = remote.clone();
    ctx.run = Some(run_context.clone());
    let input = serde_json::json!({ "command": cmd });
    // 노드마다 승인을 되묻지 않는다 — 이미 허용된 실행에서만 돌린다.
    let allowed = agent::effective_yes(yes, remote) || run_context.run_tree_approved();
    if !matches!(
        approve_bash_command(&input, tool, &ctx, allowed, remote, local_ask, run_context).await,
        Ok(BashApproval::Allowed)
    ) {
        return None;
    }
    crate::spinner::set_label_in(run_context, &format!("노드 경계 검증: {cmd}"));
    match tool.run(input, &ctx) {
        Ok(out) if !out.contains("[exit") => {
            crate::ui::live_line_in(run_context, &format!("[노드 검증] {cmd} — 통과"));
            None
        }
        other => {
            let err = match other {
                Ok(o) => o,
                Err(e) => e.to_string(),
            };
            crate::ui::live_warn_in(
                run_context,
                &format!("[노드 검증] {cmd} 실패 — 이 노드에서 바로잡습니다."),
            );
            Some(format!(
                "노드 경계 검증 실패 ({cmd}):\n{}",
                err.chars().take(1500).collect::<String>()
            ))
        }
    }
}

/// 노드 하나의 실행 결과를 그래프 합산본에 누적한다 (실패한 시도도 보존).
fn merge_node_outcome(agg: &mut AgentOutcome, node: &AgentOutcome) {
    agg.input_tokens = agg.input_tokens.saturating_add(node.input_tokens);
    agg.output_tokens = agg.output_tokens.saturating_add(node.output_tokens);
    agg.iterations = agg.iterations.saturating_add(node.iterations);
    agg.context_tokens = node.context_tokens;
    agg.cached_tokens = agg.cached_tokens.saturating_add(node.cached_tokens);
    agg.cache_reported = agg.cache_reported || node.cache_reported;
    for path in &node.changed_files {
        if !agg.changed_files.contains(path) {
            agg.changed_files.push(path.clone());
        }
    }
    agg.tool_errors.extend(node.tool_errors.clone());
    agg.deny_reasons.extend(node.deny_reasons.clone());
    if node.verify_fail.is_some() {
        agg.verify_fail = node.verify_fail.clone();
    }
    if node.verify_recovered.is_some() {
        agg.verify_recovered = node.verify_recovered.clone();
    }
    agg.messages = node.messages.clone();
}

/// 위상순 노드 실행. 노드는 신선한 messages 로 돌고, 실패하면 사유를 덧붙여 1회만
/// 재시도한다. 재실패면 그래프를 중단하고 그때까지의 산출물을 합산해 돌려준다.
#[allow(clippy::too_many_arguments)]
async fn run_graph_discipline(
    cfg: &Config,
    binding: &Binding,
    task: &str,
    nodes: &[DagNode],
    order: &[usize],
    system: &str,
    yes: bool,
    remote: Option<agent::RemoteApproval>,
    local_ask: Option<agent::LocalAsk>,
    run_context: RunContext,
) -> Result<AgentOutcome> {
    let total = order.len();
    let node_iter = (binding.max_iterations / 2).max(GRAPH_NODE_MIN_ITER);
    let mut produced: Vec<(String, String)> = Vec::with_capacity(total);
    let mut agg = AgentOutcome::default();
    for (step, &i) in order.iter().enumerate() {
        if run_context.is_cancelled() {
            return Ok(cancelled_outcome());
        }
        let node = &nodes[i];
        let id = node.id.trim();
        let goal_head: String = node.goal.trim().chars().take(120).collect();
        crate::graph::node_in(
            &run_context,
            "graph_node",
            id,
            &goal_head,
            Some("plan_first"),
        );
        crate::ui::live_line_in(
            &run_context,
            &format!("[그래프] 노드 {}/{} {id} — {goal_head}", step + 1, total),
        );
        crate::spinner::set_label_in(
            &run_context,
            &format!("그래프 노드 {}/{}: {id}", step + 1, total),
        );
        // 노드마다 todo 를 비운다 — 앞 노드가 남긴 항목이 이 노드의 완료 판정을 흐리지 않게.
        crate::tools_more::clear_todos_in(&run_context);
        let node_system = graph_node_system(system, step + 1, total, node, &produced);
        let mut prompt = graph_node_prompt(task, node);
        for attempt in 0..2u8 {
            let outcome = agent::run_agent_with_context(
                AgentRun {
                    cfg,
                    provider_name: &binding.provider_name,
                    combo_chain: binding.combo_chain.clone(),
                    model: &binding.model,
                    task: &prompt,
                    yes,
                    max_iterations: node_iter,
                    system: node_system.clone(),
                    registry: ToolRegistry::with_names(&binding.tools),
                    // 신선한 컨텍스트: 앞 노드의 대화를 물려받지 않는다.
                    resume: None,
                    remote: remote.clone(),
                    local_ask: local_ask.clone(),
                    context_window: binding.context_window,
                },
                run_context.clone(),
            )
            .await?;
            let progress = crate::tools_more::todo_progress(&crate::tools_more::current_todos_in(
                &run_context,
            ));
            let mut ok = graph_node_ok(&outcome.status, progress.completed, progress.total);
            let mut reason = outcome
                .error
                .clone()
                .unwrap_or_else(|| format!("status={}", outcome.status));
            merge_node_outcome(&mut agg, &outcome);
            // 취소는 실패가 아니다 — 재시도하지 않고 그대로 끝낸다.
            if outcome.status == "cancelled" || run_context.is_cancelled() {
                return Ok(cancelled_outcome());
            }
            // 노드 경계 검증 — 깨진 상태가 다음 노드로 전파되지 않게 한다(§15.5).
            // 재시도 여지가 있는 첫 시도에서만 돌린다(검증은 무겁고, 마지막 시도 뒤에는
            // 어차피 그래프 종료 후 종료 검증이 같은 명령을 다시 돌린다).
            if ok
                && attempt == 0
                && let Some(fail) = graph_node_boundary_check(
                    cfg,
                    &agg.changed_files,
                    yes,
                    &remote,
                    &local_ask,
                    &run_context,
                )
                .await
            {
                ok = false;
                reason = fail;
            }
            if ok {
                produced.push((id.to_string(), graph_node_summary(&outcome)));
                crate::graph::node_in(&run_context, "graph_node", id, "ok", Some("plan_first"));
                break;
            }
            if attempt == 0 {
                crate::ui::live_warn_in(
                    &run_context,
                    &format!(
                        "[그래프] 노드 {id} 실패 — 1회 재시도합니다: {}",
                        reason.lines().next().unwrap_or(&reason)
                    ),
                );
                prompt = graph_retry_prompt(&prompt, &reason, &outcome.changed_files);
                continue;
            }
            crate::graph::node_in(&run_context, "graph_node", id, "fail", Some("plan_first"));
            crate::ui::live_warn_in(
                &run_context,
                &format!("[그래프] 노드 {id} 재실패 — 그래프를 중단합니다: {reason}"),
            );
            agg.status = outcome.status.clone();
            agg.error = Some(format!(
                "그래프 중단: 노드 {id} 실패 ({}/{total} 노드 완료)",
                produced.len()
            ));
            return Ok(agg);
        }
    }
    agg.status = "ok".into();
    agg.error = None;
    crate::ui::live_line_in(
        &run_context,
        &format!("[그래프] {total}개 노드 완료 — 검증으로 넘어갑니다"),
    );
    Ok(agg)
}

pub(crate) fn auto_verify_command(cfg: &Config, changed: &[String]) -> String {
    if cfg.workspace.join("Cargo.toml").exists() {
        // 테스트 디렉터가 있으면 컴파일 게이트를 넘어 회귀 게이트로 — G5 해소.
        if cfg.workspace.join("tests").is_dir() {
            return "cargo test --quiet".into();
        }
        return "cargo check".into();
    }
    let py_changed: Vec<&str> = changed
        .iter()
        .filter(|p| p.ends_with(".py"))
        .map(|s| s.as_str())
        .collect();
    if cfg.workspace.join("pyproject.toml").exists() || !py_changed.is_empty() {
        if py_changed.is_empty() {
            return String::new();
        }
        let files = py_changed.join(" ");
        #[cfg(windows)]
        {
            return format!("python -m py_compile {files}");
        }
        #[cfg(not(windows))]
        {
            return format!("python3 -m py_compile {files}");
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_policy_keeps_work_in_english_and_briefing_in_korean() {
        let dir = std::env::temp_dir().join(format!("rafikx-lang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = crate::config::Config::load(Some(&dir.join("config.toml"))).unwrap();
        let s = system_prompt(&cfg, "", "");
        assert!(s.contains("[언어 정책]"));
        assert!(s.contains("작업 진행 설명은 영어로 쓴다"));
        assert!(s.contains("제목(헤더·섹션 제목)은 한국어로 쓴다"));
        assert!(s.contains("최종 답변(작업 결과 브리핑)은 한국어로 쓴다"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
