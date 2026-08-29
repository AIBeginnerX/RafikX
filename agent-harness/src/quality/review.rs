//! 리뷰 위원회 (S7) — 5개 독립 리뷰 패스로 "시니어 1000명"의 본질(독립적 시선의 수)을 재현.
//! 근거: docs/agent-upgrade/07_QUALITY.md §2 S7.
//! 각 리뷰어는 서로 다른 체크리스트를 갖고, fresh-context 에서 변경 파일을 직접 읽고,
//! 구조화 판정(pass|fail + 지적·수정 지시)을 낸다. 전원 통과해야 위원회 통과다.

use serde::Serialize;

use crate::harness::parse_review_verdict;
use crate::harness::ReviewVerdict;

pub struct Reviewer {
    pub id: &'static str,
    pub focus: &'static str,
    pub checklist: &'static [&'static str],
}

/// 5 리뷰어 — 지시서 §2 S7 정의 그대로.
pub const COMMITTEE: &[Reviewer] = &[
    Reviewer {
        id: "accuracy",
        focus: "정확성",
        checklist: &[
            "로직이 요구사항과 정확히 일치하는가",
            "경계값(빈 입력·최대값·유니코드)이 처리됐는가",
            "오류 경로가 누락되지 않았는가 (unwrap 난발·예외 삼킴)",
        ],
    },
    Reviewer {
        id: "security",
        focus: "보안",
        checklist: &[
            "외부 입력이 검증·정규화 없이 사용되지 않는가",
            "SQL/명령/경로 조립이 파라미터화·배열화·정규화돼 있는가",
            "시크릿 하드코딩·에러 메시지 내부 정보 노출이 없는가",
        ],
    },
    Reviewer {
        id: "performance",
        focus: "성능",
        checklist: &[
            "핵심 경로의 복잡도가 데이터 규모에 맞는가",
            "불필요한 복제·순회·할당이 없는가",
            "동기 작업이 비동기 경로를 막지 않는가",
        ],
    },
    Reviewer {
        id: "readability",
        focus: "가독성·간결성",
        checklist: &[
            "함수가 하나의 일을 하며 길이가 상한 내인가",
            "3중 이상 중복·복붙 블록이 없는가 — 삭제 가능한 코드는 결함이다",
            "이름이 동작을 설명하는가, 비관용 패턴이 없는가",
        ],
    },
    Reviewer {
        id: "api-design",
        focus: "API·설계",
        checklist: &[
            "공개 인터페이스가 최소 표면적인가 (YAGNI)",
            "호출자 입장에서 오용 어렵고 의도가 명확한가",
            "기존 관례와 일관되고 추상화 층위가 섞이지 않았는가",
        ],
    },
];

/// 리뷰어 1인의 심사 프롬프트 — fresh-context, 자기 체크리스트만.
/// 변경 파일 본문은 첨부하지 않고 읽기 전용 도구로 직접 확인하게 한다.
pub fn reviewer_prompt(
    reviewer: &Reviewer,
    task: &str,
    changed: &[String],
    criterion: &str,
) -> String {
    let mut s = format!(
        "너는 코드 리뷰 위원회의 '{}'({}) 리뷰어다. 아래 변경을 네 체크리스트로만 심사하라.\
         \n\n[원 작업]\n{task}\n\n[변경된 파일]\n",
        reviewer.focus, reviewer.id
    );
    if changed.is_empty() {
        s.push_str("(변경 파일 없음)\n");
    } else {
        for path in changed.iter().take(40) {
            s.push_str(&format!("- {path}\n"));
        }
    }
    s.push_str("\n[너의 체크리스트]\n");
    for (i, item) in reviewer.checklist.iter().enumerate() {
        s.push_str(&format!("{}. {item}\n", i + 1));
    }
    if !criterion.trim().is_empty() {
        s.push_str(&format!("\n[완료 기준]\n{}\n", criterion.trim()));
    }
    s.push_str(
        "\n변경 내용은 첨부하지 않는다 — read_file·grep 으로 직접 읽어 확인하라.\n\
         네 체크리스트 밖의 사안은 다른 리뷰어의 몫이다. 판정은 마지막 줄에 \
         '[판정] pass' 또는 '[판정] fail — 지적' 한 줄로 내린다. \
         fail 이면 각 지적에 파일:줄과 수정 지시를 포함하라.",
    );
    s
}

