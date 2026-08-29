use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskClass {
    Simple,
    Medium,
    Advanced,
    Dev,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessStrategy {
    Single,
    Multi,
}

impl HarnessStrategy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "single" => Some(Self::Single),
            "multi" => Some(Self::Multi),
            _ => None,
        }
    }
}

impl TaskClass {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskClass::Simple => "simple",
            TaskClass::Medium => "medium",
            TaskClass::Advanced => "advanced",
            TaskClass::Dev => "dev",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "simple" => Some(TaskClass::Simple),
            "medium" => Some(TaskClass::Medium),
            "advanced" => Some(TaskClass::Advanced),
            "dev" => Some(TaskClass::Dev),
            _ => None,
        }
    }
}

/// medium 키워드 — 규칙 분류와 확신도 판정이 같은 목록을 쓴다(단일 원천).
const MEDIUM_KEYWORDS: &[&str] = &[
    "요약", "정리", "번역", "초안", "검색", "찾아", "노트", "문서", "파일", "마크다운",
    "폴터", "디렉토리", "워크스페이스",
    // 기억 의도 — remember/recall 도구가 필요하므로 도구 없는 quick 으로내면 안 된다 (T4 실측).
    "기억해", "기억나", "기록해", "remember", "recall",
];

/// 경로·파일 신호 — dev 키워드는 안 맞지만 도구 필요 가능성이 있는 짧은 입력.
const TOOL_HINTS: &[&str] = &["~/", "./", "src/", "/tmp", ".txt", ".log", ".csv"];

const ENGLISH_DEV_ACTIONS: &[&str] = &[
    "build", "code", "create", "develop", "edit", "fix", "generate", "implement", "make",
    "modify", "program", "repair", "update", "write",
];

const ENGLISH_ARTIFACTS: &[&str] = &[
    "api", "app", "application", "browser game", "cli", "code", "component", "file", "game",
    "script", "tool", "web page", "website",
];

pub fn classify_rules(text: &str, obsidian: bool) -> TaskClass {
    classify_rules_with_confidence(text, obsidian).0
}

/// 규칙 분류 + 확신도. 확신=false 면 호출부가 소형 모델 재판정을 검토한다 (F2 IntentGate).
///
/// 확신 판정:
/// - dev/advanced 키워드 매칭 → 확신 (강한 신호)
/// - 길이 medium 구간(150~600자)의 경계 ±50자 → 불확신
/// - medium 키워드 매칭 → 불확신 (약한 단일 신호일 수 있음)
/// - 경로 신호만 있는 short 입력 → 불확신 (도구 필요 가능성)
pub fn classify_rules_with_confidence(text: &str, obsidian: bool) -> (TaskClass, bool) {
    if looks_like_dev(text) {
        return (TaskClass::Dev, true);
    }
    if looks_like_advanced(text) {
        return (TaskClass::Advanced, true);
    }
    // obsidian 플래그는 컨텍스트 주입 여부일 뿐 — 인사말까지 medium 으로 올리지 않는다.
    let _ = obsidian;
    let n = text.chars().count();
    if (150..=600).contains(&n) {
        return (TaskClass::Medium, (200..=550).contains(&n));
    }
    let operative = strip_quoted(text);
    if contains_any(&operative, MEDIUM_KEYWORDS) {
        return (TaskClass::Medium, false);
    }
    if contains_any(&operative, TOOL_HINTS) {
        return (TaskClass::Simple, false);
    }
    (TaskClass::Simple, true)
}

/// 분류 판정 출처 — 재판정 뒤집힘 분석·run graph 기록에 쓴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassSource {
    Forced,
    Rules,
    Judge,
}

#[derive(Debug, Clone, Copy)]
pub struct ClassDecision {
    /// 최종 분류.
    pub class: TaskClass,
    /// 규칙 단독 판정이면 그 값, 재판정이면 규칙이 냈던 원래 값 (비교용).
    pub rules_class: TaskClass,
    pub confident: bool,
    pub via: ClassSource,
}

