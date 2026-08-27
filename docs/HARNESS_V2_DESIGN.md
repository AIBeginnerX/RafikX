# RafikX Harness v2 — 멀티 엔진 · 분야 선택 · 전문가 파이프라인 설계

> 설계: Claude Fable 5 (2026-08-26). 구현: claude-opus-4-8 위임.
> 근거 조사: DeepSeek Harness(dsh) 아키텍처, Qwen Code/Qwen3-Coder Agent RL, Claude Code(arXiv 2604.14228)·Anthropic Agent SDK 원칙, Kimi K2 기술 리포트(arXiv 2507.20534), Self-Harness(arXiv 2606.09498), MetaGPT 멀티롤 산출물 계약.

## 0. 목표와 비목표

**목표**
1. 엔진 카탈로그 확장: `rafikx | claude | deepseek | qwen | kimi | pi` — 각 엔진은 해당 하네스의 **검증된 품질 장치**를 구현한다 (이름 차용이 아님).
2. Self-Harness를 "엔진 중 하나"에서 **엔진 독립 메타 레이어**로 승격 — 어떤 엔진 위에도 자기개선 루프가 겹쳐진다.
3. 실행 분야(discipline) 선택: `harness | loop | graph` — 같은 엔진이라도 실행 제어 전략을 바꿀 수 있다.
4. 전문가 역할 파이프라인: 기획(planner) → 구현(frontend/backend/coder) → 독립 리뷰(reviewer). 역할 간 통신은 자유 대화가 아니라 **구조화 산출물**(스펙/DoD → diff → 리뷰 리포트).
5. "한번에 완료": 계획 단계가 **완료 기준(DoD) 체크리스트**를 산출하고, 종료 전 **독립 검증자 게이트**가 DoD 대조를 통과해야 완료 선언.

**비목표 (이번 범위 제외)**
- 컴팩션 사다리 개편 (기존 80% auto-compaction 유지)
- 병렬 그래프 노드 실행 (graph discipline은 위상순 순차 실행만)
- 새 외부 크레이트 추가 (SPEC 원칙: 기존 스택 + std)
- TUI 시각 개편 (기존 graph_events 표시 재활용)

**설계 원칙**
- 루프는 단순하게, 품질은 주변 시스템(계획·검증·격리)에 투자한다 (2026 수렴점).
- 엔진 차이는 **데이터(EngineSpec)** 로 표현한다. 제어 흐름 분기 최소화.
- 모든 신규 동작은 config로 끌 수 있다 (extensible-by-default).
- 기존 사용자 config.toml과 100% 하위호환. 기존 테스트 전부 통과 유지.

## 1. engine.rs 신설 — EngineSpec 카탈로그

새 파일 `src/engine.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlanDepth { Off, Brief, Contract }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VerifyPolicy { Inherit, Auto, Strict }
// Inherit = 프로파일의 verify 설정 그대로. Auto = verify 강제 on(자동 감지).
// Strict = Auto + 독립 검증자 게이트(§5).

// 구현 반영: 문자열 필드는 Cow 다. 내장 카탈로그는 정적 문자열(Cow::Borrowed)을
// 그대로 쓰고, config `[engines.*]` 오버라이드가 적용된 사본만 소유 문자열
// (Cow::Owned)을 담는다 — 오버라이드 때문에 카탈로그 전체를 복제하지 않기 위해서다.
pub struct EngineSpec {
    pub name: Cow<'static, str>,
    pub summary: Cow<'static, str>,       // /engine 목록 한 줄 설명 (한국어)
    pub prompt_block: Cow<'static, str>,  // 시스템 프롬프트 증강 블록 (한국어)
    pub force_staged: bool,           // 모든 도구 작업에 todo 단계 강제
    pub plan_depth: PlanDepth,        // dev/advanced 클래스에 적용
    pub verify_policy: VerifyPolicy,
    pub max_continuations: u8,        // goal continuation 한도 (기존 8)
    pub pin_provider: Option<Cow<'static, str>>, // §11.1
    pub pin_strict: bool,             // §15.2 — true 면 고정 장애에도 폴백 금지
}

pub fn catalog() -> &'static [EngineSpec];      // 내장 6종 (Phase 4 에서 minimax 추가 → 7종)
pub fn resolve(name: &str) -> Option<&'static EngineSpec>;
/// config 값 정규화: "dk"→"deepseek", "self"→("rafikx", self_meta=true), 빈 값→"rafikx"
pub fn normalize(raw: &str) -> (String, bool);  // (engine_name, legacy_self_flag)
```

내장 카탈로그 (prompt_block 요지 — 실제 문구는 한국어로 정성껏, 각 3~8줄):

