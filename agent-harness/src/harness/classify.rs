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
];

/// 경로·파일 신호 — dev 키워드는 안 맞지만 도구 필요 가능성이 있는 짧은 입력.
const TOOL_HINTS: &[&str] = &["~/", "./", "src/", "/tmp", ".txt", ".log", ".csv"];

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
    if contains_any(text, MEDIUM_KEYWORDS) {
        return (TaskClass::Medium, false);
    }
    if contains_any(text, TOOL_HINTS) {
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
    if cfg.file.general.classifier == "llm"
        && let Ok(c) = classify_llm(cfg, text).await
    {
        return Ok(ClassDecision {
            class: c,
            rules_class: c,
            confident: true,
            via: ClassSource::Judge,
        });
    }
    let (rules_class, confident) = classify_rules_with_confidence(text, obsidian);
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
        _ => Ok(ClassDecision {
            class: rules_class,
            rules_class,
            confident: false,
            via: ClassSource::Rules,
        }),
    }
}


fn looks_like_dev(text: &str) -> bool {
    if text.contains("```") {
        return true;
    }
    let exts = [
        ".rs", ".py", ".js", ".ts", ".tsx", ".jsx", ".toml", ".json", ".go", ".java", ".c", ".cpp",
        ".h", ".cs", ".rb", ".php", ".kt", ".swift", ".sh", ".ps1", ".md", ".yml", ".yaml",
    ];
    if exts.iter().any(|e| text.contains(e)) {
        return true;
    }
    contains_any(
        text,
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
            "만들어",
            "생성해",
            "작성해",
            "적용해",
        ],
    )
}

fn looks_like_advanced(text: &str) -> bool {
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
