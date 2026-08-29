# 05 — 구현 로드맵 (Phase 5)

> 원칙(지시서 7.1): 마일스톤 1은 반드시 검증 인프라다. 각 마일스톤의 태스크는 04_DESIGN §6.2 스키마로 정의한다.
> 상태: M1~M4 완료(구현+테스트+실증), M5·M6 대기.

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

## 마일스톤 2 — 계획 시스템 + 검증 기본 상향 (완료 ✅ 2026-08-29)

| 태스크 | 내용 | 검증(증거) | 상태 |
|---|---|---|---|
| T-M2-1 | PlanDoc 스키마 + AC 커버리지 매트릭스 자동 검사 + `rafikx verify-plan` CLI (G6,G10) | `cargo test verify::plan` 2 passed; 실측 — 미매핑 AC-3·검증 없는 T-2 감지 exit 1, 정상 계획 exit 0 | ✅ |
| T-M2-2 | 계획 프롬프트에 [매핑] 지시 추가 — AC 미매핑 계획은 불완전(MUST) (G10) | runner.rs plan_system_prompt | ✅ |
| T-M2-3 | AcceptUnknown 폐지 — 재질의 후 판정 불능은 Report(fail) 로 통과 금지 (G11) | `cargo test` gate_action 테스트 갱신 통과 | ✅ |
| T-M2-4 | 테스트 수 래칫 — evidence.tests_passed 파싱 + LEDGER 최고점 대비 후퇴 감지 (G17) | 실측 — 20→10 후퇴 시도 차단, 원장 metric 기록 | ✅ |
| T-M2-5 | 검증 정책 기본값 Inherit→Auto (사용자 승인) + 자동 검증이 tests/ 있으면 `cargo test --quiet` (G5) + 무변경 턴 검증 스킵 | `cargo test` 364 passed | ✅ |
| T-M2-6 | tests/acceptance 불변 가드 — acceptance 경로 변경은 무조건 위반 (G4 선행) | guard::check_acceptance_immutable + 흐름 결합 | ✅ |

전체 회귀: `cargo test` **364 passed / 0 failed**.

## 마일스톤 3 — 스펙 인터뷰 + acceptance 불변 (완료 ✅ 2026-08-29)

| 태스크 | 내용 | 검증(증거) | 상태 |
|---|---|---|---|
| T-M3-1 | 인터뷰 루프 — 시스템 프롬프트 [스펙 우위] 섹션: 해석 후보·질문 ≤5·'가정' 명시·동결 기준 임의 변경 금지 (G9) | runner.rs system_prompt | ✅ |
| T-M3-2 | SpecDoc 스키마 + `rafikx spec-freeze` CLI — AC 검증 방법 필수·가정/인터뷰 기록 필수·동결 후 덮어쓰기 거부 (G9) | 실측: 정상 동결 exit 0(frozen=true), 무단 변경 "이미 동결된 SPEC", 불완전 SPEC 2건 사유 리포트 후 거부 | ✅ |
| T-M3-3 | tests/acceptance 쓰기 경로 차단 — resolve_tool_path 게이트(파일 도구 전체 커버: edit/write/multi_edit/apply_patch), 읽기는 자유 (G4) | 단위 테스트 acceptance_paths_are_write_blocked_for_agent_tools 통과 | ✅ |

전체 회귀: `cargo test` **368 passed / 0 failed**.

**판단 기록 — tests/acceptance 도입 시점: M3 (스펙 인터뷰와 동시) — ✅ 이행 완료.** 이유: acceptance test 는
동결된 SPEC AC 의 실행 형태다. 동결 게이트 없이 먼저 도입하면 그것들은 그저 일반 테스트일 뿐
Executor 가 고칠 수 있는 파일이 되어 취지가 사라진다. M1·M2 에서 이미 무결성 가드가
tests/ 하위 변경을 전수 검사하고 acceptance 경로는 무조건 위반 처리하므로 기술적 기반은 준비돼 있다.
이행 내용: (1) SpecDoc 동결(spec-freeze) 2) 도구 계층 쓰기 차단(resolve_tool_path 게이트,
읽기는 자유) 3) 검증 시 acceptance 경로 변경 무조건 위반(guard).