| 엔진 | plan | verify | staged | 프롬프트 핵심 (조사 근거) |
|---|---|---|---|---|
| `rafikx` | Brief | Inherit | 기존 판정 | 기본. 증강 없음 |
| `claude` | **Contract** | **Strict** | 기존 판정 | 단순 루프+검증 우선: "todo_write로 계획을 가시화하고 항목 단위로 진행·완료 표시. 큰 탐색은 task 도구로 위임해 부모 컨텍스트를 결론만으로 유지. 완료 선언 전 반드시 검증 실행" (Claude Code TodoWrite·사이드체인·verify-work) |
| `deepseek` | Brief | Inherit | **true** | 단계별 실행: "[N/총M] 단계 보고, 도구 실행 전 의도 선언(pre)·후 결과 요약(post)" (dsh pre/post 훅 파이프라인) |
| `qwen` | Brief | Auto | 기존 판정 | ReAct 명시: "생각→행동→관찰 사이클을 유지하라. 각 관찰에서 계획과의 차이를 확인하고 조정한다. 반복 작업은 동일 패턴으로 일관 처리" (Qwen Code ReAct+스킬 루프) |
| `kimi` | **Contract** | **Strict** | 기존 판정 | interleaved thinking+루브릭: "작업 시작 시 성공 루브릭(기준·예상 도구 패턴·체크포인트)을 명시하고, 도구 호출을 끊지 말고 연쇄 유지. 관찰마다 루브릭 대비 위치를 확인" (K2 기술 리포트) |
| `pi` | Brief | Inherit | 기존 판정 | 기존 저소음 블록 유지 |

- `catalog()`는 config `[engines.<name>]` 테이블로 **필드 단위 오버라이드** 가능해야 한다 (문구·플래그를 코드 수정 없이 튜닝). 오버라이드 구조체: `EngineOverride { prompt_block, force_staged, plan_depth, verify_policy, max_continuations, pin_provider, pin_strict }` 전 필드 Option.
- `is_valid_engine`(chat.rs:418)과 `ENGINES` 상수를 카탈로그 기반으로 교체. `self`는 유효 입력으로 유지하되 normalize가 처리.

### 1.1 run_pipeline_inner 분기 교체

harness.rs:1481-1547의 하드코딩 분기(engine_deep/engine_pi/engine_self)를 제거하고:

```rust
let (engine_name, legacy_self) = engine::normalize(&cfg.file.general.engine);
let spec = engine::resolve(&engine_name).unwrap_or(default);
let self_meta_on = cfg.file.self_harness.enabled && (legacy_self || cfg.file.self_harness.meta);
```

- `spec.prompt_block`을 시스템 프롬프트에 append (비어 있으면 생략).
- `staged = !binding.tools.is_empty() && (spec.force_staged || class != Simple)` (기존 로직 일반화).
- Self-Harness decorate/observe는 `self_meta_on`으로 게이트 (§3).
- goal continuation 한도 8을 `spec.max_continuations`로 치환 (goal_should_continue 시그니처에 전달).

## 2. 계획 단계 강화 — PEV의 P (기존 결함 수정 포함)

**결함 수정**: harness.rs:1551-1557의 plan 호출이 시스템 프롬프트를 통째로 교체해 lessons·system_extra·프로젝트 규칙(AGENTS.md)·Self-Harness bootstrap이 계획에 반영되지 않는다. 수정:

- plan 호출의 system = (메인 system 조립 결과) + "\n\n[계획 모드] 지금은 계획만 세운다. 도구는 쓰지 마라." + depth별 지시.
- max_tokens: Brief=1024(기존), Contract=2048.

**PlanDepth::Contract** (claude/kimi 엔진 × dev|advanced 클래스에서 활성):

계획 산출물을 3부로 강제:
```
[해석] 요구사항을 한 문단으로 재진술 + 모호한 점과 채택한 해석
[완료 기준] 검증 가능한 DoD 체크리스트 3~10항목 (각 항목은 "어떻게 확인하는가"를 포함)
[작업 분해] 실행 순서 3~9단계
```
- 계획 텍스트는 기존처럼 시스템 프롬프트 `[실행 계획]`으로 append.
- 추가로 첫 사용자 메시지에 "위 [작업 분해]를 todo_write로 등록한 뒤 시작하라"를 삽입해 staged goal continuation과 결합.
- DoD 텍스트는 §5 독립 검증자 게이트에 전달되므로 outcome 경로로 보존해야 한다 (AgentOutcome에 넣지 말고 run_pipeline_inner 지역 변수로 유지).

**전문가 사전 준비 (20년 경력자 요구)**: Contract 계획 프롬프트에 "20년 경력 시니어가 착수 전 검토하듯: 기존 코드/파일 구조에 대한 가정을 명시하고, 위험(호환성·회귀·엣지케이스)을 한 줄씩 짚어라"를 포함.

## 3. Self-Harness 메타 레이어 승격

**현행**: engine=="self"일 때만 decorate_system + maybe_observe.
**변경**:
- `SelfHarnessConfig`에 `meta: bool` 추가 (기본 false). `[self_harness] meta = true`면 **모든 엔진** 위에 겹침.
- `engine = "self"`는 normalize가 `("rafikx", legacy_self=true)`로 해석 — 완전 하위호환 (기존 사용자 동작 불변).
- decorate_system은 엔진 prompt_block **뒤**에 append (엔진 지시 < 학습된 지시 우선).
- effective_iter_cap, maybe_observe, flush_observations 게이트도 동일 조건.
- `/engine` 표시에 `self 메타: on|off` 라인 추가. `/engine self`는 기존대로 동작(legacy), `/selfharness on|off` 슬래시 신설로 meta 토글 + config 영속화.
- surfaces에 `plan_instruction` (5번째 면) 추가: 기본값 "계획을 세울 때 완료 기준을 검증 가능한 형태로 명시한다." — decorate는 계획 호출(§2)에도 이 면만 주입. EDITABLE_SURFACES에 등록해 자기개선 대상에 포함. (기존 self_harness.json v2와의 호환: serde default로 필드 추가만 하면 됨. version은 그대로 유지.)