/// IntentGate 분류 — 규칙 우선, 경계값만 소형 모델 재판정.
///
/// judge 모델은 classify_llm 이 쓰는 "small" 역할 모델 (config [providers.*] model_role).
/// 3초 상한·실패·목록 외 응답은 규칙 결과로 폴 fallback — 규칙이 영구 안전망.
pub async fn classify_gated(
    cfg: &Config,
    text: &str,
    obsidian: bool,
    forced: Option<&str>,
) -> Result<ClassDecision> {
    if let Some(s) = forced {
        let class = TaskClass::parse(s)
            .ok_or_else(|| anyhow!("--class 값은 simple|medium|advanced|dev 여야 합니다"))?;
        return Ok(ClassDecision {
            class,
            rules_class: class,
            confident: true,
            via: ClassSource::Forced,
        });
    }
    let (rules_class, confident) = classify_rules_with_confidence(text, obsidian);
    if cfg.file.general.classifier == "llm"
        && let Ok(c) = classify_llm(cfg, text).await
    {
        return Ok(ClassDecision {
            class: apply_tool_floor(rules_class, c),
            rules_class,
            confident: true,
            via: ClassSource::Judge,
        });
    }
    if confident {
        return Ok(ClassDecision {
            class: rules_class,
            rules_class,
            confident,
            via: ClassSource::Rules,
        });
    }
    let judged = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        classify_llm(cfg, text),
    )
    .await;
    match judged {
        Ok(Ok(c)) => Ok(ClassDecision {
            class: c,
            rules_class,
            confident: true,
            via: ClassSource::Judge,
        }),
        _ => {
            // 재판정이 못 열린 채 불확실 분류로 진행한다 — 조용한 무동작을 막기 위해
            // 사용자에게 보인다 (T2 실측: judge 실패 시 simple 조용 종료).
            crate::ui::live_warn(&format!(
                "분류 재판정을 쓸 수 없어 규칙 결과({})로 진행합니다. 작업이 안 움직이면 /class dev 로 고정하세요.",
                rules_class.as_str()
            ));
            Ok(ClassDecision {
                class: rules_class,
                rules_class,
                confident: false,
                via: ClassSource::Rules,
            })
        }
    }
}


/// 인용 구간("…"·'…'·「…」·`…`) 제거 — 인용 속 동사는 지시가 아니라 인용이다 (G5).
/// "이 요청을 검토해줘: '…만들어줘'" 같은 입력이 인용 속 생성 동사 때문에
/// dev 로 오분류되는 것을 막는다. 인용 밖의 파일명·동사는 그대로 남는다.
pub(crate) fn strip_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if matches!(c, '"' | '\'' | '「' | '『' | '`') {
                    quote = Some(match c { '「' => '」', '『' => '』', other => other });
                } else {
                    out.push(c);
                }
            }
        }
    }
    out
}

fn looks_like_dev(text: &str) -> bool {
    if text.contains("```") {
        return true;
    }
    let text = strip_quoted(text);
    let lower = text.to_ascii_lowercase();
    let exts = [
        ".rs", ".py", ".js", ".ts", ".tsx", ".jsx", ".html", ".css", ".toml", ".json", ".go",
        ".java", ".c", ".cpp", ".h", ".cs", ".rb", ".php", ".kt", ".swift", ".sh", ".ps1",
        ".md", ".yml", ".yaml",
    ];
    if exts.iter().any(|e| lower.contains(e)) {
        return true;
    }
    if contains_english_term(&lower, "compile")
        || contains_english_term(&lower, "debug")
        || contains_english_term(&lower, "refactor")
        || ENGLISH_DEV_ACTIONS
            .iter()
            .any(|term| contains_english_term(&lower, term))
            && ENGLISH_ARTIFACTS
                .iter()
                .any(|term| contains_english_term(&lower, term))
    {
        return true;
    }
    contains_any(
        &text,
        &[
            "코드",
            "구현",
            "수정해",
            "고쳐",
            "버그",
            "디버그",
            "디버깅",
            "검증",
            "컴파일",
            "빌드",
            "리팩터",
            "테스트 작성",
            "스크립트",
            "함수",
            "에러 잡아",
            "업그레이드",
            "바꿔",
            "바꾸",
            "교체",
            "변경해",
            "만들어",
            "생성해",
            "작성해",
            "적용해",
        ],
    )
}

