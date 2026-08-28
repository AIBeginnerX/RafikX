//! 교차-표면 UI 정책 상수 — TUI·데스크탑·텔레그램이 공유하는 표시 정책의 단일 원천.
//!
//! 배경: 붙여넣기 접힘 임계값이 tui.rs와 desktop/ui/js/paste-blocks.js에 각각
//! 하드코딩되어 있다가 2026-08-27(29c8288)에 사후 통일된 이력이 있다. 표멸별
//! 정책 중복을 구조적으로 막기 위해 값은 여기서만 정의하고, 데스크탑 JS는
//! `ui_policy` Tauri 명령으로 부팅 시 가져간다. JS 폴 fallback 기본값은
//! `js_fallback_matches_rust` 테스트가 Rust 상수와의 일치를 단언한다.

use serde::Serialize;

/// 붙여넣기 접힘: 이 글자 수 이상이면 본문 대신 칩(요약)으로 접는다.
/// 왜 이 값인가: 코드·로그 대량 붙여넣기가 스크롤백을 밀어내는 0.7.x 사고 방지
/// 기준으로 도입된 값을 TUI·데스크탑이 공유하기로 합의한 수치.
pub const PASTE_COLLAPSE_CHARS: usize = 1200;

/// 붙여넣기 접힘: 이 줄 수를 초과할 때도 접는다 (글자 수 조건과 OR).
/// 왜 이 값인가: 25줄이면 한 화면을 넘어 대화 흐름을 끊는 최소 단위.
pub const PASTE_COLLAPSE_LINES: usize = 25;

/// 접힌 칩의 미리보기 최대 글자 수 (데스크탑 칩 라벨).
/// 왜 이 값인가: 칩 한 줄에 내용 식별이 가능한 최소 길이.
pub const PASTE_PREVIEW_MAX: usize = 300;

/// 글자/줄 수가 접힘 기준에 걸리는지 — 모든 표면이 이 함수 하나로 판정한다.
pub const fn paste_needs_collapse(chars: usize, lines: usize) -> bool {
    chars >= PASTE_COLLAPSE_CHARS || lines > PASTE_COLLAPSE_LINES
}

/// 데스크탑 JS에 전달하는 정책 스냅샷 (Tauri `ui_policy` 명령의 반환형).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UiPolicy {
    pub paste_collapse_chars: usize,
    pub paste_collapse_lines: usize,
    pub paste_preview_max: usize,
}

pub fn current() -> UiPolicy {
    UiPolicy {
        paste_collapse_chars: PASTE_COLLAPSE_CHARS,
        paste_collapse_lines: PASTE_COLLAPSE_LINES,
        paste_preview_max: PASTE_PREVIEW_MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_boundary() {
        assert!(!paste_needs_collapse(1199, 25));
        assert!(paste_needs_collapse(1200, 1));
        assert!(paste_needs_collapse(10, 26));
        assert!(!paste_needs_collapse(10, 25));
    }

    /// 데스크탑 JS의 폴 fallback 기본값이 Rust 원천과 어긋나면 실패한다.
    /// 어긋났다는 것은 한쪽만 값을 바꿨다는 뜻이다.
    #[test]
    fn js_fallback_matches_rust() {
        let js = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../desktop/ui/js/paste-blocks.js"),
        )
        .expect("desktop paste-blocks.js를 읽어야 한다");
        for (name, value) in [
            ("CHAR_THRESHOLD", PASTE_COLLAPSE_CHARS),
            ("LINE_THRESHOLD", PASTE_COLLAPSE_LINES),
            ("PREVIEW_MAX", PASTE_PREVIEW_MAX),
        ] {
            let needle = format!("{name}: {value}");
            assert!(
                js.contains(&needle),
                "paste-blocks.js 폴 fallback이 Rust 원천과 다르다: {needle} 없음"
            );
        }
    }
}