## 4. 전문가 역할 프로파일 + 산출물 계약

**신규 내장 프로파일 4종** — 코드 내장 프리셋(`engine.rs` 또는 `config.rs`의 `builtin_profile(name)`)으로 제공하고, **bind가 config에서 못 찾으면 내장 프리셋으로 폴백** (기존 사용자 config에 [subagents.planner]가 없어도 동작). config에 같은 이름이 있으면 사용자 정의가 이긴다.

| 프로파일 | 도구 | plan | verify | system_extra 요지 |
|---|---|---|---|---|
| `planner` | read_file, list_dir, grep, obsidian_search, todo_write | false | false | 20년 경력 PM/아키텍트. 요구사항을 해석하고 스펙·DoD·작업 분해를 산출한다. 코드를 작성하지 않는다. 출력은 [해석]/[완료 기준]/[작업 분해] 구조를 지킨다 |
| `frontend` | * | true | true | 20년 경력 프론트엔드 전문가. 접근성·반응형·상태 관리·성능(불필요 렌더 방지)을 기본 품질로 삼는다. 기존 코드 스타일을 따른다 |
| `backend` | * | true | true | 20년 경력 백엔드 전문가. 입력 검증·오류 처리·트랜잭션 경계·보안(주입·권한)을 기본 품질로 삼는다. 기존 코드 스타일을 따른다 |
| `reviewer` | read_file, list_dir, grep, bash | false | false | 20년 경력 수석 리뷰어. 신선한 시각으로 산출물을 DoD와 대조하고 결함(정확성>보안>성능)을 찾는다. 칭찬 없이 사실만. 출력은 [판정] pass|fail + [미충족 항목] + [결함] 목록 |

- task 도구의 role 인자가 이 프로파일들을 받을 수 있도록 resolve 경로 확인 (tools/task.rs — 프로파일 이름으로 bind되는지, 아니면 class만 받는지 확인 후 프로파일 지정 경로 추가).
- claude/kimi 엔진 prompt_block에 "여러 분야(프론트+백엔드)에 걸친 대형 작업은 task 도구로 planner→frontend/backend 순 위임하고, 단계 간에는 구조화 산출물(스펙→diff 요약)만 주고받는다" 지침 포함 (MetaGPT 산출물 계약).

## 5. 독립 검증자 게이트 (VerifyPolicy::Strict)

run_verify 성공 **후** (또는 검증 생략 시 그 자리에서), spec.verify_policy==Strict && class∈{Dev,Advanced}이면:

1. reviewer 프로파일 + `[harness] manual_verify` 모델(현재 fable-5 배정)로 **신선한 컨텍스트** 1회 호출 (agent 루프 아님, read-only 도구 포함 미니 루프 max 6 iter — task.rs 재활용 가능하면 재활용).
2. 입력: 원 task + DoD 체크리스트(§2, 없으면 원 task만) + 계획의 [반박] + **변경 파일 목록만**. diff는 첨부하지 않는다 — 리뷰어가 read_file·grep 으로 직접 읽고 필요하면 bash 로 빌드·테스트를 돌린다(구현된 정책). 첨부된 요약만 보고 판정하면 "신선한 시각"이 성립하지 않기 때문이다.
3. 출력 파싱: `[판정] pass` → 완료. `[판정] fail` → [미충족 항목]+[결함]을 사용자 메시지로 만들어 **agent 루프 1회 재개** (기존 verify 재시도 패턴과 동일한 resume 방식). 재개 후 재검증은 1회만 — 2번째 fail이면 상태 보고 후 종료 (무한루프 방지). outcome.error에 "검증자 미통과: <요약>" 기록.
4. 게이트 자체 실패(모델 오류 등)는 경고 1줄 후 통과 취급 (게이트가 가용성을 해치면 안 됨).
5. graph 노드: `critic` kind로 기록.
6. config `[harness] strict_gate = true`(기본)가 게이트를 **활성**한다 — `false` 로 끈다.

근거: K2 검증자 선택 +5.8%p, graph engineering의 fresh-context 리뷰어 노드, 자기평가 편향 방지.

## 6. discipline 축 — harness | loop | graph

`GeneralConfig`에 `discipline: String` (serde default → "harness"). `/discipline` 슬래시 명령 (인자 없으면 현재+목록 표시, 값 주면 전환+영속화). 팔레트 등록.

- **harness** (기본): §1~5 파이프라인 그대로.
- **loop**: 루프 엔지니어링 강화 —
  - max_continuations +4 (spec 값에 가산, 상한 12)
  - 정체(stale) 1회 감지 시 즉시 전략 전환 메시지 주입: "직전 사이클에서 진전이 없었다. 현재 todo를 더 작은 단위로 쪼개거나 다른 도구/경로로 전환하라. 같은 접근의 반복을 금지한다" (Ralph 루프 + self_harness loop_break_instruction 계열)
  - 종료 조건 명시 프롬프트: "모든 todo 완료 + 검증 통과 전에는 완료를 선언하지 마라"