fn contains_english_term(text: &str, term: &str) -> bool {
    let words: Vec<&str> = text
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let term_words: Vec<&str> = term.split_whitespace().collect();
    words
        .windows(term_words.len())
        .any(|window| window == term_words.as_slice())
}

pub(crate) fn continuation_class(
    text: &str,
    class: TaskClass,
    history: &[Message],
) -> TaskClass {
    if class != TaskClass::Simple {
        return class;
    }
    let lower = text.to_ascii_lowercase();
    let continues = contains_any(
        &lower,
        &[
            "다시 해",
            "다시 만들",
            "계속",
            "마저",
            "이어서",
            "재시도",
            "continue",
            "finish it",
            "fix it",
            "keep going",
            "make it work",
            "resume",
            "retry",
            "try again",
        ],
    );
    let current_turn_start = history
        .iter()
        .rposition(|message| {
            message.role == crate::provider::Role::User
                && message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Text { .. }))
        })
        .unwrap_or(0);
    let had_dev_tools = history[current_turn_start..]
        .iter()
        .flat_map(|message| &message.content)
        .any(|block| {
            matches!(
                block,
                ContentBlock::ToolUse { name, .. }
                    if matches!(
                        name.as_str(),
                        "apply_patch"
                            | "bash"
                            | "edit_file"
                            | "multi_edit"
                            | "task"
                            | "todo_write"
                            | "write_file"
                    )
            )
        });
    if continues && had_dev_tools {
        TaskClass::Dev
    } else {
        class
    }
}

pub(crate) fn apply_tool_floor(rules_class: TaskClass, judged_class: TaskClass) -> TaskClass {
    if rules_class == TaskClass::Dev {
        TaskClass::Dev
    } else {
        judged_class
    }
}

fn looks_like_advanced(text: &str) -> bool {
    let text = &strip_quoted(text);
    if text.chars().count() > 600 {
        return true;
    }
    if list_item_count(text) >= 3 {
        return true;
    }
    contains_any(
        text,
        &[
            "설계",
            "아키텍처",
            "분석",
            "전략",
            "비교 평가",
            "보고서",
            "계획 수립",
            "검토",
            "구성",
        ],
    )
}

fn list_item_count(text: &str) -> usize {
    text.lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("- ")
                || t.starts_with("* ")
                || t.starts_with("• ")
                || t.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() && t.contains('.'))
        })
        .count()
}

fn contains_any(text: &str, kws: &[&str]) -> bool {
    kws.iter().any(|k| text.contains(k))
}

pub async fn classify(
    cfg: &Config,
    text: &str,
    obsidian: bool,
    forced: Option<&str>,
) -> Result<TaskClass> {
    if let Some(s) = forced {
        return TaskClass::parse(s)
            .ok_or_else(|| anyhow!("--class 값은 simple|medium|advanced|dev 여야 합니다"));
    }
    Ok(classify_gated(cfg, text, obsidian, forced).await?.class)
}

async fn classify_llm(cfg: &Config, text: &str) -> Result<TaskClass> {
    let default = cfg.file.general.default_provider.clone();
    let order = fallback_order(cfg, &default, None);
    let req = ChatRequest {
        model: String::new(),
        system: "다음 지시를 simple/medium/advanced/dev 중 한 단어로만 분류하라.".into(),
        messages: vec![Message::user_text(text)],
        tools: vec![],
        max_tokens: 8,
        stream: false,
    };
    let (_name, resp) = chat_with_fallback(cfg, &order, "small", req).await?;
    let word = resp
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("")
        .trim();
    TaskClass::parse(word).ok_or_else(|| anyhow!("llm 분류 모호: {word}"))
}

