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

pub struct EngineSpec {
    pub name: &'static str,
    pub summary: &'static str,        // /engine 목록 한 줄 설명 (한국어)
    pub prompt_block: &'static str,   // 시스템 프롬프트 증강 블록 (한국어)
    pub force_staged: bool,           // 모든 도구 작업에 todo 단계 강제
    pub plan_depth: PlanDepth,        // dev/advanced 클래스에 적용
    pub verify_policy: VerifyPolicy,
    pub max_continuations: u8,        // goal continuation 한도 (기존 8)
}

pub fn catalog() -> &'static [EngineSpec];      // 내장 6종
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

- `catalog()`는 config `[engines.<name>]` 테이블로 **필드 단위 오버라이드** 가능해야 한다 (문구·플래그를 코드 수정 없이 튜닝). 오버라이드 구조체: `EngineOverride { prompt_block, force_staged, plan_depth, verify_policy, max_continuations }` 전 필드 Option.
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
2. 입력: 원 task + DoD 체크리스트(§2, 없으면 원 task만) + 변경 파일 목록 + 각 변경 파일 diff 요약(전문 아님 — 크기 상한 8KB).
3. 출력 파싱: `[판정] pass` → 완료. `[판정] fail` → [미충족 항목]+[결함]을 사용자 메시지로 만들어 **agent 루프 1회 재개** (기존 verify 재시도 패턴과 동일한 resume 방식). 재개 후 재검증은 1회만 — 2번째 fail이면 상태 보고 후 종료 (무한루프 방지). outcome.error에 "검증자 미통과: <요약>" 기록.
4. 게이트 자체 실패(모델 오류 등)는 경고 1줄 후 통과 취급 (게이트가 가용성을 해치면 안 됨).
5. graph 노드: `critic` kind로 기록.
6. config `[harness] strict_gate = true`(기본 true)로 전역 차단 가능.

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

각 Phase 종료 조건: `cargo test` 전체 통과 + `cargo build --release` 성공.