- **graph**: 그래프 엔지니어링 (PEV 상태 그래프, 순차) —
  - 계획 호출이 DAG JSON을 산출: `{"nodes":[{"id","goal","deps":[],"produces":"산출물 한 줄"}]}` (3~7 노드). 파싱 실패 시 경고 1줄 + harness discipline 폴백.
  - 위상순으로 노드마다 **독립 agent 루프** 실행 (신선한 messages, max_iterations는 binding값의 절반, 최소 8). 노드 프롬프트 = 원 task + 이 노드 goal + 선행 노드들의 산출물 요약 (각 500자 상한 — 서브에이전트 격리 원칙: 결론만 전달).
  - 노드 산출물 요약: 노드 종료 시 마지막 assistant 텍스트에서 추출 (전체가 아니라 앞 500자) + 변경 파일 목록.
  - 노드 실패(status != ok) 시 같은 노드 1회 재시도, 재실패면 중단하고 완료 노드/실패 노드 보고.
  - graph_events에 각 노드를 `graph_node` kind로 기록 (기존 TUI 표시 재활용).
  - verify(§5 포함)는 전체 그래프 종료 후 1회.
  - 사이클 감지: deps에 순환이 있으면 경고 + harness 폴백.

## 7. UI/명령 정리

- `/engine` — 목록에 6종 + 각 summary 표시. 선택 UI 기존 유지 (엔진 저장 + provider mode 질문).
- `/discipline [harness|loop|graph]` — 신설.
- `/selfharness [on|off]` — meta 토글 신설.
- `rafikx status` / doctor 출력에 engine·discipline·self-meta 표시 추가 (기존 표시 지점에 필드 추가 수준).
- 실행 시작 1줄 표시 확장: `[하네스] dev → coder (minimax:MiniMax-M3) · engine=claude · graph` 형식.

## 8. 하위호환·마이그레이션

- 기존 config 그대로 동작: engine 미설정→rafikx, discipline 미설정→harness, self_harness.meta 미설정→false, engine="self"→legacy 경로 (동작 동일).
- `dk` 값: normalize가 deepseek으로 매핑 (api.rs:218 반쪽 호환 제거, 한 곳으로 통일).
- self_harness.json: plan_instruction 필드 serde default 추가 — 기존 v2 파일 그대로 로드됨.
- 기본 config 템플릿(DEFAULT_CONFIG)에 신규 키 주석 포함 예시 추가 (engines 오버라이드, discipline, self_harness.meta, strict_gate).

## 9. 테스트 계약 (구현자가 반드시 추가)

1. `engine::normalize` — dk→deepseek, self→(rafikx,true), 빈 값→rafikx, 대소문자.
2. `engine::resolve` + config 오버라이드 병합 (prompt_block 교체·plan_depth 오버라이드).
3. Contract 계획 프롬프트에 lessons/system_extra가 포함되는지 (plan 컨텍스트 결함 회귀 방지).
4. bind 폴백: config에 없는 `planner` 조회 시 내장 프리셋 반환, config에 있으면 사용자 정의 우선.
5. 독립 검증자 게이트: fail 파싱 → 재개 1회 → 2회 fail 시 종료 (모델 호출은 mock 불가하므로 파싱·상태 전이 함수를 순수 함수로 분리해 단위 테스트).
6. DAG 파싱: 정상/순환/형식 오류 → 폴백.
7. discipline=loop의 continuation 한도 가산.
8. 기존 147+ 테스트 전부 통과 유지.

## 10. 구현 순서 (Phase)

- **Phase 1**: engine.rs + 분기 교체 + 계획 강화(Contract·컨텍스트 수정) + normalize/dk + /engine 갱신 + config 스키마(EngineOverride, discipline 필드만 선언) + 테스트 1,2,3
- **Phase 2**: 전문가 프로파일 프리셋 + bind 폴백 + task role 경로 + 독립 검증자 게이트 + /selfharness + self-harness meta 승격 + plan_instruction surface + 테스트 4,5
- **Phase 3**: discipline 구현(loop·graph) + /discipline + status/doctor 표시 + DEFAULT_CONFIG 갱신 + 테스트 6,7 + 문서(README 한 단락)
- **Phase 4**: minimax 엔진 + pin_provider(bind·fallback_order·검증자 게이트 배선) + EngineOverride.pin_provider + 테스트(§11.4)
- **Phase 5**: 진행 가시성(§12) — 계획 스트리밍 + StreamEvent 확장 + 스피너 경과 시간 + 단계 전환 라이브 라인 + paperthin 패턴 흡수(§14)
- **Phase 6**: 팀 모드(§13) — team = single|multi + /team + 프로파일별 모델 + task 병렬 실행
- **Phase 7**: 하드닝(§15) — 게이트 판정 견고화 + pin 교차 완화 + Contract→todo 결합 + 수락 통계 강화 + graph 노드 경계 검증 + 문서 정합

각 Phase 종료 조건: `cargo test` 전체 통과 + `cargo build --release` 성공.

## 11. minimax 엔진 — 단일 프로바이더 고정 하네스 (Phase 4)

