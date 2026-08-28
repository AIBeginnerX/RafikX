use super::*;

/// 현재 팀 모드 — `[harness] team`. 미설정·오타는 single 로 떨어진다.
pub fn team_mode(cfg: &Config) -> crate::engine::TeamMode {
    crate::engine::normalize_team(&cfg.file.harness.team)
}

/// team = multi — 계획이 확정된 뒤 독립 단계를 역할 서브에이전트에게 넘기게 하는 지침.
/// graph 분야와는 상호 배타다 (그래프가 이미 분해 실행을 담당한다).
pub(crate) const TEAM_MULTI_BLOCK: &str = "\n\n[팀 모드]\n\
     여러 파일·여러 분야(UI/로직/스타일/검증)에 걸친 작업에서는 독립적인 갈래를 \
     직접 작성하지 말고 task 도구로 위임하는 것을 기본으로 한다 — 직접 작성은 \
     갈래가 하나뿐이거나 파일 2개 이하의 소규모일 때만.\n\
     계획이 확정되면 [작업 분해]에서 서로 독립적인 단계들을 task 도구로 위임하라 — \
     role 은 planner|frontend|backend|reviewer 중 작업 성격에 맞는 것.\n\
     서로 독립적인 갈래는 한 응답에서 task 를 여러 개 함께 호출하면 병렬로 실행된다.\n\
     위임 프롬프트에는 해당 단계의 목표·대상 파일·완료 기준을 자체 완결로 담아라 \
     (수신자는 이 대화를 보지 못한다).\n\
     순차 의존이 있는 단계는 스스로 수행하거나 순서대로 위임하라.";

/// 팀 지침 주입 조건 (순수 함수). multi 이고, 도구를 쓰는 설계·개발 작업이며,
/// graph 분야가 아닐 때만 켠다 — graph 는 이미 노드 DAG 로 분해 실행을 담당한다.
pub fn team_block_active(
    team: crate::engine::TeamMode,
    class: TaskClass,
    has_tools: bool,
    graph_mode: bool,
) -> bool {
    team == crate::engine::TeamMode::Multi
        && matches!(class, TaskClass::Dev | TaskClass::Advanced)
        && has_tools
        && !graph_mode
}