#[cfg(test)]
mod intent_gate_tests {
    use super::*;

    #[test]
    fn strong_signals_are_confident() {
        assert_eq!(classify_rules_with_confidence("안녕", false), (TaskClass::Simple, true));
        let (c, conf) = classify_rules_with_confidence("buggy.py 만들어서 고쳐줘", false);
        assert_eq!((c, conf), (TaskClass::Dev, true));
        let (c, conf) = classify_rules_with_confidence("이 저장소 구조 분석해서 개선 전략 보고서 써줘", false);
        assert_eq!((c, conf), (TaskClass::Advanced, true));
    }

    #[test]
    fn length_boundary_is_uncertain() {
        // 160자 — medium 구간(150~600) 안이지만 경계 ±50 이내
        let text = "가".repeat(160);
        let (c, conf) = classify_rules_with_confidence(&text, false);
        assert_eq!((c, conf), (TaskClass::Medium, false));
        // 400자 — 구간 중앙, 확신
        let text = "가".repeat(400);
        let (c, conf) = classify_rules_with_confidence(&text, false);
        assert_eq!((c, conf), (TaskClass::Medium, true));
        // 590자 — 상단 경계
        let text = "가".repeat(590);
        let (c, conf) = classify_rules_with_confidence(&text, false);
        assert_eq!((c, conf), (TaskClass::Medium, false));
    }

    #[test]
    fn weak_keyword_match_is_uncertain() {
        // 짧은데 medium 키워드만 매칭 — 약한 신호
        let (c, conf) = classify_rules_with_confidence("파일 정리해줘", false);
        assert_eq!((c, conf), (TaskClass::Medium, false));
    }

    #[test]
    fn path_hint_short_input_is_uncertain() {
        let (c, conf) = classify_rules_with_confidence("~/notes.txt 읽어줘", false);
        assert_eq!((c, conf), (TaskClass::Simple, false));
    }

    #[test]
    fn rules_wrapper_behavior_is_unchanged() {
        // with_confidence 도입 후에도 규칙 결과 자체는 기존과 동일해야 한다
        assert_eq!(classify_rules("안녕", false), TaskClass::Simple);
        assert_eq!(classify_rules("AGENTS.md 업그레이드해줘", false), TaskClass::Dev);
        assert_eq!(classify_rules("내 노트 찾아줘", true), TaskClass::Medium);
    }

    #[test]
    fn contract_instruction_carries_readchk_clause() {
        let inst = super::super::PLAN_CONTRACT_INSTRUCTION;
        assert!(inst.contains("[질문]"));
        assert!(inst.contains("확인 연극 금지"));
    }
}