목표: **MiniMax 모델만으로** 높은 품질을 내는 전용 엔진. 근거: Self-Harness(arXiv 2606.09498) 실증에서 MiniMax 계열이 하네스 개선 이득이 가장 큼(M2.5 기준 상대 +53%) — 모델 교체 없이 하네스가 품질을 견인한다.

### 11.1 EngineSpec 확장 — pin_provider

```rust
pub struct EngineSpec {
    // ... 기존 필드 ...
    /// Some이면 실행 경로(계획·에이전트 루프·verify·검증자 게이트)의 프로바이더를
    /// 이 값으로 고정한다. 자동 선택(manual_*, sticky, ranks, fallback)을 전부 이긴다.
    /// CLI --provider 명시 오버라이드만 예외(사용자 직접 의지) — 이때 경고 1줄.
    pub pin_provider: Option<Cow<'static, str>>,
}
```

- `EngineOverride`에도 `pin_provider: Option<String>` 추가 (빈 문자열 = 고정 해제).
- 일반 메커니즘이다: config `[engines.*]`로 어떤 엔진에든 다른 프로바이더를 고정할 수 있다.

### 11.2 배선 지점

1. **bind**: pin이 있고 명시 `--provider` 오버라이드가 없으면 binding의 provider를 pin으로 강제, 모델은 그 프로바이더의 `model_role` 규칙(main/small)으로 해석. `[harness] strategy=manual`의 manual_* 배정·sticky보다 pin이 우선. 명시 오버라이드가 pin과 다르면 오버라이드 존중 + "엔진 {name}은 {pin} 고정이지만 --provider 지정을 따릅니다" 경고 1줄.
2. **fallback_order**: pin이면 해당 프로바이더 하나로 제한 (계정 다중 순회는 유지 — 리밋 시 같은 프로바이더의 다른 계정으로만).
3. **독립 검증자 게이트**: pin이면 `manual_verify`를 무시하고 pin 프로바이더의 main 모델로 리뷰. (신선한 컨텍스트가 본질이므로 같은 모델이어도 유효 — 논문도 동일 모델 제안/실행.)
4. **범위 밖(그대로 유지)**: lessons reflection·LLM classifier·self-harness proposer 같은 백그라운드 보조 호출은 기존 로직 유지 — 실행 경로만 고정한다.

### 11.3 minimax 엔진 스펙

| 필드 | 값 | 근거 |
|---|---|---|
| plan_depth | Contract | DoD가 통합 수준 결함(예: 지면 충돌 누락)을 검증 항목으로 끌어냄 |
| verify_policy | Strict | 독립 검증자(신선한 컨텍스트의 MiniMax)가 DoD 대조 |
| force_staged | false | Contract의 todo 시드가 이미 담당 |
| max_continuations | 10 | MiniMax는 반복 여유가 유효 |
| pin_provider | "minimax" | |

prompt_block (한국어, 관찰된 실제 약점의 정면 보정):
- 도구 호출 시 필수 인자(path 등)를 절대 누락하지 마라 — 호출 직전에 인자 완전성을 한 번 확인한다.
- 큰 파일은 한 번에 통째로 쓰지 말고 처음부터 골격→분할 추가로 나눠 작성한다.
- 구현 후 문법 검사에서 멈추지 말고, 핵심 상호작용·엣지케이스를 실행 관점에서 자체 시뮬레이션 점검한다 (플레이/호출 흐름을 따라가며 상태 변화를 검증).
- 같은 실패를 같은 방식으로 반복하지 마라 — 2회 실패하면 접근을 바꾼다.

summary: "MiniMax 전용 — 프로바이더 고정 + 약점 보정 + 계약형 계획·검증"

### 11.4 테스트 계약 (Phase 4 — 구현 완료)

1. 카탈로그에 minimax 포함(7종), pin_provider="minimax", is_selectable 통과.
   - `is_selectable(name)` = `/engine <name>` 입력으로 받아들일지 판정한다: `resolve(name).is_some() || name == "self"`(대소문자 무시). legacy `self` 를 유효 입력으로 남기되 normalize 가 `(rafikx, legacy_self=true)` 로 해석한다.
2. pin 우선순위: 자동 선택·manual_*를 이기고, 명시 오버라이드에는 진다 (순수 함수로 분리해 검증).
3. fallback_order 제한: pin이면 결과가 pin 프로바이더 하나.
4. EngineOverride.pin_provider 병합 (설정·해제).
5. 기존 전체 테스트 통과 유지.

## 12. 진행 가시성 (Phase 5) — "모델이 뭘 하는지 항상 한 줄은 보인다"

실측 근거(2026-08-26 21:25 run): 계획 40초 비스트리밍 침묵, MiniMax-M3의 대형 tool call 인자 생성 88~124초 구간(reasoning도 텍스트도 없음)이 완전 침묵 — 사용자는 "모델이 일하지 않는다"고 느꼈다.

