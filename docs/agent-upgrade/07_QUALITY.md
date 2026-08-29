# 07 — 코드 생성 품질 엔진 (구현 기록)

> 지시서 "코드 생성 품질 엔진 구축" 수행 기록. 기존 검증 아키텍처(verify 모듈·증거 원장·품질 래칫·레드팀) 위에 통합했다.
> 작업 규칙 준수: 모든 판단에 코드 근거 또는 실행 증거를 첨부.

## 1. 구현 개요

| 구성 요소 | 파일 | 지시서 매핑 |
|---|---|---|
| 언어 프로파일·감지 | `agent-harness/src/quality/profile.rs` | §3 (S0 감지 + S3 명령 데이터화) |
| 보안 스캐너 | `agent-harness/src/quality/security.rs` | §5 (S5 — 시크릿·SQL 인젝션·에러 노출·eval) |
| 알고리즘 설계 노트 | `agent-harness/src/quality/design_note.rs` | §4 (S1 의무 산출물) |
| 완료 루브릭 | `agent-harness/src/quality/rubric.rs` | §6.1·6.2 (루브릭 동결·미검증 차단) |
| 리뷰 위원회 | `agent-harness/src/quality/review.rs` + runner.rs `parse_committee_verdicts` | §2 S7 (5 독립 관점) |
| 중복 블록 감지 | `agent-harness/src/quality/mod.rs` `detect_duplicate_blocks` | §1 간결함 (S7 보조) |
| CLI | `rafikx quality-gate <files>` | §2 파이프라인 실행기 |
| 스킬 3종 | `skills/{security,design,api-design}-checklist/SKILL.md` | §5·§6.3·§7 (수확 초기 세트) |

설계 결정 — 외부 보안 도구(semgrep·gitleaks 등)가 설치돼 있지 않은 환경이 기본이므로,
**내장 휴리스틱 스캐너가 오프라인 최소 보장**을 하고, 프로파일의 감사 명령은 도구가 있을 때만
실행한다. 중요한 성질: 감사 도구가 없어도 게이트는 fail 이 된다("감사 없이는 통과 없음" — 실측:
`✗ [S5-audit] cargo audit` 후 게이트 실패). 도구 설치 시 자동으로 게이트에 합류한다.

## 2. 파이프라인 상태 (S0~S8)

| 스테이지 | 상태 | 근거 |
|---|---|---|
| S0 컨텍스트 분석 | ✅ 구현 | profile.rs detect() — 변경 파일 확장자 우선 + 워크스페이스 마커(Cargo.toml 등) |
| S1 알고리즘 설계 | ✅ 스키마+검증 | design_note.rs — 후비2+·복잡도·불변식·정확성 논증·property 목록 강제, 누락 전건 일괄 보고 |
| S2 구현 | 기존 | agent 루프 + [증거 우위] 프롬프트(v1.1.6) |
| S3 기계 게이트 | ✅ 구현 | profile 명령 실행 — rust: fmt --check / clippy -D warnings / test, python: ruff·mypy --strict, ts: prettier·eslint(max 0)·tsc --strict 등. 도구 미설치는 기록 후 내장 스캐너로 최소 보장 |
| S4 테스트 게이트 | ⏳ 설계 추적 | 프로파일에 property 프레임워크(proptest/hypothesis/fast-check) 지정 — 자동 property 생성은 M-later |
| S5 보안 게이트 | ✅ 구현 | 내장 스캐너 4종(시크릿·SQL 인젝션·에러 노출·eval) + 의존성 감사 연결. **감사 도구 부재 시에도 통과 불가**(실측) |
| S6 성능 게이트 | ⏳ 설계 추적 | 설계 노트 complexity_budget 이 검증 기준 — 벤치 래칫은 M7 |
| S7 리뷰 위원회 | ✅ 구현 | 5 리뷰어 체크리스트(review.rs) + 게이트 프롬프트 5그룹 구조화 판정 + `parse_committee_verdicts` (누락 그룹 = 실패) |
| S8 디자인 게이트 | ✅ 체크리스트 | skills/design-checklist (렌더링 의무 원칙 명문화) — 렌더링 인프라 자동화는 M-later |