/// 위원회 집계 판정.
#[derive(Debug, Clone, Serialize)]
pub struct CommitteeVerdict {
    pub results: Vec<CommitteeResult>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitteeResult {
    pub reviewer: &'static str,
    pub focus: &'static str,
    pub verdict: Verdict,
    /// fail 사유 — ReviewVerdict::Fail 요약.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    Indeterminate,
}

/// 리뷰어 응답 텍스트 → 판정. 기존 검증자 파서(parse_review_verdict)를 재사용해
/// 판정 문법이 전 시스템에서 하나로 유지된다.
pub fn verdict_of(reviewer: &Reviewer, response: &str) -> CommitteeResult {
    let (verdict, summary) = match parse_review_verdict(response) {
        ReviewVerdict::Pass => (Verdict::Pass, None),
        ReviewVerdict::Fail { summary } => (Verdict::Fail, Some(summary)),
        ReviewVerdict::Indeterminate => (Verdict::Indeterminate, None),
    };
    CommitteeResult {
        reviewer: reviewer.id,
        focus: reviewer.focus,
        verdict,
        summary,
    }
}

/// 위원회 전체 집합 — 전원 Pass 여야 통과. Indeterminate 는 통과가 아니다.
pub fn aggregate(results: &[CommitteeResult]) -> CommitteeVerdict {
    let passed = results.iter().all(|r| r.verdict == Verdict::Pass) && !results.is_empty();
    CommitteeVerdict {
        results: results.to_vec(),
        passed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committee_has_five_distinct_reviewers() {
        assert_eq!(COMMITTEE.len(), 5);
        let ids: Vec<&str> = COMMITTEE.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec!["accuracy", "security", "performance", "readability", "api-design"]
        );
        for r in COMMITTEE {
            assert!(r.checklist.len() >= 3, "{} 체크리스트 최소 3항", r.id);
        }
    }

    #[test]
    fn prompt_contains_checklist_and_verdict_format() {
        let r = &COMMITTEE[0];
        let p = reviewer_prompt(r, "작업", &["src/a.rs".to_string()], "AC-1 통과");
        assert!(p.contains("정확성"));
        assert!(p.contains("경계값"));
        assert!(p.contains("- src/a.rs"));
        assert!(p.contains("[판정] pass"));
        assert!(p.contains("read_file"));
    }

    #[test]
    fn verdict_and_aggregate_require_unanimous_pass() {
        let p = &COMMITTEE[0];
        assert_eq!(verdict_of(p, "좋다\n[판정] pass").verdict, Verdict::Pass);
        let f = verdict_of(p, "[판정] fail — 3줄 중복 발견");
        assert_eq!(f.verdict, Verdict::Fail);
        assert!(f.summary.as_deref().unwrap().contains("중복"));
        let i = verdict_of(p, "판정 줄이 없는 장문 리뷰");
        assert_eq!(i.verdict, Verdict::Indeterminate);

        let results = vec![
            verdict_of(&COMMITTEE[0], "[판정] pass"),
            verdict_of(&COMMITTEE[1], "[판정] pass"),
            verdict_of(&COMMITTEE[2], "[판정] fail — 루프 내 할당"),
            verdict_of(&COMMITTEE[3], "[판정] pass"),
            verdict_of(&COMMITTEE[4], "[판정] pass"),
        ];
        let agg = aggregate(&results);
        assert!(!agg.passed, "한 명이라도 fail 면 위원회 미통과");
    }
}