1. **계획 스트리밍**: plan 호출을 `chat_with_fallback` → `stream_with_fallback`로 전환, 시작 시 `[계획 수립 중 · {model}]` 라이브 라인, 텍스트는 기존 `[모델 작업]` 래핑으로 흘린다.
2. **스트림 이벤트 확장**: provider `chat_stream`의 `on_text: FnMut(&str)`를 `FnMut(StreamEvent)`로 확장 — `Text(&str)` | `ToolArgs { name: &str, total_bytes: usize }`. openai_compat이 tool_calls delta를 누적할 때 8KB 단위로 ToolArgs를 발행. 소비자(agent.rs 등)는 Text→기존 출력, ToolArgs→스피너 라벨 "도구 호출 작성 중: {name} · {KB}KB". 호출부 영향이 크면 어댑터(기존 &str 클로저 래핑)로 침습 최소화.
3. **스피너 경과 시간**: 모델 호출 대기 중 스피너 라벨에 경과 시간 자동 표기 ("반복 3/25 · 모델 호출 · 1m24s"). spinner 틱에서 시작 시각 기준 계산.
4. **단계 전환 라이브 라인 표준화**: verify 시작·critic 시작(모델·회차)·goal_continue(사유)·graph 노드 시작이 각각 한 줄씩 화면에 남는지 점검하고 빠진 곳을 보강. CLI(비 TUI) 경로도 동일.

## 13. 팀 모드 (Phase 6) — single | multi 에이전트 선택

`[harness] team = "single"`(기본) | `"multi"` + `/team` 슬래시 + status 표시.

- **single**: 현행 그대로 — 한 바인딩 모델이 전 과정 수행.
- **multi** (dev|advanced 클래스): 시스템 프롬프트에 팀 지침 주입 — "계획 확정 후 [작업 분해]에서 독립적인 단계들을 task 도구(role=planner|frontend|backend|reviewer)로 위임하라. 독립적인 갈래는 한 턴에 task 를 여러 개 함께 호출하면 병렬 실행된다."
- **프로파일별 모델**: `SubAgentConfig.model: Option<String>` ("provider:model" 또는 모델 ID) — 역할마다 다른 모델 배정. pin 활성 시 provider 부분은 pin이 이기고 모델 ID만 존중하되, pin 프로바이더에 없는 모델이면 경고 후 model_role 기본으로. 이로써 pin=minimax + team=multi = "MiniMax 안에서 M2(경량 역할)/M3(구현·리뷰) 분업", pin 없음 + team=multi = 기존 ranks/manual 역할 배정과 프로파일 모델의 조합.
- **task 병렬 실행**: agent.rs 도구 실행 루프에서 같은 응답의 연속 task 호출들을 묶어 동시 실행(join_all). 조건: 승인 불필요 상태(allow_all)일 때만, 아니면 기존 순차. 자식 라이브 출력에는 `[팀:{role}]` 프리픽스로 구분. task 자식은 이미 독립 RunContext·graph 기록을 가진다.

## 14. paperthin 패턴 흡수 (Phase 5에 포함)

LilMGenius/paperthin(MIT)의 저수준 에이전트 패턴 중 실측 실패와 맞물리는 것만 하네스에 반사신경으로 내장:

1. **re0 (패치 대신 재작성)**: 공통 시스템 프롬프트 [엔지니어링] 절에 추가 — "같은 파일에서 edit_file/apply_patch 가 2회 연속 실패하면 부분 패치를 멈추고 그 파일을 읽어 전체를 write_file 로 재작성하라." (2026-08-26 21:33 edit_file old_str 불일치 3연속 실측 대응)
2. **hate (계획을 죽일 유일한 이의)**: Contract 계획 산출물에 `[반박]` 절 추가 — "이 계획을 실패시킬 가장 유력한 위험 1개와 그것을 조기 확인할 최소 테스트 1개." DoD와 함께 검증자 게이트 입력에 포함.
3. **mandela/shower (자체 확인 편향 감시)**: reviewer 프리셋 system_extra 에 추가 — "직접 읽고 실행해 얻은 외부 근거 없이 pass 를 주지 마라. 작성 맥락을 모르는 낯선 눈으로 본다."
4. 도입하지 않는 것: 나머지 스킬들은 대화형 슬래시 워크플로라 RafikX 하네스 자동 반사에 부적합 — 필요 시 사용자가 paperthin 을 Claude Code 쪽에서 직접 쓴다.

## 15. 하드닝 (Phase 7) — paperthin 점검(2026-08-26) 발견 반영

mandela·hate·prism·shower 렌즈 점검의 확인된 발견만 반영한다. 공통 원칙: **"신호 없음 = 통과" 금지 — 판정 불능은 통과와 구분해 기록한다.**

### 15.1 검증자 게이트 판정 견고화 [high×2]
- `parse_review_verdict`: 이어붙인 전체가 아니라 **마지막 assistant 메시지**에서, `[판정]`이 여러 개면 **마지막 것**을 채택 (1회차 발화의 "[판정] …하겠다" 오탐 제거).
- 판정 줄 부재 또는 리뷰어 미니루프 status≠ok → Pass가 아니라 **Indeterminate**: "[판정] pass 또는 fail 한 줄로만 결론을 내라" 재질의 1회 → 그래도 불능이면 통과 처리하되 `outcome.error`가 아닌 별도 표기로 "검증자 판정 불능"을 라이브 라인+graph(critic indeterminate)로 남긴다 (가용성 유지 + 관측 가능, "안 돈 것"과 "통과" 구분).
- `REVIEW_GATE_MAX_ITER` 6→8.

