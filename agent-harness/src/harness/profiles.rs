use super::*;

pub fn profile_name_for(cfg: &Config, class: TaskClass) -> &str {
    match class {
        TaskClass::Simple => cfg.file.harness.simple.as_str(),
        TaskClass::Medium => cfg.file.harness.medium.as_str(),
        TaskClass::Advanced => cfg.file.harness.advanced.as_str(),
        TaskClass::Dev => cfg.file.harness.dev.as_str(),
    }
}

/// 프로파일 사양 조회 — config `[subagents.<name>]` 이 내장 전문가 프리셋을 이긴다.
/// (기존 사용자 config 에 planner/reviewer 가 없어도 동작하게 하는 폴백.)
pub fn resolve_profile(cfg: &Config, name: &str) -> Option<crate::config::SubAgentConfig> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    cfg.file
        .subagents
        .get(n)
        .cloned()
        .or_else(|| crate::config::builtin_profile(n))
}

/// config `[subagents]` 또는 내장 프리셋에 있는 프로파일 이름인지.
pub fn profile_exists(cfg: &Config, name: &str) -> bool {
    resolve_profile(cfg, name).is_some()
}

/// 레인 프로파일의 도구 허용목록 — mutation 도구를 바인딩 단계에서 강제로 걸러낸다
/// (프롬프트 경고에 의존하지 않는다, F5). config 로 같은 이름을 재정의해도 필터는 유효하다.
pub fn lane_tool_allowlist(profile: &str) -> Option<&'static [&'static str]> {
    match profile.trim().to_ascii_lowercase().as_str() {
        "explorer" => Some(&[
            "read_file",
            "list_dir",
            "grep",
            "glob",
            "lsp_diagnostics",
            "lsp_definition",
            "todo_read",
        ]),
        "researcher" => Some(&["web_search", "webfetch", "read_file"]),
        "reviewer" => Some(&["read_file", "list_dir", "grep", "glob"]),
        _ => None,
    }
}
