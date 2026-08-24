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
};

pub const THEMES: [&Theme; 3] = [&RAFIKX, &OPAL, &SYNTH];

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
        assert_eq!(names().len(), 3);
    }
}