### 15.2 pin 게이트 교차 완화 [med×2 + 장애 대응]
- 리뷰어 system에 **엔진 prompt_block을 함께 주입** (약점 보정 없이 판정만 시키는 모순 제거).
- `review_prompt`에 1줄: "완료 기준은 실행 모델이 세운 것이다 — 원 작업 요구와 어긋나면 원 작업이 우선한다."
- pin 런타임 전면 장애: 주 실행에서 pin 프로바이더가 재시도 소진 후에도 실패하면 **이번 실행에 한해** 경고 1줄과 함께 `[harness] fallback` 순서로 폴백 (pin은 선호이지 가용성 희생이 아니다). `[engines.*]`에 `pin_strict = true` 오버라이드로 폴백 금지 선택 가능.

### 15.3 Contract→todo 사슬 결합 [hate root]
- Contract 활성 시 첫 사용자 메시지의 착수 지시에 **[작업 분해] 본문을 그대로 복사**해 넣는다 (system 안 [실행 계획]을 가리키는 원거리 참조 제거).
- Contract 활성 시 staged 블록의 "2~6개" 문구를 생략 (지시 충돌 제거 — 시드 지시가 단계 수를 지배).
- 관측: goal 첫 판정 시 계획 단계 수 N과 todo total이 다르면 경고 라이브 라인 1줄 (강제 없음).

### 15.4 Self-Harness 수락 통계 강화 [high×3]
- 재발 판정을 signature 완전일치 → **cause(결정적 추출) 일치**로 완화. causal|mechanism은 표시·클러스터용으로 유지.
- baseline을 **버전 경계와 무관하게** 최근 window개로 계산. `baseline_n < 5`면 판정 보류(트라이얼 연장).
- `trial_min_episodes` 기본 3→5. (엄밀한 이항검정은 도입하지 않는다 — 문서에 한계 명시: 이 게이트는 통계 검정이 아니라 보수적 휴리스틱이다.)
- verify 재시도로 최종 통과한 실행: `AgentOutcome.verify_fail`은 **최종 실패만** 담는다 (재시도 성공 시 None). 단 "실패 후 회복" lesson 수집 경로는 유지 (소비자 확인 후 최소 수정).

### 15.5 graph 노드 경계 검증 [prism fail×2]
- 노드 종료마다 `auto_verify_command` 1회 (명령이 있을 때만). 실패 시 해당 노드 재시도 프롬프트에 검증 출력 포함.
- 노드 재시도에 **첫 시도의 변경 파일 목록** 포함: "첫 시도가 아래 파일을 이미 바꿨다 — 현재 상태를 읽고 이어가라." (resume 없는 재시도의 이중 편집 방지)
- 노드 system의 범위 제한 문구에 "선행 노드가 바꾼 파일은 읽고 그 위에서 작업하라" 추가.
- graph 계획에서 DoD가 추출되지 않으면 경고 라이브 라인 1줄 (조용한 약화 방지).

### 15.6 문서 정합 (shower)
- §5.6 문구 교정: "strict_gate = true(기본)가 게이트 활성 — false로 끈다".
- §1 타입을 Cow 반영, §11.4의 is_selectable 정의 추가, §5.2 diff 정책을 구현(목록만 전달, 리뷰어가 직접 읽음)으로 갱신, §10 Phase 목록에 4~7 추가.

### 15.7 점검이 확인한 "누수 없음" (기록)
- 리뷰어 컨텍스트 격리(본 대화·lessons 미상속, 도구로 외부 근거 접근)는 건전.
- 게이트 오류=통과(가용성 우선)는 15.1의 관측 표기와 결합하면 수용 가능.

## 16. TUI 라이브 배선 수리 + working 패널 (Phase 8)

근본 원인(정찰 확정): TUI 턴 실행 시 `chat.rs:1540-1547`이 run sink를 no-op으로 설치하고, `ui::emit_in`의 단락 평가(`run.emit_live(e) || emit(e)`)가 no-op에서 true를 돌려 전역 TUI sink에 아무 라이브 이벤트도 도달하지 않는다. 수신 측(`apply_live`, `render_streaming_tail`)은 완비 — 송신 배선만 끊김.

### 16.1 라이브 배선 수리
- observer 유무와 무관하게 `let live_sink = crate::ui::current_live_sink();` — no-op 분기 제거. 이것만으로 스트리밍 텍스트·[도구] 로그·todo 패널·[팀:role] 라인·서브에이전트 진행(Live::Agent)이 TUI에 살아난다.
- 자식 lifecycle이 부모 스테이지 스트립을 덮어쓰는 버그(`tui.rs:946-949`, run_id 무필터) 수정: 루트 run의 state만 반영.

### 16.2 working 패널 (chunks[5] — 비어 있는 status 슬롯)
- 목적: **에이전트별 한 줄** — `working  {역할}  {provider/model}  {지금 하는 일}` + 마지막에 `mode` 줄 1개. 팔레트와 푸터 사이(최하단 푸터 위).
- 데이터: 기존 `Live::Agent`(현재 task.rs만 발신)를 확장 — 필드에 model(provider/model 문자열)·activity·done 추가(하위호환 serde default). 발신 지점:
  (a) 메인 에이전트: 턴 시작 직후(바인딩 확정 지점)에 1줄 시작 발신, agent.rs 반복 시작마다 activity="반복 n/m", StreamEvent::ToolArgs 소비 지점에서 activity="도구 호출 작성 중: {name} · {KB}KB", 도구 실행 시 activity="[도구] {name}", 턴 종료 시 done.
  (b) task 자식: 기존 발신 지점(task.rs 시작/실패/종료)에 role·model 채워 확장 — done이면 줄 제거.
  (c) 검증자 게이트: 시작 시 role="reviewer", activity="완료 기준 대조 · {n}회차", 종료 시 done.
