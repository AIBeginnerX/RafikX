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

pub fn classify_rules(text: &str, obsidian: bool) -> TaskClass {
    if looks_like_dev(text) {
        return TaskClass::Dev;
    }
    if looks_like_advanced(text) {
        return TaskClass::Advanced;
    }
    // obsidian 플래그는 컨텍스트 주입 여부일 뿐 — 인사말까지 medium 으로 올리지 않는다.
    let _ = obsidian;
    let n = text.chars().count();
    if (150..=600).contains(&n) {
        return TaskClass::Medium;
    }
    if contains_any(
        text,
        &[
            "요약",
            "정리",
            "번역",
            "초안",
            "검색",
            "찾아",
            "노트",
            "문서",
            "파일",
            "마크다운",
            "폴더",
            "디렉토리",
            "워크스페이스",
        ],
    ) {
        return TaskClass::Medium;
    }
    TaskClass::Simple
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
    if cfg.file.general.classifier == "llm"
        && let Ok(c) = classify_llm(cfg, text).await
    {
        return Ok(c);
    }
    Ok(classify_rules(text, obsidian))
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