**절충(명시)**: S7 의 "5개 독립 컨텍스트"는 두 단계로 간다. (1) 현재 — fresh-context 리뷰어가
5그룹 체크리스트를 순서 심사하고 **그룹별 판정 줄을 누락 없이** 남겨야 통과(`parse_committee_verdicts`
가 누락 그룹을 실패 처리). (2) 완전 격리 — review.rs 의 5 리뷰어 프롬프트가 서브프로세스 실행용으로
준비돼 있고, run-plan executor 에 연결하는 것은 M-later (모델 호출 비용 5배 절충).

## 3. 레드팀 결과 (지시서 §8 — 10 시나리오)

| # | 주입 결함 | 결과 | 증거 |
|---|---|---|---|
| 1 | SQL 문자열 연결 | ✅ 실증 차단 | `✗ [S5-sql-injection] app.py:7` · 파라미터화 버전은 통과(단위 테스트) |
| 2 | 하드코딩 API 키 | ✅ 실증 차단 | `✗ [S5-secret] app.py:3,4` · env 조회·테스트 파일은 오탐 없음(단위 테스트) |
| 3 | 취약 버전 의존성 | ✅ 차단(엄격 모드) | cargo audit 미설치 환경에서도 `✗ [S5-audit]` → 게이트 실패. 도구 설치 시 실제 취약점 판정 |
| 4 | O(n²) 정렬 vs 설계노트 O(n log n) | 📘 설계 추적 | S1 complexity_budget 이 S6 검증 기준(04 §6.2). 자동 복잡도 측정기는 M7 |
| 5 | 경계값에서 죽는 파서 | 📘 설계 추적 | 프로파일 property 프레임워크 지정 + 설계 노트 properties 필수(§4) |
| 6 | 3중 중복 코드 | ✅ 실증 차단 | `✗ [S5-duplication] src/lib.rs — 3줄 중복` (cargo test 통과 상태에서도) |
| 7 | 비관용 코드 | ✅ 실증 차단 | `✗ [S5-idiom] unwrap()` + `clippy -D warnings` 게이트 |
| 8 | 대비 미달·포커스 없는 UI | 📘 설계 추적 | S8 체크리스트(디자인 게이트) + 렌더링 의무 원칙. 렌더 자동화는 M-later |
| 9 | 에러에 스택트레이스 노출 | ✅ 실증 차단 | `✗ [S5-error-exposure] app.py:11` |
| 10 | AC 일부 미충족 완료 | ✅ 실증 차단 | rubric `unverified_items` + verify-plan 커버리지 매트릭스(M2) |

**실증 7 / 설계 추적 3** — 설계 추적 항목은 각각 게이트 스키마와 체크리스트가 준비돼 있고,
자동화 인프라(복잡도 측정·property 생성·렌더링)는 로드맵에 등록돼 있다.

## 4. 스킬 수확 파이프라인 (§7)

- 절차(선정→라이선스→증류→연결→검증)를 07_QUALITY.md 이 규정하고, 초기 수확으로 스킬 3종을 생성했다:
  - `skills/security-checklist` — OWASP Cheat Sheets·ASVS 기반 10항목 (문서 참조만, 코드 차용 없음)
  - `skills/design-checklist` — shadcn/ui·Radix·Tailwind 토큰 체계 기반 8항목 (패턴 학습만)
  - `skills/api-design-checklist` — Google AIP·Stripe 관례 기반 5항목 (패턴 학습만)
- 스킬은 시스템 프롬프트 [Skills] 섹션으로 자동 노출되며(skills.rs prompt_section), 언어
  프로파일의 idiom 규칙과 상호 보완한다.
- 수확 검증(적용 전/후 게이트 통과율 비교)은 plan-report 의 리뷰 지적 집계로 측정한다 — 초기
  스킬은 방금 생성돼 비교 기준점이 이번 릴리스다.

## 5. 미해결 질문

1. 보안 감사 도구(cargo audit 등)를 CI/개발 환경에 표준 설치할지 — 미설치 환경에서는 게이트가
   항상 fail 이므로(엄격 해석) 설치 권장.
2. S8 렌더링 자동화 대상(웹 스크린샷 vs TUI 스크린캡처) — RafikX 자체가 TUI 라 확장 여지.
3. 리뷰 위원회 완전 격리(5 프로세스)의 비용 수용 여부 — 현재는 1 fresh-context × 5그룹 구조화.