- mode 줄: `mode  engine={name}{(고정)} · team={mode} · discipline={d} · self v{N}|off · gate on|off` — 턴 시작 시 조립해 App에 저장(새 Live variant 또는 Agent 확장으로 전달), 턴 진행 동안 상시 표시.
- 렌더: `render_working_rows(&[WorkerLine], mode_line, width) -> Vec<Line>` 순수 함수. "working" 리터럴은 영문 소문자, 강조색. 행 수 = workers+1, 상한 6(초과 시 오래된 worker 생략), 좁은 높이에서 0으로 접힘 — `responsive_rows`에 wanted_status 인자 추가, 기존 `narrow_layout_never_allocates_more_rows_than_available` 계약 유지.
- App 상태: `workers: Vec<WorkerLine>{id, role, model, activity}` — apply_live에서 upsert/remove.

### 16.3 테스트 (기존 관행: 순수 함수 단위)
- `render_working_rows` 행 조립(working 접두·역할·모델·활동, 한국어 폭), 높이 함수, responsive_rows 확장 회귀, Live::Agent 직렬화 하위호환(구 필드만 있는 JSON 역직렬화), 루트 run 필터.

## 17. /model 원격 조회·선택 (Phase 9)

현황(조사 완료): `auth::list_remote_models(cfg, name)`가 프로바이더 API(`/v1/models`)에서 모델 목록을 가져오고, `auth::save_catalog`가 `~/.rafikx/catalogs.json`에 캐시하며, `auth::catalog_models`(→`registered_models`)가 config 등록분 + 선호 목록 + **캐시**를 합쳐 돌려준다. TUI `/model`은 이미 검색 가능한 피커(`PickerKind::Model`)로 그 목록을 고르고 선택을 영속화한다.
**빠진 것은 단 하나 — 카탈로그를 사용자가 원할 때 갱신하는 경로.** 지금은 서비스 연결 직후 자동 조회 때만 채워져, 그 뒤 프로바이더가 새 모델을 내도 목록에 영영 안 뜬다.

### 17.1 수동 조회 — `/model refresh`
- `auth.rs`: `pub async fn refresh_catalogs(cfg: &Config) -> Vec<CatalogRefresh>` 신설. `usable_names(cfg)`의 모든 연결을 **동시에**(futures_util::future::join_all — 기존 의존성) 조회하고, 성공분만 `save_catalog`. 반환: `CatalogRefresh { provider: String, result: Result<usize, String> }` (개수 또는 오류 메시지 요약).
- 요약 문자열 조립은 **순수 함수** `pub fn refresh_summary(&[CatalogRefresh]) -> Vec<String>`로 분리(테스트 대상): 프로바이더별 `minimax  12개` / `anthropic  실패: HTTP 401` 한 줄씩 + 총계 한 줄.
- `handle_slash`는 동기이므로 새 variant `Slash::ModelFetch { query: String }`를 반환한다 (기존 `Slash::AssignRoles`가 쓰는 비동기 위임 패턴 그대로). 트리거: `/model refresh|fetch|새로고침|-r` (`/models`도 동일 별칭).

### 17.2 소비자
- **TUI**(`tui.rs`): `start_model_fetch(app, done_tx)` — `start_assign`(1150행대)을 본떠 busy 설정·`tokio::spawn`·완료 시 `session.cfg.reload()`. 완료 후 **모델 피커를 자동으로 열고**, `Slash::ModelFetch.query`가 비어 있지 않으면 `picker.query`에 미리 채운다.
- **CLI 대화 루프**(`chat.rs` 219행대): `.await`로 직접 실행하고 요약을 출력.
- **api.rs**(RPC/데스크탑): `SlashResult`에 `model_fetch: bool` 추가 — `assign` 필드와 같은 취급.

### 17.3 선택 UX 보강
- `/model <검색어>`(숫자가 아닌 인자): TUI는 피커를 열고 `query`를 그 값으로 채운다(타이핑 검색과 동일 경로). 숫자 인자는 기존 `apply_model_choice` 유지.
- 피커 제목에 총 개수 표시(`모델 (N개)`), 항목 라벨은 기존 `{provider} / {id}` 유지.
- 등록분과 조회분을 구분할 필요는 없다 — `registered_models`가 이미 합쳐 주고, 선택 시 config 등록분이면 영속 저장, 아니면 "세션 한정"으로 기존 코드가 갈라 처리한다.

### 17.4 테스트
`refresh_summary` 포맷(성공·실패 혼재·빈 목록), `/model refresh` 별칭이 `Slash::ModelFetch`를 반환하고 `/model 3`은 기존 선택 경로를 타는 라우팅, `/model <검색어>`의 query 전달, 기존 전체 테스트 통과.
