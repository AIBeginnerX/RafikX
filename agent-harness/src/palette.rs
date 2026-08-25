//! TUI·데스크탑 공용 팔레트. config `[ui] theme` 에서 고른다.
//! ratatui 없이도 쓸 수 있게 RGB 튜플로 정의한다.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub bg: (u8, u8, u8),
    pub accent: (u8, u8, u8),
    pub secondary: (u8, u8, u8),
    pub code: (u8, u8, u8),
    pub text: (u8, u8, u8),
    pub body: (u8, u8, u8),
    pub mute: (u8, u8, u8),
    pub warn: (u8, u8, u8),
    pub success: (u8, u8, u8),
    /// 오류 전용 붉은 계열.
    pub err: (u8, u8, u8),
    /// 오류 메시지 안의 중요 단어 강조용 다크 엘로우.
    pub kw: (u8, u8, u8),
    /// 코드블록·표 패널 배경 — 본문과 시각적으로 분리한다.
    pub panel: (u8, u8, u8),
    /// 고정 프레임과 구분선.
    pub border: (u8, u8, u8),
    /// 모델 사고·중간 작업 전용 저대비 색.
    pub thinking: (u8, u8, u8),
}

pub const RAFIKX: Theme = Theme {
    name: "rafikx",
    bg: (3, 6, 14),
    accent: (232, 213, 163),
    secondary: (107, 92, 255),
    code: (94, 231, 255),
    text: (255, 255, 255),
    body: (170, 170, 170),
    mute: (92, 97, 120),
    warn: (250, 200, 60),
    success: (92, 214, 146),
    err: (255, 92, 92),
    kw: (216, 168, 32),
    panel: (12, 18, 34),
    border: (42, 49, 72),
    thinking: (118, 126, 154),
};

/// opencode 느낌의 따뜻한 앰버 테마.
pub const OPAL: Theme = Theme {
    name: "opal",
    bg: (16, 16, 18),
    accent: (255, 179, 102),
    secondary: (126, 231, 135),
    code: (120, 220, 232),
    text: (240, 240, 235),
    body: (200, 198, 190),
    mute: (110, 110, 118),
    warn: (255, 123, 114),
    success: (126, 231, 135),
    err: (244, 90, 90),
    kw: (226, 178, 40),
    panel: (26, 26, 31),
    border: (66, 66, 75),
    thinking: (132, 132, 144),
};

/// 청록 네온 계열.
pub const SYNTH: Theme = Theme {
    name: "synth",
    bg: (10, 10, 26),
    accent: (255, 121, 198),
    secondary: (97, 214, 255),
    code: (183, 219, 255),
    text: (235, 235, 255),
    body: (180, 180, 210),
    mute: (100, 100, 140),
    warn: (255, 85, 85),
    success: (98, 225, 188),
    err: (255, 70, 104),
    kw: (230, 184, 46),
    panel: (18, 18, 44),
    border: (52, 52, 92),
    thinking: (128, 128, 172),
};

/// Claude Code 스타일 — Anthropic 공식 브랜드 팔레트 기반.
/// Dark #141413 · Light #faf9f5 · Mid Gray #b0aea5 · Light Gray #e8e6dc,
/// 액센트 Orange #d97757 · Blue #6a9bcc · Green #788c5d.
/// (success/code 는 다크 배경 가독성을 위해 브랜드 톤을 유지한 채 밝기만 보정.)
pub const CLAUDE: Theme = Theme {
    name: "claude",
    bg: (20, 20, 19),
    accent: (217, 119, 87),
    secondary: (106, 155, 204),
    code: (137, 180, 216),
    text: (250, 249, 245),
    body: (232, 230, 220),
    mute: (176, 174, 165),
    warn: (224, 175, 104),
    success: (137, 162, 106),
    err: (224, 85, 85),
    kw: (203, 158, 76),
    panel: (30, 30, 28),
    border: (64, 62, 56),
    thinking: (122, 120, 112),
};

pub const THEMES: [&Theme; 4] = [&RAFIKX, &OPAL, &SYNTH, &CLAUDE];

pub fn by_name(name: &str) -> &'static Theme {
    let n = name.trim().to_ascii_lowercase();
    THEMES.into_iter().find(|t| t.name == n).unwrap_or(&RAFIKX)
}

pub fn names() -> Vec<&'static str> {
    THEMES.iter().map(|t| t.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_default() {
        assert_eq!(by_name("없는것").name, "rafikx");
        assert_eq!(by_name("opal").name, "opal");
        assert_eq!(by_name("claude").name, "claude");
        assert_eq!(names().len(), 4);
    }

    #[test]
    fn claude_theme_uses_anthropic_brand_colors() {
        let t = by_name("claude");
        assert_eq!(t.bg, (20, 20, 19)); // #141413
        assert_eq!(t.accent, (217, 119, 87)); // #d97757
        assert_eq!(t.secondary, (106, 155, 204)); // #6a9bcc
        assert_eq!(t.text, (250, 249, 245)); // #faf9f5
        assert_eq!(t.mute, (176, 174, 165)); // #b0aea5
    }
}
