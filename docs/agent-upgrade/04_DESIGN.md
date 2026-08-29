# 04 — 목표 아키텍처 설계 (Phase 4)

> 원칙: 기존 구조(Inspector·하네스·메모리·ULW)를 목표로 진화시킨다. 폐기 항목 없음 — 축소/역할 선명화만 있다.
> 본 문서는 구현 가능한 수준의 데이터 구조·상태 머신·모듈 경계를 정의한다.

## 6.1 역할 분리와 컨텍스트 격리

| 역할 | 담당 | 입력(컨텍스트 격리 경계) | 출력 | 기존 자산 매핑 |
|---|---|---|---|---|
| **Interviewer/Spec-Writer** | 사용자 의도 → SPEC | 사용자 메시지, (재)질문 이력 | SPEC.md(AC 포함) | 신규 (M3). 시스템 프롬프트 [의도 게이트]는 그대로 1차 필터 |
| **Planner** | SPEC → 태스크 그래프 | SPEC.md | plan/tasks/*.yaml | 기존 plan_system_prompt 진화 — [계획][완료 기준][반박] 섹션을 태스크 데이터로 승격 |
| **Plan-Critic** | 계획 공격 | SPEC + 계획 (**Planner 컨텍스트 비공유**) | 구조화 지적 목록 | 기존 critic 노드(runner.rs:1802+)를 별도 컨텍스트 호출로 승격 — 현재는 같은 실행의 노드라 격리 약함 |
| **Executor** | 태스크 1개 수행 | **태스크 문서만** (전체 이력 금지) | diff + 자기 점검 | 기존 task 위임(tools/task.rs)이 이미 자식 컨텍스트 — 입력을 태스크 문서로 제한하는 확장 |
| **Verifier** | 판정 | SPEC + 태스크 정의 + diff + 증거 원장 (**Executor 이력 비공유**) | pass / fail(사유·재작업 지시) | 기존 독립 검증자 게이트(runner.rs:1541+)가 씨앗 — review_prompt가 이미 "diff 미첨부·직접 읽기" 원칙 유지 |

**Inspector**는 판정 조직에서 제외한다: 통계·이상 감지·리포트 전담(inspector.rs 현행 유지). 판정권은 Verifier만. **불변식: 상태를 바꿀 수 있는 역할은 Planner(생성)와 Verifier(판정)뿐이고, Executor 는 그 어느 쪽 권한도 갖지 않는다.**

## 6.2 태스크 스키마와 상태 머신 (완료의 정의를 데이터로)

```yaml
task:
  id: T-014
  title: "config 파서에 환경변수 오버라이드 추가"
  spec_refs: [AC-3, AC-7]
  depends_on: [T-011]
  context_files: [src/config.rs]
  instructions: |
    (자기완결적 — 이 문서만으로 수행 가능)
  constraints:
    max_diff_lines: 300
    forbidden_paths: [tests/acceptance/]
  verification:                    # 완료의 유일한 정의
    - cmd: "cargo build"
      expect_exit: 0
    - cmd: "cargo test config::"
      expect_exit: 0
  evidence: []                     # 시스템 전용 쓰기 영역 — 모델 출력이 채우지 않는다
  state: PENDING
```

증거 항목(evidence entry, 시스템이 기록):

```json
{"cmd":"cargo test config::","exit_code":0,"started_at":1730000000,
 "duration_ms":4120,"output_tail":"…test result: ok. 12 passed…",
 "diff_hash":"blake3:…","diff_stat":"+58 -12","changed_files":[…]}
```

**상태 머신과 불변식 (Rust 타입으로 강제)**

```
PENDING → IN_PROGRESS → VERIFYING → DONE
                     ↘ REWORK → IN_PROGRESS
임의 상태 → ESCALATED (예산 초과·사다리 소진)
```

핵심: `VERIFYING → DONE` 전이를 **타입 시스템으로 모델에게 봉인**한다.

```rust
// VERIFYING 토큰을 얻는 유일한 길은 시스템이 verification 을 실행한 결과다.
pub struct Verifying<'a> { task: &'a mut TaskDoc, report: VerifiedReport }
// VerifiedReport 는 runner 가 cmd 실행 결과에서만 생성 — From<CmdResults> 만 존재.
impl Verifying<'_> {
    pub fn finish(self, verdict: VerifierVerdict) -> TaskOutcome {
        match verdict { Pass => TaskOutcome::Done(self.report), Fail(r) => TaskOutcome::Rework(r) }
    }
}
// TaskDoc::state 필드는 pub 없이 pub(crate) 로 은닉하고, 전이는
// TaskDoc::apply(outcome: TaskOutcome) 한 곳에서만 일어난다.
// 모델(도구 출력 파서)은 TaskOutcome 을 생성할 수 없다 — CmdResults 나
// VerifierVerdict 를 만드는 경로가 전부 시스템 코드에 있기 때문이다.
```

- 모델에게 노출되는 도구(파일 편집·bash)는 diff 와 명령 실행만 낳고, 상태 전이 API를 호출할 수 없다 — **프롬프트가 아니라 가시성으로 차단**.
- 단위 테스트로 불변식 고정: "verification 전부 통과 + Verifier pass 없이 DONE 불가", "모델 출력 파서가 상태를 건드릴 경로 없음".

## 6.3 스펙 인터뷰 프로토콜

1. 요청 수신 → 해석 후보 나열(내부, 출력 없음).
2. 모호성 감지 → 영향도 순 정렬 질문 **최대 5개** 한 번에 제시. 사소한 것은 "가정: …" 명시.
3. SPEC 초안: 목표 요약 / 범위 내·외 / **acceptance criteria(각각 검증 방법 포함)** / 비기능 요구 / 가정 목록.
4. 사용자 승인 → **SPEC 동결** (`SPEC.md` front-matter에 `frozen: true` + 승인 시각). 변경은 변경 요청 절차(사유 기록 + 재승인)만.
5. AC는 가능한 한 실행 가능한 acceptance test로 먼저 작성 → `tests/acceptance/` (**Executor 쓰기 금지 — 도구 계층 경로 차단**, 기존 reject_vault(tools.rs)와 같은 패턴으로 구현).

기존 [의도 게이트] 프롬프트(runner.rs:42)는 Interviewer의 1차 필터로 유지하고, 질문·승인·동결은 코드 장치로 승격한다.

## 6.4 계획 이중 검증 (Plan → Attack → Revise)

1. Planner가 태스크 그래프 생성 (AC 매핑 필수 — `spec_refs` 비어 있는 태스크는 생성 불가).
2. **Plan-Critic이 별도 컨텍스트에서 공격**: "이 계획이 실패할 방법 10가지를 찾아라. 누락된 의존성, 검증 불가능한 태스크, 미매핑 AC, 크기 기준 위반을 지적하라." — 기존 [반박] 지시(runner.rs:241)를 독립 호출로 승격.
3. Planner 수정 → 재공격 → 수렴(Critical 지적 0 또는 반복 상한 2회)에서 확정.
4. 확정 조건 자동 검사: **모든 AC가 ≥1 태스크에 매핑(커버리지 매트릭스)**, 모든 verification cmd 존재, 크기 기준 준수. 실패 시 확정 거부.
5. 확정 계획은 `plan.yaml` + `tasks/*.yaml` 로 영속화.

## 6.5 실행 루프와 품질 래칫

```
태스크 문서 로드 → Executor 구현 → 자기 점검 체크리스트
→ 시스템이 verification 실행 (exit code 수집 → evidence)
   → 실패: 로그 반환, 재시도 (사다리 §6.8 예산 내)
   → 통과: 회귀 게이트 (build + 전체 test + lint)
      → 실패: 후퇴 — 원인 태스크 REWORK
      → 통과: Verifier 판정 (diff 리뷰 + AC 매핑 + 테스트 무결성)
         → pass: git 커밋(체크포인트) + evidence 기록 + DONE
         → fail: 구조화 재작업 지시와 함께 REWORK
```

**품질 래칫**: `LEDGER.jsonl`에서 최근 통과 상태(테스트 수, clippy 무경고, 커버리지 하한)를 읽어 현재 실행 결과와 비교 — **후퇴 감지 = 즉시 REWORK**. 구현은 게이트 단계에서 스칼라 비교 한 번이므로 저렴하다.

기존 run_verify(runner.rs:1424)는 이 루프의 "verification 실행" 단계로 흡수된다. 차이: (1)명령이 태스크 데이터에서 나옴, (2)결과가 evidence 원장에 남음, (3)회귀 게이트가 기본 포함.

## 6.6 테스트 무결성 가드

1. **diff 스캔**: 태스크 diff에서 `tests/` 하위 변경을 감지. 규칙:
   - `#[ignore]`/`#[cfg(ignore)]` **추가** 라인 → 자동 fail (Verifier 명시 승인 사유 없을 때)
   - `#[test]` 함수 **삭제**·어서션(assert!/assert_eq! 라인) **순감소** → 자동 fail
   - `expect`/기대값 문자열이 느슨해지는 패턴(예: `assert_eq!` → `assert!`) → 경고 후 Verifier 검토
2. **경로 차단**: `tests/acceptance/` 쓰기 시도는 도구 계층에서 거부(사유: "acceptance 는 SPEC 동결 산출물").
3. **하드코딩 탐지**: Verifier 리뷰 체크리스트에 "테스트 입력에만 특화됐는가" 추가 + 의심 시 변형 입력 테스트 추가 지시. 리뷰어 프롬프트(review_prompt)에 고정 항목으로 삽입.

구현 위치: 신규 `verify::guard` 모듈(diff는 similar 크레이트 — 이미 의존성 있음 — 또는 git diff 텍스트 파싱).

## 6.7 모델 어댑터와 보정

- 어댑터는 이미 충족(provider/, 4-1 점수 4). 보강: 구조화 출력 요청 시 파싱 실패 → 오류 첨부 수정 재요청 **최대 2회** → 초과 시 에스컬레이션(G13).
- **보정 스위트**(신규 `calibrate`): 소형 프로브 세트 — (a)지시 준수(형식 지키기), (b)구조화 출력(JSON 스키마 준수율), (c)미니 코딩 태스크 3종(정답률). 결과를 `model_profile.yaml`에 기록.
- **하네스 확장**: 기존 1차원(난이도→프로파일)을 **(난이도 × 모델 능력) 2차원**으로 — 보정 점수가 낮을수록: 태스크 입도 축소(Planner가 분할 한도 하향), 자기 비평 반복 +1, 검증 정책 Auto→Strict 강제, 재시도 예산 +1. 바인딩 결정(binding.rs)에 모델 프로필 조회를 추가하는 형태라 구조 변경 없음.

## 6.8 실패 처리와 에스컬레이션 사다리

| 단계 | 동작 | 상한 |
|---|---|---|
| 1 재시도 | 동일 접근 + 실패 로그 첨부 | 2회 |
| 2 접근 변경 | "이전 접근은 실패했다. 다른 접근을 시도하라" + 실패 이력 | 2회 |
| 3 태스크 재분해 | Planner 반환, 더 작은 태스크로 분할 | 1회 |
| 4 에스컬레이션 | 막힌 지점·시도 이력·질문 1~3개를 사용자에게 보고 | — |

각 태스크에 `budget: {minutes, tokens, attempts}` — 초과 시 현 단계 무관하게 4로 강제 이동. 기존 장치(동일호출 3회 차단 agent.rs:40, 검증 3라운드 runner.rs:1458)는 사다리 1단계의 구현으로 흡수된다.

## 6.9 상태 영속화와 재개

```
.rafikx-work/<run_id>/
  SPEC.md          (동결 플래그 포함)
  plan.yaml        (태스크 그래프)
  tasks/T-*.yaml   (개별 태스크 + evidence)
  LEDGER.jsonl     (이벤트 원장: 모델 호출·도구·상태 전이·비용)
  PROGRESS.md      (사람용 요약 — 기존 PROGRESS.md 워크플로 흡수)
```

- **재개 절차**: 시작 시 LEDGER+태스크 상태 로드 → 첫 미완료 태스크부터 재개 → 재개 직전 **마지막 체크포인트의 회귀 게이트 재실행**으로 상태 파일과 실제 코드 불일치 감지(불일치 시 ESCALATED).
- 기존 PROGRESS.md 3줄 워크플로(AGENTS.md)는 "사람이 읽는 요약" 계층으로 시스템이 자동 갱신하는 파일로 흡수된다(형식 유지).
- ULW의 goal.md/state.json(ulw.rs:136)은 이 구조의 선행 구현 — 동일 스키마로 흡수.

## 6.10 관측성

- 모든 모델 호출·도구 실행·상태 전이를 `LEDGER.jsonl` 이벤트로 기록: `{ts, run_id, task_id, role, event, cost, duration_ms, result}` — 기존 applog(graph 노드·lifecycle 이벤트)와 병행, LEDGER 가 "증거" 계층을 담당.
- 실행 종료 요약 리포트: 태스크별 시도 횟수·실패 사유 분포·총 비용/시간·게이트 통과 이력 — 기존 usage.rs+graph 집계를 LEDGER 소스로 교체.

## 트레이드오프

| 결정 | 대안 | 선택 이유 | 비용 |
|---|---|---|---|
| 완료 판정을 타입 시스템으로 봉인 | 프롬프트 강화만 | 프롬프트는 우회 가능, 가시성은 불가능 | 상태 전이 코드가 한 곳으로 모임(변경 비용↑) |
| YAML 태스크 파일 | DB 테이블 | 사람이 읽고 고칠 수 있어 재개·감사에 유리, 기존 .rafikx-work 관례와 일치 | 대규모 동시성엔 DB가 유리 — 현재 단일 실행 전제 |
| 전체 회귀 게이트 기본 포함 | 태스크 검증만 | 후퇴(3-6)는 태스크 검증으로 못 잡음 | 큰 저장소에서 느림 — 난이도 하네스로 simple 은 subset 실행으로 완화 가능 |
| acceptance 를 도구 경로 차단 | 모델 서약 | 경로 차단은 우회 불가 | 긴급 수정 시 사용자 승인 절차 필요 |
| Inspector 판정권 없음 유지 | Inspector 에 판정 부여 | 진단과 판정의 독립 유지(한 조직이 채점하고 채점 검증까지 하면 오염) | 역할이 하나 더 늘어남(Verifier) |