## 마일스톤 4~6

| 마일스톤 | 내용 | 갭 |
|---|---|---|
| M4 역할 분리 | Executor 입력을 태스크 문서로 제한, Verifier 입력=SPEC+diff+evidence | G12 |

## 마일스톤 4 — 역할 분리·오케스트레이션·재개 (완료 ✅ 2026-08-29)

| 태스크 | 내용 | 검증(증거) | 상태 |
|---|---|---|---|
| T-M4-1 | WorkRun — SPEC 게이트(미동결 실행 거부)·재개 지점·완료 집계 | 단위 테스트 3종 (재개 인덱스·미동결 거부·전부 완료) | ✅ |
| T-M4-2 | run-plan 오케스트레이터 — Executor(서브프로세스 격리)→검증→체크포인트 루프 | 실측: 2태스크 전부 Done, 태스크별 git 커밋(task(T-1)·task(T-2)) | ✅ |
| T-M4-3 | 사다리 1단계+에스컬레이션 — executor 재시도(1회·피드백 첨부) 후 Escalated, 의존성 안전 중단 (G18 일부) | 실측: T-3 실패 시도 2회→ESCALATED·[이후] 중단 표시 | ✅ |
| T-M4-4 | 재개 — Done 신뢰(load_trusting_state)·미완료부터 재실행 | 실측: T-1·T-2 건너뛰고 T-3부터 재개, 원장 전이 기록 | ✅ |

구현 파일: verify/{work,orchestrator}.rs, main.rs(RunPlan CLI), plan.rs(load_trusting_state).
컨텍스트 격리: Executor 는 서브프로세스로 instructions 만 환경변수(RAFIKX_TASK_INSTRUCTIONS)로
전달받는다 — 부모 대화와 물리적으로 분리되어 "태스크 문서만 받는다"는 계약이 아키텍처로 강제된다.
체크포인트 재검증(마지막 Done 회귀 재실행)은 M6 항목으로 유지한다.
| M5 모델 보정 | calibrate 프로브 세트, (난이도×모델능력) 2차원 하네스, 파싱 재시도 루프 | G13,G14,G16,G18 |

**판단 기록 — 보정 스위트 프로브 선정 (M5 확정안, 9문항 3군):**
- 군 A 지시 준수(3): ①지정 행수 JSON 정확 출력 ②금지 단어 미사용(부정 지시) ③3단계 지정 순서 수행
- 군 B 구조화 출력(3): ④JSON 스키마 3회 연속 준수 ⑤도구 호출 인자명 정확성 ⑥마크다운 표 열수 유지
- 군 C 미니 코딩(3): ⑦한 줄 오타 수정 ⑦순수 함수 구현(엣지 케이스 포함) ⑨기존 테스트 파괴 변경 감지·보고
채점: 군별 통과율 → 능력 점수 0~1 → 하네스 파라미터(태스크 입도·재시도 예산·검증 Auto↔Strict) 자동 조정.
| M6 관측성·재개 | LEDGER 통합 리포트, 검증 체크포인트 재개, PROGRESS.md 자동 갱신, 교훈 검증화 | G12,G19,G20,G21 |

## 프롬프트 강화 (즉시 반영 완료 ✅)

| 항목 | 위치 |
|---|---|
| 시스템 프롬프트 [증거 우위 — 거짓 완료 원천 차단] 섹션 신설 — exit code 없는 완료 주장 금지·테스트 약화 금지·무변경 완료 금지·작게 만들고 즉시 검증 | harness/runner.rs:88 |
| 검증자 프롬프트 판정 전 필수 검사 4종 — 테스트 무결성·하드코딩 탐지·AC 커버리지·자기 보고 배제 | harness/runner.rs:1705-1715 |
