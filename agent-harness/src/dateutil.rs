//! 표준 라이브러리만으로 현재 UTC 날짜를 계산하는 유틸.
//! chrono 의존성 없이 검색 결과 신선도 표기에 쓴다.
//! 날짜 역산은 Howard Hinnant 의 civil_from_days 알고리즘.

use std::time::{SystemTime, UNIX_EPOCH};

/// 현재 UTC 날짜를 "YYYY-MM-DD" 문자열로 반환한다.
pub fn today_string() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// epoch 이후 일수 -> (연, 월, 일). Hinnant, "chrono-Compatible Low-Level Date Algorithms".
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
    }

    #[test]
    fn today_shape() {
        let t = today_string();
        assert_eq!(t.len(), 10);
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[7..8], "-");
    }
}
