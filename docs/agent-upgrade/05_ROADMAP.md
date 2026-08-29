# 05 — 구현 로드맵 (Phase 5)

> 원칙(지시서 7.1): 마일스톤 1은 반드시 검증 인프라다. 각 마일스톤의 태스크는 04_DESIGN §6.2 스키마로 정의한다.
> 상태: M1 완료(구현+테스트+레드팀 실증), M2~M6 대기.

## 마일스톤 1 — 검증 인프라 (완료 ✅)

태스크 문서(6.2 스키마, JSON):

```json
{"id":"T-M1-1","title":"태스크 스키마와 봉인된 상태 전이",
 "spec_refs":["3-1","3-2"],
 "verification":[{"cmd":"cargo test verify::"}],"state":"DONE"}
```

| 태스크 | 내용 | 검증(증거) | 상태 |
|---|---|---|---|
| T-M1-1 | `verify::task` — TaskDoc 스키마·기본값(require_diff=true 등)·봉인된 상태 전이(apply)·DONE 파일 재로드 시 Pending 정규화 | `cargo test verify::task` 3 passed (exit 0) | ✅ |
| T-M1-2 | `verify::runner` — 명령 직접 실행·exit code 수집·diff 수집·diff 부재 차단 | `cargo test verify::runner` 2 passed (exit 0) | ✅ |
| T-M1-3 | `verify::guard` — `#[ignore]` 추가·테스트 삭제·어서션 순감소 감지 | `cargo test verify::guard` 3 passed (exit 0) | ✅ |
| T-M1-4 | LEDGER.jsonl 원장 + `rafikx verify-task <json>` CLI | 레드팀 시나리오 1~4 실실행(06_REDTEAM.md) exit 1·REWORK 기록 | ✅ |
| T-M1-5 | 테스트 전체 회귀 | `cargo test` 360 passed / 0 failed (exit 0) | ✅ |

구현 파일: `agent-harness/src/verify/{mod,task,runner,guard}.rs`, `main.rs`(VerifyTask 서브커맨드), `lib.rs`.
불변식 강제: `CmdResults`·`VerifierVerdict` 의 생성자가 verify 모듈 내부에만 존재 — 모델 출력 파서는 `TaskOutcome::Done` 을 만들 경로가 없다(가시성 봉인, 04_DESIGN §6.2).

## 마일스톤 2 — 계획 시스템 (대기)

| 태스크 | 내용 | 비고 |
|---|---|---|
| T-M2-1 | plan.yaml/tasks/*.yaml 파서+AC 커버리지 매트릭스 자동 검사 (G6,G10) | Planner 입출력을 데이터로 |
| T-M2-2 | Plan-Critic 별도 컨텍스트 호출 (G10) — 기존 critic 노드 승격 | 격리 원칙 6.1 |
| T-M2-3 | AcceptUnknown 제거 — 판정 불능은 에스컬레이션 (G11) | runner.rs gate_action 수정 |
| T-M2-4 | 통과 테스트 수 래칫 — LEDGER 기반 스칼라 비교 (G17) | |
| T-M2-5 | 검증 정책 기본값 상향 — Inherit에서 Auto로 (G1 완화) | engine.rs:176 |

## 마일스톤 3 — 스펙 인터뷰 (대기)

| 태스크 | 내용 |
|---|---|
| T-M3-1 | 인터뷰 루프 — 모호성 질문 ≤5개 제시 + 가정 명시 (G9) |
| T-M3-2 | SPEC.md 생성·승인·동결 플래그 (G9) |
| T-M3-3 | tests/acceptance/ 생성 + Executor 쓰기 경로 차단 (G4) |

## 마일스톤 4~6

| 마일스톤 | 내용 | 갭 |
|---|---|---|
| M4 역할 분리 | Executor 입력을 태스크 문서로 제한, Verifier 입력=SPEC+diff+evidence | G12 |
| M5 모델 보정 | calibrate 프로브 세트, (난이도×모델능력) 2차원 하네스, 파싱 재시도 루프 | G13,G14,G16,G18 |
| M6 관측성·재개 | LEDGER 통합 리포트, 검증 체크포인트 재개, PROGRESS.md 자동 갱신, 교훈 검증화 | G12,G19,G20,G21 |

## 프롬프트 강화 (즉시 반영 완료 ✅)

| 항목 | 위치 |
|---|---|
| 시스템 프롬프트 [증거 우위 — 거짓 완료 원천 차단] 섹션 신설 — exit code 없는 완료 주장 금지·테스트 약화 금지·무변경 완료 금지·작게 만들고 즉시 검증 | harness/runner.rs:88 |
| 검증자 프롬프트 판정 전 필수 검사 4종 — 테스트 무결성·하드코딩 탐지·AC 커버리지·자기 보고 배제 | harness/runner.rs:1705-1715 |