/// 일의 성격에 따른 레인 제안 (F5) — TaskClass 와 무관하게 읽기 전용 레인을 우선 배정한다.
/// dev 신호(파일 생성·수정 요청)가 있으면 제안하지 않는다: 구현 요청을 탐색 레인으로
/// 본으면 쓰기 도구가 없어 작업이 실패한다.
pub fn suggest_lane(text: &str) -> Option<&'static str> {
    // 쓰기 행위 요청이면 레인 제안 금지 — "함수/코드" 같은 명사가 들어간 읽기 전용
    // 질문("이 함수를 호출하는 곳 찾아줘")까지 차단하지 않도록 행위 동사만 본다.
    if contains_any(
        text,
        &[
            "수정해", "고쳐", "만들어", "생성해", "작성해", "적용해", "리팩터",
            "구현해", "바꿔", "추가해", "삭제해", "커밋", "옮겨",
        ],
    ) {
        return None;
    }
    // 코드베이스 탐색 패턴 — "어디/찾아/호출/의존/구조" + 코드 맥락.
    if contains_any(
        text,
        &[
            "호출하는", "호출하는 곳", "어디서", "의존하는", "의존성", "참조하는",
            "정의로", "레퍼런스", "어느 파일", "코드 구조", "구조를 보여", "구조 알려",
        ],
    ) {
        return Some("explorer");
    }
    // 외부 정보 리서치 패턴 — 최신 정보·공식 문서·라이브러리 조사.
    if contains_any(
        text,
        &[
            "최신", "공식 문서", "라이브러리", "비교해줘", "알아봐", "버전 뭐",
            "출시", " changelog", "문서 찾아", "스펙 확인",
        ],
    ) {
        return Some("researcher");
    }
    None
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    #[test]
    fn code_exploration_routes_to_explorer() {
        assert_eq!(suggest_lane("이 함수를 호출하는 곳을 다 찾아줘"), Some("explorer"));
        assert_eq!(suggest_lane("Db::open 이 어디서 의존하는지 궁금해"), Some("explorer"));
        assert_eq!(suggest_lane("이 구조체 정의로 이동해줘"), Some("explorer"));
        assert_eq!(suggest_lane("레퍼런스 검색해줘"), Some("explorer"));
    }

    #[test]
    fn external_research_routes_to_researcher() {
        assert_eq!(suggest_lane("최신 ratatui 버전이 뭐야?"), Some("researcher"));
        assert_eq!(suggest_lane("tokio 공식 문서에서 select! 사용법 찾아줘"), Some("researcher"));
        assert_eq!(suggest_lane("이 라이브러리 두 개 비교해줘"), Some("researcher"));
        assert_eq!(suggest_lane("rust 1.98 출시했는지 알아봐줘"), Some("researcher"));
    }

    #[test]
    fn dev_requests_never_route_to_lanes() {
        // "찾아" 류 표현이 있어도 dev 신호(수정·생성)가 있으면 레인 제안 금지
        assert_eq!(suggest_lane("호출하는 곳 찾아서 코드 수정해줘"), None);
        assert_eq!(suggest_lane("최신 방식으로 리팩터해줘"), None);
    }

    #[test]
    fn plain_questions_have_no_lane() {
        assert_eq!(suggest_lane("안녕"), None);
        assert_eq!(suggest_lane("이 파일 요약해줘"), None);
    }
}

#[test]
fn t3_actual_input_routes_explorer() {
    assert_eq!(crate::harness::suggest_lane("src 에서 helper 함수를 호출하는 곳을 다 찾아줘"), Some("explorer"));
}

#[cfg(test)]
mod quote_strip_tests {
    use super::*;

    #[test]
    fn strips_double_single_backtick_korean_quotes() {
        assert_eq!(strip_quoted("이 파일을 \"지금\" 고쳐줘"), "이 파일을  고쳐줘");
        assert_eq!(strip_quoted("'이 부분'을 수정해줘"), "을 수정해줘");
        assert_eq!(strip_quoted("「이 함수」를 만들어줘"), "를 만들어줘");
        assert_eq!(strip_quoted("`x` 라는 변수"), " 라는 변수");
    }

    #[test]
    fn quoted_action_verbs_do_not_trigger_dev() {
        // G5 — 인용 속 생성 동사는 지시가 아니다
        let (c, _) = classify_rules_with_confidence(
            "이 요청을 검토해줘: '내일까지 뭐든 빨리 만들어줘'", false);
        assert_ne!(c, TaskClass::Dev);
    }

    #[test]
    fn real_edit_requests_still_dev() {
        // 인용 밖 동사는 그대로 유효
        assert_eq!(classify_rules("notes.txt 의 \"beta\" 를 BETA2 로 바꿔줘", false), TaskClass::Dev);
        assert_eq!(classify_rules("'fix.py' 만들어줘", false), TaskClass::Dev);
        assert_eq!(classify_rules("이 파일을 고쳐줘", false), TaskClass::Dev);
    }

    #[test]
    fn quoted_sentence_as_content_is_not_dev() {
        assert_ne!(classify_rules("'이 함수를 고쳐' 라고 쓰여 있는 문장을 번역해줘", false), TaskClass::Dev);
    }
}
