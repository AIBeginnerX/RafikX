# 03 — 갭 분석과 우선순위 (Phase 3)

> 심각도 규칙: Critical = 거짓 완료 허용 경로(축 3 점수<3 자동)·데이터 파괴 가능 / High = 모호 실행·계획 없는 실행·재개 불가 / Medium = 품질 루프 약함·모델 교체 수동·관측성 공백 / Low = 편의·문서
> 개선 순서 원칙: **축 3 → 축 2 → 축 1 → 나머지**

## 갭 목록

| ID | 갭 | 심각도 | 근거 | 채점 |
|---|---|---|---|---|
| G1 | 완료 1차 신호가 모델 종료 행동이고, 시스템 검증은 binding/policy 에 따라 스킵 가능 (Inherit 기본) | **Critical** | agent.rs:444-448(status="ok"), runner.rs:1251-1255, engine.rs:176(Inherit) | 3-1:2 |
| G2 | 증거 원장 부재 — 검증 명령·exit code·diff 해시가 태스크 단위로 기록되지 않음 | **Critical** | runner.rs:1480-1483(성공 시 요약만), db.rs:1290-1308 | 3-2:1 |
| G3 | 테스트 무결성 가드 부재 — `#[ignore]` 추가·테스트 삭제·어서션 약화 미탐지 | **Critical** | 01_AUDIT Q3 (해당 코드 없음) | 3-4:0 |
| G4 | acceptance test 소유권·읽기 전용 잠금 부재 | **Critical** | tests/acceptance 개념 없음, reject_vault만 존재(tools.rs) | 3-5:0 |
| G5 | 회귀 게이트 기본값이 `cargo check` — 테스트 미실행이 기본 | **Critical** | runner.rs:2482-2485 | 3-6:1 |
| G6 | AC↔태스크 커버리지 매트릭스 부재 | **Critical** | dod_checklist 텍스트 대조만 존재(runner.rs:642) | 3-7:1 |
| G7 | 하드코딩/스텁 탐지 장치 부재 (리뷰어 프롬프트에도 없음) | **Critical** | review_prompt runner.rs:1702-1726, 프롬프트 runner.rs:87 | 3-8:1 |
| G8 | diff 부재(수정 0) 완료 차단 부재 | **Critical** | changed_files 미사용 검증 경로, ulw.rs:87-88 | 3-1 파생 |
| G9 | 모호 요청이 확인 없이 실행됨 — 인터뷰·SPEC 승인·동결 부재 | High | runner.rs:42-45(프롬프트 의존), 축1 전체 | 1-1~1-6 |
| G10 | 계획이 데이터가 아님 — 태스크 스키마·의존성·크기 기준·재계획 트리거 부재 | High | 축2 스코어카드 | 2-1~2-6 |
| G11 | 판정 불능(AcceptUnknown) 통과 경로 | High | runner.rs gate_action | 3-3 보강 |
| G12 | 일반 경로 계획/증거 미영속 — 재개 시 계획 유실 (ULW만 goal.md) | High | ulw.rs:136 외 없음 | 6-1,6-2 |
| G13 | 구조화 출력 파싱 실패 재시도 루프 부재 | Medium | lessons.rs:192(스킵), runner.rs:1594(본문 fallback) | 4-2:1 |
| G14 | 모델 보정 스위트 부재 | Medium | model_wizard ping만 | 4-6:0 |
| G15 | git 체크포인트·태스크 커밋 부재 (mutation 롤백만) | Medium | tools/mutation/commit.rs:142 | 5-2:1 |
| G16 | 변경 크기 제한 부재 | Medium | 해당 없음 | 5-3:0 |
| G17 | 품질 래칫 부재 (통과 테스트 수 추적 없음) | Medium | 해당 없음 | 4-5:0 |
| G18 | 실패 사다리 미데이터화 (재시도→접근변경→재분해→에스컬레이션) | Medium | agent.rs:40(3회 차단), runner.rs:1492(2회) | 5-5:2 |
| G19 | 검증 출력 원문·exit code 비보존으로 사후 분석 약함 | Medium | runner.rs:1480-1483 | 7-3:2 |
| G20 | 컨텍스트 요약이 검증 여부 구분 안 함 | Low | chat.rs:1567 | 6-3:2 |
| G21 | lessons가 모델 반성 텍스트라 "검증된 지식" 아님 | Low | lessons.rs:192 | 6-4:2 |

## 우선순위 큐 (원칙: 3 → 2 → 1 → 나머지)

| 순위 | 갭 | 마일스톤 | 근거 |
|---|---|---|---|
| 1 | G1,G2,G5,G8 — **검증 실행기 + 증거 원장 + diff 부재 차단** | **M1** | 거짓 완료의 주 경로. 시스템이 직접 실행한 exit code 만이 증거 |
| 2 | G3,G4 — **테스트 무결성 가드 + acceptance 잠금** | **M1** | 모의 Executor 의 최우선 우회로 |
| 3 | G7,G17 — Verifier 리뷰 항목에 하드코딩 검사+변형 입력, 통과 테스트 수 래칫 | M1(M2 연계) | 리뷰 프롬프트 확장은 즉시 가능 |
| 4 | G10,G6 — 태스크 스키마(YAML/JSON)·의존성·AC 커버리지 매트릭스 | **M2** | 계획을 데이터로 |
| 5 | G11 — 판정 불능 시 에스컬레이션(통과 금지) | M2 | Verifier 판정 강화 |
| 6 | G9 — 스펙 인터뷰 프로토콜 + 승인 게이트 | **M3** | 모호함은 실행 전에 |
| 7 | G12 — SPEC/PLAN/tasks/LEDGER 파일 영속화 + 검증 체크포인트 재개 | M4/M6 | 장기 작업 재개 |
| 8 | G13,G14 — 출력 파싱 재시도, 보정 스위트 | M5 | 모델 독립성 완성 |
| 9 | G15,G16,G18 — git 체크포인트, diff 크기 제한, 사다리 데이터화 | M5 | 안전 고도화 |
| 10 | G19,G20,G21 — 원장 기반 관측성·요약 검증 구분·교훈 검증화 | M6 | 마무리 |

## 기존 자산의 진화 방향 (폐기 없음)

| 자산 | 진화 경로 |
|---|---|
| Inspector(inspector.rs) | **진단 조직 유지 + 판정권 부여 않음** — Verifier(신규)가 판정, Inspector는 통계·이상 감지 담당으로 역할 선명화 |
| 독립 검증자 게이트(runner.rs:1541+) | Verifier 역할의 씨앗 — 태스크 스키마의 evidence·AC 를 입력으로 확장하고 AcceptUnknown 경로 제거(G11) |
| ULW 품질 게이트(ulw.rs:260-335) | 검증 우선 원칙의 기존 구현 — M1 실행기로 내부 구현을 일원화하고 일반 경로에도 동일 강제 적용 |
| 난이도 하네스(classify/binding) | "난이도 × 모델 능력" 2차원 매트릭스로 확장(M5, 보정 스위트 접합) |
| lessons/facts | LEDGER 의 검증 결과를 교훈 생성 조건으로 추가해 "검증된 지식"으로 승격(G21) |
| PROGRESS.md | 사람용 요약으로 유지하되 완료 태스크 요약을 시스템이 자동 첨부(6.9 매핑) |
