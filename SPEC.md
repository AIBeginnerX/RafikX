🛠 RafikX v3.0 — AI 구현 지시서 (SPEC.md)
목적: Rust 기반 초경량 개인용 AI 코딩 에이전트 CLI (Claude Code류) v3 핵심: ① 작업 난이도별 자동 Harness(서브에이전트) 시스템 ② 자기학습 메모리(같은 실수 반복 방지) ③ Inspector 자가 점검 에이전트(오류 로그 분석 → 개선 리포트) ④ 모바일 원격 접속 필수화(텔레그램) 사용법: 이 파일을 프로젝트 루트에 SPEC.md로 저장 → AI(Claude Code / Cursor)에게 "SPEC.md의 Phase N을 구현해라"라고 지시

0. 사람(운영자)이 지켜야 할 운영 규칙 — 먼저 읽기
Phase 순서대로만 진행. 한 AI 세션에 한 Phase(또는 그 절반)만.
Phase 완료 시마다 git 커밋: git add -A && git commit -m "phase-N 완료"
문제 발생 시 롤백: git checkout . 또는 git reset --hard <커밋ID>
PROGRESS.md 유지 (3줄: 완료: ... / 다음: ... / 이슈: ...)
AI 세션 시작 멘트 (복사해서 사용):

SPEC.md와 PROGRESS.md를 먼저 읽어라.

지금은 Phase N을 구현한다. SPEC.md 5장 '인터페이스 계약'과 6장 '안전장치'는 수정 금지.

새 크레이트 추가는 금지다. 꼭 필요하면 사유와 대안을 먼저 보고하고 내 승인을 받아라.

파일 1~2개 작성할 때마다 cargo check로 확인하고,

마지막에 빌드 방법·검증 커맨드·예상 결과·git 커밋 메시지를 알려줘라.

설명은 전부 한국어로 해라.

1. 변경 이력
v1 → v2 (요약): 에이전트 루프·도구 7종·승인 게이트 신설(코딩 에이전트의 심장), 안전장치(경로 jail·bash 차단목록·비용 가드), 프로바이더를 Anthropic 네이티브 + OpenAI 호환 2종으로 압축, FTS5 스키마 결함(중복 삽입·특수문자 오류) 수정, serde_yaml 배제, 한글 검색(trigram 옵션), 성능 목표 현실화, "AI와 첫 대화"를 Phase 1로 이동.

v2 → v3 (이번 개정):

| # | 추가/변경 | 내용 |
| --- | --- | --- |
| 1 | 4단계 작업 분류 Harness | 사용자 지시를 simple(단순) / medium(중급) / advanced(고급) / dev(개발)로 자동 분류. 개발 작업은 전용 dev Harness가 계획→실행→검증 루프로 처리 |
| 2 | 서브에이전트 = 실행 프로파일 | 별도 프로세스가 아니라 "모델 + 도구 목록 + 반복 상한 + 역할 프롬프트" 묶음. 같은 Agent Loop 코드가 프로파일만 바꿔 실행 → 초경량 유지의 핵심 |
| 3 | 등록 모델 능력 자동 적응 | 사용자가 config에 등록한 모델들의 능력(도구 지원 여부 등)을 검사해 서브에이전트를 자동 바인딩. 능력 부족 시 자동 대체 또는 정중한 거부 |
| 4 | 자기학습 메모리 | runs(실행 이력)·lessons(교훈) 테이블 + 자동 리플렉션. 실패·거부·검증 실패에서 교훈을 추출해 다음 실행의 시스템 프롬프트에 주입 → 같은 실수 반복 방지 |
| 5 | Inspector 점검 에이전트 | 별도 등록 가능한 자가 점검 에이전트. 실행 이력·오류 로그를 주기 분석 → 개선 리포트를 터미널/텔레그램으로 사용자에게 전달. 읽기 전용, 제안만(자동 수정 금지) |
| 6 | 모바일 원격 접속 필수화 | 텔레그램을 선택 기능에서 코어(기본 feature)로 승격. /ask·/status·/report 원격 명령 + 원격 도구 승인 버튼 + Inspector 리포트 자동 푸시 |
| 7 | 동시성 안전 | 데몬(텔레그램)과 CLI가 같은 DB를 동시 사용 → SQLite WAL 모드 + busy_timeout 의무화 |

2. 제품 정의와 성능 목표
한 줄 정의: 터미널이나 폰(텔레그램)에서 일을 시키면, 도구가 작업 난이도를 스스로 분류해 알맞은 Harness(서브에이전트)로 실행하고, 파일을 읽고·수정하고·명령을 실행하되 매번 승인을 받으며, 실수에서 배운 교훈을 기억해 반복하지 않고, 점검 에이전트가 주기적으로 자기 상태를 진단해 개선안을 보고하는 개인용 초경량 CLI 에이전트.

범위 제외: GUI/IDE, 벡터 DB·임베딩, 멀티유저, 자율 코드 자기수정(Inspector는 제안만).

성능 목표 (릴리즈 빌드 기준):

| 항목 | 합격선 | 도전 목표 |
| --- | --- | --- |
| 바이너리 (기본 빌드 = 텔레그램 포함) | < 20MB | < 12MB |
| 바이너리 (코어만, --no-default-features) | < 15MB | < 8MB |
| RAM — 단발 ask | < 30MB | < 15MB |
| RAM — 데몬 상주 (telegram + inspector 스케줄) | < 60MB | < 30MB |
| Cold start | < 150ms | < 30ms |
| FTS5 검색 (노트 1,000개) | < 50ms | < 10ms |
| 교훈 주입 오버헤드 (lessons 검색+조립) | < 10ms | < 3ms |

⚠️ 합격선만 넘으면 다음 Phase로 진행. 최적화는 Phase 8의 일이다.

3. 아키텍처 (v3)
┌────────────────────── Interfaces ──────────────────────┐

│  CLI: (TTY 기본 TUI) ask / agent / chat / index /     │

│       search / watch / lessons / inspect / report /    │

│       doctor / settings / ranks / telegram             │

│  📱 Telegram(필수): /ask /obsidian /status /report      │

│       + 원격 승인 버튼(✅/❌)          [tui 기본]        │

└──────────────────────────┬─────────────────────────────┘

                           │

┌──────────────────────────▼─────────────────────────────┐

│              Harness Orchestrator (v3 신규)             │

│  ① 분류기: simple|medium|advanced|dev (규칙, 옵션 LLM)  │

│  ② 능력 검사: 등록 모델의 도구지원 여부 → 프로파일 바인딩 │

│  ③ 서브에이전트 프로파일 선택                            │

│     quick(단순) worker(중급) thinker(고급) coder(개발)  │

│  ④ [교훈 주입] lessons 상위 K개를 시스템 프롬프트에 첨부  │

└──────────────────────────┬─────────────────────────────┘

                           │

┌──────────────────────────▼─────────────────────────────┐

│                    Agent Core (공용)                    │

│  Agent Loop: LLM → tool_use? → [승인 게이트] → 실행 →   │

│              tool_result 재투입 → 반복 (상한)           │

│  dev Harness: 계획 → 실행 → 검증(빌드/테스트) → 실패 시   │

│              오류 되먹임 재시도(최대 2회)                │

│  Tools: read/edit/write/list/grep/bash/obsidian_search │

│  종료 후: runs 기록 → (실패·거부 시) 리플렉션 → lesson   │

└───────┬──────────────────┬──────────────────┬──────────┘

        │                  │                  │

┌───────▼────────┐ ┌───────▼─────────┐ ┌──────▼─────────────┐

│ Providers (2종)│ │ Obsidian Engine │ │ Inspector (별도)    │

│ Anthropic 네이티브│ │ scan→parse→FTS5 │ │ runs·로그 통계 분석 │

│ OpenAI 호환 범용 │ │ notify watcher  │ │ → 개선 리포트 생성  │

│ (Ollama·Gemini· │ └─────────────────┘ │ → 터미널+텔레그램   │

│  OpenRouter·GLM)│                     │ 읽기 전용·제안만    │

└────────────────┘                      └────────────────────┘

     공용 저장소: ~/.rafikx/

     config.toml + data.db(WAL: 인덱스·세션·runs·lessons·reports)

     + logs/agent.log + reports/*.md

4. 기술 스택 (확정 크레이트 목록)
원칙: 이 표에 없는 크레이트는 AI가 임의로 추가할 수 없다. 추가 필요 시 사유·크기·대안을 보고하고 운영자 승인을 받는다. v3의 신규 기능(Harness·메모리·Inspector)은 새 크레이트 없이 기존 스택 + std로 구현한다.

| 용도 | 크레이트 | feature / 비고 |
| --- | --- | --- |
| 비동기 런타임 | tokio | ["rt-multi-thread","macros","time","process","fs","io-util","sync","signal"] |
| HTTP 클라이언트 | reqwest | default-features=false, ["json","stream","rustls-tls"] — openssl 금지 |
| 스트림 유틸 | futures-util |  |
| CLI 파싱 | clap | ["derive"] |
| 직렬화 | serde, serde_json | serde ["derive"] |
| 설정 | toml |  |
| DB | rusqlite | ["bundled"] — WAL·busy_timeout 필수(5.8장) |
| 에러 | anyhow, thiserror |  |
| 경로 | dirs |  |
| 정규식 | regex | 분류기·링크/태그 추출·grep |
| 파일 순회 | ignore |  |
| diff 표시 | similar | edit/write 승인 시 diff 출력 |
| 마크다운 | pulldown-cmark | Phase 4 |
| 파일 감시 | notify, notify-debouncer-mini | Phase 4 |
| REPL 입력 | rustyline | Phase 5, 선택 (미사용: TUI 입력은 crossterm) |
| async trait | async-trait | 선택 — enum 디스패치 시 불필요 |
| [feature: telegram — 기본 포함] | teloxide | Phase 7. default = ["telegram"]. 초경량 코어만 원하면 --no-default-features 빌드 |
| [feature: tui — 기본 포함] | ratatui, crossterm | TTY에서 `rafikx` 무인자 대화 화면. Windows cmd/WT 포함 |

금지: LangChain류, 벡터 DB, 임베딩, ORM/sqlx, native-tls, 정밀 토크나이저, uuid/rand(ID는 타임스탬프+시퀀스로), cron 크레이트(스케줄은 tokio interval).

릴리즈 프로필: opt-level="z", lto=true, codegen-units=1, panic="abort", strip="symbols"

5. 인터페이스 계약 ❄️ FROZEN
이 장은 모든 Phase에서 수정 금지. 변경 필요 시 AI는 구현을 멈추고 사유를 보고한다.
(2026-08-21 제품 승인: TTY에서 인자 없는 `rafikx` 가 대화 TUI를 연다. 비TTY는 사용법을 출력한다.)
5.1 CLI 명령 표면
rafikx                         # TTY: 대화 TUI. 파이프/비TTY: 사용법

rafikx ask "지시"              # 통합 진입점: 분류기 → 서브에이전트 자동 실행

rafikx ask --class dev "지시"  # 분류 강제 (simple|medium|advanced|dev)

rafikx ask --obsidian "질문"   # Vault 컨텍스트 강제 주입

rafikx agent "작업 지시"       # = ask --class dev 별칭 (개발 Harness 강제)

rafikx chat                    # 대화 TUI (Harness 규칙 동일). --list 세션 목록, --resume <id>

rafikx index / search "키워드" / watch     # Obsidian

rafikx lessons list            # 교훈 관리

rafikx lessons add "교훈 문장"

rafikx lessons rm <id> | clear

rafikx inspect [--last N] [--apply]   # 즉시 점검 (--apply: 제안 교훈 일괄 승인 저장)

rafikx report last             # 마지막 점검 리포트 다시 보기

rafikx doctor                  # 자가진단 (키·경로·FTS5·프로바이더·프로파일 바인딩)

rafikx login                   # 서비스 연결 (OpenCode Zen/Go, Anthropic, …)

rafikx telegram [--with-watch] # 📱 원격 데몬: 봇 + Inspector 스케줄 (+옵션 watch)

공통 옵션: --provider <이름> --model <ID> --class <분류> --yes --config <경로>

OpenCode Zen: base `https://opencode.ai/zen/v1`, Bearer, 환경변수 `OPENCODE_API_KEY` (별칭 `OPENCODE_ZEN_API_KEY`). 기본 모델 `glm-5.1` (chat/completions). 키는 opencode.ai/auth.
OpenCode Go: base `https://opencode.ai/zen/go/v1`, Bearer, `OPENCODE_GO_API_KEY` 또는 `OPENCODE_API_KEY`. 기본 모델 `kimi-k2.7-code`.

5.2 config.toml (전체 예시 — 최초 실행 시 ~/.rafikx/config.toml 자동 생성)
[general]

default_provider = "anthropic"

workspace = "~/dev/playground"     # 파일/bash 도구 접근 루트 (이 밖은 차단)

max_tokens = 8192

max_context_chars = 200000

approval = "ask"                   # ask | auto-safe | yolo

classifier = "rules"               # rules | llm (llm: small 모델이 한 단어로 분류, 실패 시 rules 폴백)

[providers.anthropic]

kind = "anthropic"

api_key_env = "ANTHROPIC_API_KEY"  # 환경변수 '이름'만 기록 (키 원문 저장 금지)

model = "claude-sonnet-4-6"        # 예시 — 시점에 맞는 모델 ID로 교체

small_model = "claude-haiku-4-5"

supports_tools = true

[providers.local]

kind = "openai_compat"

base_url = "http://localhost:11434/v1"   # Ollama

model = "qwen3:8b"                       # 설치한 로컬 모델명으로 교체

api_key_env = ""

supports_tools = false                   # 도구 미지원 → Harness가 자동으로 도구 작업에서 제외

# OpenRouter / Z.ai / Gemini(OpenAI 호환 엔드포인트)도 같은 형식으로 추가

[harness]                          # 분류 → 서브에이전트 매핑

simple   = "quick"

medium   = "worker"

advanced = "thinker"

dev      = "coder"

fallback = ["anthropic", "local"]  # 프로바이더 장애 시 재시도 순서

[subagents.quick]                  # 단순 작업: 인사·단답·짧은 변환

provider = "local"

model_role = "small"               # main | small (프로바이더의 model / small_model)

tools = []

max_iterations = 3

plan_first = false

verify = false

system_extra = "짧고 정확하게 답한다. 불필요한 설명을 붙이지 않는다."

[subagents.worker]                 # 중급 작업: 요약·정리·검색·문서 초안

provider = "anthropic"

model_role = "small"

tools = ["read_file","list_dir","grep","obsidian_search"]

max_iterations = 10

plan_first = false

verify = false

system_extra = "자료를 찾아 정확히 정리하는 실무 보조자다. 출처(파일 경로)를 밝힌다."

[subagents.thinker]                # 고급 작업: 설계·분석·전략·보고서

provider = "anthropic"

model_role = "main"

tools = ["read_file","list_dir","grep","obsidian_search","write_file"]

max_iterations = 15

plan_first = true                  # 실행 전 계획(3~7항목)을 먼저 출력

verify = false

system_extra = "복잡한 문제를 구조화하는 분석가다. 결론 전에 근거를 제시한다."

[subagents.coder]                  # 개발 작업 전용 Harness

provider = "anthropic"

model_role = "main"

tools = ["*"]                      # 전체 도구

max_iterations = 25

plan_first = true

verify = true                      # 작업 후 빌드/테스트 검증 단계 실행

verify_command = ""                # 빈값 = 자동 감지 (5.5장). 프로젝트별 지정 가능

system_extra = "신중한 시니어 개발자다. 수정 전 반드시 원문을 읽고, 최소 diff로 고친다."

[memory]                           # 자기학습

enabled = true

max_lessons = 500                  # 초과 시 가중치 낮고 오래된 것부터 삭제

inject_limit_chars = 2000          # 프롬프트에 주입하는 교훈 총량 상한

[inspector]                        # 자가 점검 에이전트

subagent = "thinker"               # 분석에 사용할 프로파일 (도구는 강제 제거되어 읽기 전용)

auto_interval_hours = 24           # 데몬에서 주기 실행. 0 = 수동(inspect)만

notify_telegram = true             # 리포트 요약을 텔레그램으로 푸시

[obsidian]

vault_path = "~/Documents/TestVault"   # 개발 중엔 반드시 '사본' Vault

db_path = "~/.rafikx/data.db"

context_limit_chars = 12000

tokenizer = "unicode61"            # unicode61 | trigram(한글 부분일치)

[telegram]                         # 📱 모바일 원격 (설계상 필수 기능)

enabled = true

token_env = "TELEGRAM_BOT_TOKEN"

allowed_user_ids = [123456789]     # 본인 user id — 이 외 계정은 완전 무응답

allow_agent = false                # 원격 도구 실행 허용 여부 (켜면 승인 버튼 필수)

approval_timeout_secs = 300        # 승인 무응답 시 자동 거부
5.3 Provider 계약 (내부 공용 메시지 모델)
pub enum Role { System, User, Assistant }

pub enum ContentBlock {

    Text       { text: String },

    ToolUse    { id: String, name: String, input: serde_json::Value },

    ToolResult { tool_use_id: String, content: String, is_error: bool },

}

pub struct Message { pub role: Role, pub content: Vec<ContentBlock> }

pub struct ChatRequest {

    pub model: String,

    pub system: String,

    pub messages: Vec<Message>,

    pub tools: Vec<ToolSpec>,          // 비어 있으면 도구 미사용

    pub max_tokens: u32,

    pub stream: bool,

}

pub struct ChatResponse {

    pub content: Vec<ContentBlock>,

    pub stop_reason: StopReason,       // EndTurn | ToolUse | MaxTokens | Other

    pub input_tokens: u32,

    pub output_tokens: u32,

}

pub struct ToolSpec { pub name: String, pub description: String, pub input_schema: serde_json::Value }

구현체는 정확히 2종: AnthropicProvider(Messages API·네이티브 tool use·SSE), OpenAiCompatProvider(chat/completions·function calling 매핑·data: [DONE] 종료). 디스패치는 enum match 또는 async-trait 중 택1 후 유지.
5.4 Tool 계약 + 기본 도구 7종
pub trait Tool {

    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn input_schema(&self) -> serde_json::Value;   // JSON Schema, required 명시

    fn needs_approval(&self, input: &serde_json::Value) -> bool;

    fn run(&self, input: serde_json::Value, ctx: &ToolCtx) -> anyhow::Result<String>;

}

pub struct ToolCtx { pub workspace: PathBuf, /* config·db 핸들 등 */ }

| 도구 | 입력 | 승인 | 규칙 |
| --- | --- | --- | --- |
| read_file | path, offset?, limit? | 불필요 | workspace 밖 금지. 256KB 초과는 범위 지정 필수 |
| list_dir | path | 불필요 | 최대 500항목 |
| grep | pattern, path?, glob? | 불필요 | ignore+regex in-process, 매치 상한 200줄 |
| edit_file | path, old_str, new_str | 필요 | old_str 파일 내 유일 필수(아니면 에러를 tool_result로). 적용 전 diff |
| write_file | path, content | 필요 | 신규/전체 덮어쓰기. 덮어쓰기 시 diff |
| bash | command | 항상 필요 | cwd=workspace, 타임아웃 60초, 차단목록(6장), 출력 20KB 절단 |
| obsidian_search | query, limit=5 | 불필요 | FTS5 검색, 제목+경로+발췌 (Phase 4 등록) |

도구 오류는 패닉이 아니라 ToolResult{is_error:true}로 모델에 반환. 서브에이전트 프로파일의 tools 목록에 없는 도구는 해당 실행에서 아예 모델에 노출하지 않는다.
5.5 Harness 실행 파이프라인 (v3 핵심 계약)
서브에이전트 프로파일 구조체:

pub struct SubAgentProfile {

    pub name: String,

    pub provider: String,        // providers.* 키

    pub model_role: ModelRole,   // Main | Small

    pub tools: Vec<String>,      // "*" = 전체

    pub max_iterations: u32,     // 전역 하드캡 50을 넘을 수 없음

    pub plan_first: bool,

    pub verify: bool,

    pub verify_command: String,  // 빈값 = 자동 감지

    pub system_extra: String,

}

분류기 규칙 v1 (위에서부터 평가, 먼저 매칭되면 확정. --class로 항상 강제 가능):

dev — 코드블록(```) 포함, 또는 파일 확장자 언급(.rs .py .js .ts .toml .json 등), 또는 키워드: 코드·구현·수정해·고쳐·버그·디버그·컴파일·빌드·리팩터·테스트 작성·스크립트·함수·에러 잡아
advanced — 길이 > 600자, 또는 목록형 다단계 지시(줄바꿈 항목 3개 이상), 또는 키워드: 설계·아키텍처·분석·전략·비교 평가·보고서·계획 수립·검토
medium — --obsidian 지정, 또는 길이 150~600자, 또는 키워드: 요약·정리·번역·초안·검색·찾아·노트·문서
simple — 그 외 전부 (인사·단답·짧은 변환)

classifier = "llm"이면: small 모델에 "다음 지시를 simple/medium/advanced/dev 중 한 단어로만 분류하라" 호출(max_tokens=8). 실패·모호 시 규칙 폴백. 분류 결과는 항상 실행 전에 1줄 표시: [Harness] dev → coder (claude-sonnet-4-6)

능력 검사(Capability Binding) — "등록한 모델에 맞는 Harness" 보장:

시작 시 config의 모든 프로바이더에서 능력표 구성: supports_tools, 스트리밍 가능 여부.
프로파일이 도구를 요구하는데(tools 비어있지 않음) 바인딩된 프로바이더가 supports_tools=false면 → [harness].fallback 순서에서 도구 지원 프로바이더로 자동 재바인딩 + 경고 1줄.
도구 지원 프로바이더가 하나도 없으면 → dev/advanced 작업을 정중히 거부: "도구를 지원하는 모델이 등록되어 있지 않습니다. config에 supports_tools=true 프로바이더를 추가하세요." (simple/medium은 도구 없이 계속 동작)
doctor는 분류→프로파일→실제 바인딩 모델을 표로 출력한다.

실행 순서 (모든 ask/agent/chat 턴 공통):

입력 → 분류 → 프로파일 선택 + 능력검사

→ 시스템 프롬프트 조립: 기본 + system_extra + [과거 교훈 블록](5.6)

→ (plan_first) 계획 3~7항목 생성·출력 (도구 없는 1회 호출)

→ Agent Loop 실행 (프로파일의 tools/max_iterations 적용, 승인 게이트 5.4/6장)

→ (verify) 검증 단계 — dev Harness 전용:

     verify_command 실행(bash 도구 경유 → 기존 승인·차단·타임아웃 그대로 적용)

     빈값이면 자동 감지: Cargo.toml→`cargo check` / pyproject.toml 또는 .py 변경→`python3 -m py_compile <변경파일>` / 그 외→검증 생략 안내

     실패 시 오류 전문을 tool_result로 되먹여 재시도 (최대 2회, 그래도 실패면 상황 보고 후 종료)

→ runs 기록(5.8) → 실패·거부·검증실패 있었으면 리플렉션 → lesson 저장(5.6, 비동기)

→ 결과 출력 + 사용 프로파일/모델/토큰 표시

Agent Loop 공통 규칙: 반복 상한 도달 시 "상한 도달, 여기까지 결과"로 종료 / 동일 (도구, 입력) 3회 반복 시 강제 중단 / tool_result는 대응 tool_use.id를 담아 직후 user 메시지의 첫 content로(Anthropic 규격, 위반 시 400) / 히스토리가 max_context_chars 초과 시 system 제외 오래된 것부터 제거하되 tool_use·tool_result 쌍으로 제거 / agent(도구) 실행은 비스트리밍(승인 프롬프트 충돌 방지).
5.6 자기학습 메모리 계약 — "같은 실수를 반복하지 않는다"
자동 수집 트리거 (5종):

도구 실행 오류 (ToolResult{is_error:true})
dev 검증(verify) 실패 → 재시도로 성공한 경우, 그 원인
사용자의 승인 거부(n) — 거부 직후 "사유(선택, Enter로 생략): " 1줄 입력 기회
run 실패 종료 (반복 상한·API 에러·타임아웃)
수동 등록: lessons add "..." 또는 텔레그램 /lesson ...

리플렉션 호출 (교훈 추출): small 모델에 고정 프롬프트로 요청, 메인 흐름을 절대 막지 않는다(tokio spawn, 실패 시 조용히 스킵):

system: 너는 실수 기록가다. 아래 오류와 맥락에서 다음에 지킬 교훈을 딱 1개,

        JSON {"keywords":"공백구분 키워드 3~6개","lesson":"명령형 1~2문장"} 형식으로만 출력하라.

user:   [작업 요약 500자] + [오류/사유 500자]

중복 방지: 저장 전 lessons_fts에서 유사 교훈 검색 → 있으면 새로 넣지 않고 weight+1, last_hit 갱신.

주입 규칙: 매 실행 시 시스템 프롬프트에 아래 블록 첨부. 작업 키워드 FTS 상위 5개 + weight 상위 고정 2개(중복 제거), 총량 inject_limit_chars(기본 2,000자) 상한:

[과거 교훈 — 같은 실수를 반복하지 말 것]

- (w3) edit_file 전에 read_file로 원문을 반드시 확인한다

- (w2) cargo check 실패 시 에러의 첫 번째 항목부터 고친다

정리: max_lessons(500) 초과 시 weight 낮고 last_hit 오래된 것부터 삭제. lessons list/add/rm/clear CLI 제공.
5.7 Inspector 계약 — 자가 디버깅·점검·개선 에이전트
정체: 파일쓰기·bash 도구가 강제로 제거된(읽기 전용) 서브에이전트. [inspector].subagent로 어떤 프로파일을 쓸지 지정하며, 다른 프로파일을 등록해 교체·확장 가능(예: [subagents.myguard] 추가 → inspect --subagent myguard).

데이터 소스: ① runs 최근 N건(기본 200) ② lessons 전체 통계 ③ ~/.rafikx/logs/agent.log 끝부분(에러 라인) ④ doctor 결과.

파이프라인:

수집 → 코드로 1차 통계 계산(모델 호출 전):

  성공률, 분류별 건수, 도구별 실패율, 프로바이더별 429/timeout 횟수,

  평균 반복수, 총 토큰(→ 대략 비용), 최다 오류 메시지 Top5

→ 통계+오류 샘플을 [inspector].subagent 모델에 전달(고정 분석 프롬프트)

→ 마크다운 리포트 생성:

  { 기간 요약 / 건강 신호등(🟢🟡🔴) / 반복 실패 패턴 Top5 /

    제안 교훈(lesson 후보) 목록 / 제안 설정 변경(진단·사유만, 자동 적용 금지) /

    사용자 액션 아이템 }

→ 저장: reports 테이블 + ~/.rafikx/reports/YYYYMMDD-HHMM.md

→ 전달: 터미널 출력 + (notify_telegram=true && 데몬 가동 중) 텔레그램 요약 푸시

→ `inspect --apply`: 리포트의 '제안 교훈'을 일괄 승인해 lessons에 저장

실행 방식: 수동 inspect / 데몬(telegram 명령) 내 auto_interval_hours 간격 자동 실행.

안전 불변식(하드코딩): Inspector는 어떤 경우에도 ① 파일을 쓰지 않고 ② 명령을 실행하지 않으며 ③ config·코드를 수정하지 않는다. 오직 리포트와 제안만 만든다. 반영은 항상 사용자 승인(--apply 또는 수동 수정)을 거친다.
5.8 DB 스키마 (SQLite 단일 파일 ~/.rafikx/data.db)
연결 시 필수 PRAGMA (데몬+CLI 동시 사용 대비):

PRAGMA journal_mode = WAL;

PRAGMA busy_timeout = 5000;

-- Obsidian (v2와 동일)

CREATE TABLE IF NOT EXISTS notes (

  path TEXT PRIMARY KEY, title TEXT NOT NULL,

  tags TEXT NOT NULL DEFAULT '', links TEXT NOT NULL DEFAULT '', mtime INTEGER NOT NULL

);

CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(

  title, content, tags, path UNINDEXED,

  tokenize = 'unicode61 remove_diacritics 2'

);

-- 세션 (Phase 5)

CREATE TABLE IF NOT EXISTS sessions (

  id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,

  title TEXT, messages_json TEXT NOT NULL

);

-- 실행 이력 (v3: 자기학습·Inspector의 원천 데이터)

CREATE TABLE IF NOT EXISTS runs (

  id TEXT PRIMARY KEY,              -- "<unix_millis>-<seq>" (외부 crate 금지)

  started_at INTEGER NOT NULL, finished_at INTEGER,

  mode TEXT NOT NULL,               -- ask|agent|chat|telegram|inspect

  class TEXT,                       -- simple|medium|advanced|dev

  subagent TEXT, provider TEXT, model TEXT,

  task TEXT,                        -- 지시문 앞 500자

  iterations INTEGER DEFAULT 0,

  input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,

  status TEXT NOT NULL,             -- ok|fail|denied|limit

  error TEXT                        -- 오류 요약 500자

);

-- 교훈 (v3: 자기학습 메모리)

CREATE TABLE IF NOT EXISTS lessons (

  id INTEGER PRIMARY KEY AUTOINCREMENT,

  created_at INTEGER NOT NULL, last_hit INTEGER NOT NULL,

  trigger TEXT NOT NULL,            -- tool_error|verify_fail|user_deny|run_fail|manual

  keywords TEXT NOT NULL, lesson TEXT NOT NULL,

  weight INTEGER NOT NULL DEFAULT 1

);

CREATE VIRTUAL TABLE IF NOT EXISTS lessons_fts USING fts5(

  keywords, lesson, lesson_id UNINDEXED

);

-- 점검 리포트 (v3)

CREATE TABLE IF NOT EXISTS reports (

  id TEXT PRIMARY KEY, created_at INTEGER NOT NULL,

  summary TEXT NOT NULL,            -- 텔레그램 푸시용 10줄 요약

  body_path TEXT NOT NULL           -- reports/*.md 경로

);

갱신 규칙: notes/notes_fts는 DELETE→INSERT upsert, mtime 동일 시 스킵. lessons_fts는 lessons와 함께 삽입·삭제.
5.9 FTS 쿼리 이스케이프 (notes·lessons 공용 — 사용자 입력을 MATCH에 직접 넣지 말 것)
fn build_fts_query(user: &str) -> String {

    user.split_whitespace()

        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))

        .collect::<Vec<_>>()

        .join(" ")   // 공백 = 암묵적 AND

}

6. 안전장치 (필수 구현 — 어떤 Phase에서도 생략·완화 금지)
경로 jail: 모든 파일 도구는 canonicalize 후 workspace 하위인지 검증. 심볼릭 링크 탈출 차단. 위반 시 도구가 에러 반환.
bash 제한: 승인 필수 + 타임아웃 60초 + 차단목록 — sudo, rm -rf /(및 ~ 대상), mkfs, dd, shutdown, reboot, chmod -R 777, curl … | sh, > /dev/, fork bomb 패턴. Ctrl+C 시 자식 프로세스 kill.
비용 가드: max_tokens·프로파일별 max_iterations(전역 하드캡 50) 준수. 응답마다 usage 표시. 리플렉션·분류기는 small 모델만 사용. (사람 몫: 프로바이더 콘솔에서 월 지출 한도 설정 — Phase 0)
데이터 보호: agent(도구) 실행 시 workspace가 git 저장소가 아니면 "git init 권장" 경고 1회. Obsidian Vault는 파일 도구 workspace에 포함 금지(검색은 읽기 전용 DB 경유만).
시크릿 보호: API 키·봇 토큰을 로그/에러/tool_result/리포트에 절대 미포함. doctor는 마지막 4자리만 표시. config 파일 권한 600 권장.
자기수정 금지 불변식: Inspector·리플렉션·lessons 시스템은 코드와 config를 절대 자동 수정하지 않는다. 제안 → 사용자 승인 흐름만 존재한다. (lessons 데이터 저장만 예외적으로 허용 — 코드가 아니라 데이터이므로)
원격(텔레그램) 안전 규칙: allowed_user_ids 외 계정은 완전 무응답(에러 메시지도 금지) / allow_agent=false 기본 / 원격 도구 실행은 반드시 인라인 승인 버튼 경유, approval_timeout_secs 무응답 시 자동 거부 / 원격에서 yolo(--yes) 모드는 코드 수준에서 항상 금지 / 봇 토큰은 환경변수로만.
교훈 주입 상한: lessons 주입은 inject_limit_chars를 절대 초과하지 않는다(프롬프트 비대 = 비용·품질 저하).

7. Master Prompt (AI 구현자에게 세션마다 함께 제공)
[역할]

당신은 Rust 시스템 프로그래밍과 CLI 에이전트 설계에 능숙한 시니어 엔지니어다.

사용자는 비개발자다. 모든 설명은 한국어로 하고, 실행할 명령은 복사 가능한 코드블록으로 준다.

[목표]

SPEC.md에 따라 초경량 Rust CLI 에이전트 RafikX(`rafikx`)를 Phase 순서대로 구현한다.

v3의 핵심은 ① 작업 분류 Harness(서브에이전트 프로파일) ② 자기학습 메모리(lessons)

③ Inspector 점검 에이전트 ④ 텔레그램 모바일 원격이다.

[절대 규칙]

1. SPEC.md 5장(인터페이스 계약)과 6장(안전장치)은 수정 금지. 변경 필요 시 구현을 멈추고 보고한다.

2. SPEC.md 4장 표에 없는 크레이트 추가 금지. 필요 시 후보·크기·대안을 보고하고 승인을 기다린다.

3. 한 번에 한 Phase만. Phase 안에서도 파일 1~2개 단위로 작성하고 cargo check를 실행한다.

4. 이전 Phase 코드는 버그 수정 외 리팩터링 금지.

5. 각 Phase 종료 시: 빌드 방법, 검증 커맨드, 예상 출력, git 커밋 메시지를 제시한다.

6. 파괴적 작업은 계획을 먼저 보여주고 승인을 받는다.

7. 확신 없는 외부 API 사양은 추측으로 코딩하지 말고 공식 문서 확인 필요를 명시한다.

8. 성능 최적화보다 '동작하는 단순한 코드' 우선. 최적화는 Phase 8에서만.

9. Inspector·리플렉션·lessons가 코드나 config를 자동 수정하는 경로를 절대 만들지 않는다.

   서브에이전트는 프로세스가 아니라 '실행 프로파일'로만 구현한다(경량 유지의 핵심).

8. 단계별 구현 계획 (Phase 0 ~ 8)
🧰 Phase 0 — 사전 준비 (사람이 직접, 45~90분)
AI가 아니라 운영자 본인이 하는 단계. v3에서는 텔레그램 준비가 필수 항목으로 승격됐다.

macOS 개발도구: xcode-select --install
Rust 설치: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh → 새 터미널에서 rustc --version
git 확인: git --version / (권장) VS Code + rust-analyzer
Anthropic API 키: console.anthropic.com → API Keys.
⚠️ Claude Max 구독과 API 크레딧은 별개 결제. 소액($5~10) 충전 + Settings→Limits에서 월 지출 한도 필수 설정
키 등록: ~/.zshrc에 export ANTHROPIC_API_KEY="sk-ant-..." → source ~/.zshrc
📱 텔레그램 (필수): ① @BotFather → /newbot → 봇 토큰 발급 ② @userinfobot → 내 user id 확인 ③ ~/.zshrc에 export TELEGRAM_BOT_TOKEN="..." ④ 새 봇과 1:1 대화방 열어두기(먼저 /start 전송)
(권장) Ollama: brew install ollama → ollama serve → 소형 모델 1개 pull — simple 작업·무료 폴백용
(선택) OpenRouter / Z.ai / Gemini 키 등록
Obsidian 테스트용 사본 Vault 생성 (원본 훼손 방지 — 개발 중 사본만 사용)
작업 폴더: mkdir -p ~/dev && cd ~/dev

검증: rustc --version / echo $ANTHROPIC_API_KEY | head -c 12 / echo $TELEGRAM_BOT_TOKEN | head -c 10 모두 정상 출력.

🟢 Phase 1 — 뼈대 + 설정 + 첫 대화 (난이도 ★ / AI 세션 1회)
목표: rafikx ask "안녕" 이 Claude 응답을 스트리밍 출력.

cargo new agent-harness && cd agent-harness (git 초기화 확인; 소스 폴더는 agent-harness, 패키지/바이너리 이름은 rafikx)
Cargo.toml: 릴리즈 프로필 + 최소 크레이트(tokio, reqwest, futures-util, clap, serde, serde_json, toml, dirs, anyhow) + [features] default=[](telegram feature는 Phase 7에서 추가)
src/config.rs: 5.2 로드/기본 생성, api_key_env 해석, ~ 확장
로그 초기화: ~/.rafikx/logs/agent.log에 std::fs append로 1줄 JSON 기록 유틸(레벨·시각·메시지) — 크레이트 추가 없이
src/provider/anthropic.rs: 비스트리밍 먼저 → 성공 후 SSE 스트리밍
src/main.rs: clap 서브커맨드 ask, doctor
doctor v1: 설정 경로 / 키 존재(마지막 4자) / workspace 존재

검증: rafikx doctor 전 항목 OK → rafikx ask "안녕하세요. 한 문장으로" 스트리밍 응답 → cargo build --release && ls -lh target/release/rafikx 크기 기록.

자주 터지는 문제: linker 'cc' not found→xcode-select / openssl 에러→rustls-tls 확인 / 401→새 터미널에서 키 재확인 / 스트리밍 안 끝남→Anthropic은 event: message_stop으로 종료(OpenAI data: [DONE]과 다름).

🔴 Phase 2 — 에이전트 루프 + 도구 + 승인 (난이도 ★★★ / 핵심. AI 세션 2~3회 분할)
목표: rafikx agent "hello.txt 만들고 '안녕' 써줘" → 승인 → 파일 생성. 여기까지가 '미니 Claude Code' MVP.

2a: Tool trait + ToolCtx + 경로 jail / read_file·list_dir / tools 전달·tool_use 파싱(비스트리밍) / Agent Loop(5.5 공통 규칙 전부) / runs 기록 v1(id·시각·mode·task·status) 2b: edit_file(old_str 유일성 + similar diff)·write_file / 승인 게이트 [y/n/a] + --yes(경고) 2c: bash(tokio::process·타임아웃·차단목록·20KB 절단) / grep / git 저장소 경고

검증:

rafikx agent "이 폴더 파일 목록 보고 Cargo.toml 요약해줘" → 읽기 도구 자동 실행
rafikx agent "hello.txt 만들고 '안녕하세요' 써줘" → diff → y → cat hello.txt
edit_file 승인 흐름 / /etc/hosts 읽기 지시 → jail 거부 / sudo 지시 → 차단 / n 거부 시 무변경
sqlite3 ~/.rafikx/data.db "select mode,status from runs" → 기록 확인

문제 예방: 400 최다 원인 = tool_result 규격 위반(5.5) / input_schema required 누락 → 빈 인자 / 무한루프 → 상한 동작 테스트 / agent 모드는 비스트리밍 유지.

🟠 Phase 3 — 프로바이더 + Harness 오케스트레이터 (난이도 ★★★ / AI 세션 2회)
목표: 4단계 자동 분류 → 서브에이전트 프로파일 실행. 등록 모델 능력에 자동 적응. dev Harness의 검증 루프 동작.

OpenAiCompatProvider (function calling 매핑, data: [DONE])
provider registry + --provider / 폴백(타임아웃 120초·429·5xx → fallback 순서, 백오프 1s→2s→4s 최대 3회)
분류기 v1 (5.5 규칙) + --class 강제 + classifier="llm" 옵션(small 모델, 실패 시 규칙 폴백) + 실행 전 [Harness] dev → coder (...) 1줄 표시
SubAgentProfile 로딩 ([subagents.*]) + 프로파일별 tools 필터·max_iterations·system_extra 적용
능력 검사: supports_tools 기반 자동 재바인딩 + 경고 / 전무 시 dev·advanced 정중 거부
dev 검증 루프: verify_command(빈값=자동 감지) → bash 도구 경유 실행 → 실패 시 오류 되먹임 재시도(최대 2회)
plan_first: 계획 3~7항목 선출력
runs 확장(class·subagent·provider·model·iterations·tokens) / doctor 확장(프로바이더 ping + 분류→프로파일→모델 바인딩 표)

검증:

rafikx ask "안녕" → [Harness] simple → quick / rafikx ask "이 저장소 구조 분석해서 개선 전략 보고서 써줘" → advanced
rafikx ask "buggy.py 만들어서 일부러 문법 오류 넣고, 고친 뒤 검증까지 해줘" → dev 분류 → 계획 → 실행 → py_compile 검증 → 재시도 성공
rafikx ask --class simple "..." 강제 동작
config에서 anthropic을 잠시 지우고 local(도구 미지원)만 남김 → dev 지시 → 정중 거부 메시지 → 원복
키 이름 잠시 변경 → 폴백 프로바이더 응답 → 원복 / doctor 바인딩 표 확인

함정: Ollama connection refused→ollama serve / 프로바이더별 tool calling 방언으로 고생하면 초기엔 supports_tools를 Anthropic만 true로 두고 진행 / Gemini·OpenRouter·Z.ai는 base_url 교체로만 대응(개별 구현 금지) / 분류기를 과도하게 똑똑하게 만들려 하지 말 것(규칙 v1로 충분, 오분류는 --class로 교정).

🟡 Phase 4 — Obsidian 엔진 (난이도 ★★ / AI 세션 2회)
목표: 사본 Vault 인덱싱 + 검색 + ask --obsidian + obsidian_search 도구 + watch.

rusqlite 추가, 5.8 스키마 + WAL·busy_timeout PRAGMA + 시작 시 PRAGMA compile_options로 FTS5 확인(doctor 항목 추가)
스캐너: ignore 순회(.obsidian/·.trash/·숨김 제외, .md만, mtime 비교 증분)
파서: frontmatter는 ---~--- 문자열 분리 + tags: 단순 파싱(인라인 배열/리스트 2형식), 본문 #태그·[[위키링크]] regex, 본문 텍스트화 pulldown-cmark
index/search + 이스케이프 함수(5.9) 경유
obsidian_search 도구 등록(worker·thinker·coder 프로파일에서 사용 가능) + ask --obsidian(상위 N개, context_limit_chars 상한, 출처 경로 표시)
백링크 1홉 확장(최대 3개)
watch: notify-debouncer-mini(1초), upsert, 삭제 반영

검증: index "N개"→재실행 "0개 변경" / search "프로젝트" / 한글 부분일치 아쉬우면 tokenizer="trigram" 재인덱스 비교 / ask --obsidian "내 노트 기준으로 ○○ 정리" 출처 포함 / watch 중 노트 수정 → 재인덱스 1건.

함정: no such module: fts5→bundled 확인 / MATCH 문법 에러→이스케이프 미경유 색출 / 중복→upsert 누락 / 이벤트 폭주→디바운서·임시파일 필터 / 저장 중 파일 읽기 실패→0.2초 후 1회 재시도.

🔵 Phase 5 — 세션 + 자기학습 메모리 (난이도 ★★ / AI 세션 2회)
목표: chat 맥락 유지·저장·재개 + lessons 시스템 가동(같은 실수 반복 방지).

chat REPL(stdin 기본, rustyline은 승인 후): /save /quit /model /provider /class /clear /obsidian on|off /agent <지시> — 매 턴 Harness 분류 적용
sessions 저장/--resume/--list + 히스토리 상한(tool 쌍 정합성 유지) + 턴별 누적 토큰 표시
lessons 파이프라인: 트리거 5종 감지 → 리플렉션(small 모델, 고정 프롬프트, tokio spawn 비동기·실패 무시) → 중복 검사(weight+1) → 저장
주입기: 작업 키워드 FTS 상위 5 + weight 상위 2, inject_limit_chars 상한, [과거 교훈] 블록 조립
lessons list/add/rm/clear + 정리 규칙(max_lessons)

검증:

3턴 대화 → /save → --resume → 맥락 기억
일부러 실패 유발: 존재하지 않는 파일 edit 지시 → 실패 → lessons list에 교훈 1건 생성 확인
같은 실수 반복 방지 시나리오: 동일 지시 재실행 → 로그(agent.log)에서 시스템 프롬프트에 교훈 주입 확인 → 모델이 먼저 read_file/존재 확인부터 하는지 관찰
승인 n + 사유 입력 → 교훈 반영 확인 / lessons add "테스트" → 즉시 주입 후보 포함

함정: 리플렉션이 메인 흐름 블로킹 금지(반드시 spawn) / 교훈 주입 과다 = 프롬프트 비대 → 상한 준수 / tool 쌍 깨진 히스토리 저장 시 재개 400 → 저장 전 정합성 검사.

🟣 Phase 6 — Inspector 점검 에이전트 (난이도 ★★ / AI 세션 1~2회)
목표: inspect 한 번으로 자기 상태 진단 리포트 생성 → 사용자에게 개선 사항 전달.

통계 모듈(모델 호출 전 코드로 계산): 성공률·분류별 건수·도구별 실패율·프로바이더별 429/timeout·평균 반복·총 토큰·최다 오류 Top5
수집기: runs 최근 N + lessons 통계 + agent.log tail(에러 라인) + doctor 결과
분석 호출: [inspector].subagent 프로파일 사용하되 도구 목록 강제 제거(읽기 전용 불변식)
리포트 생성(5.7 형식) → reports 테이블 + ~/.rafikx/reports/…md 저장 → 터미널 출력
inspect --last N / inspect --apply(제안 교훈 일괄 lessons 저장) / report last
--subagent <이름> 옵션으로 점검 에이전트 교체 가능(확장 포인트)

검증:

Phase 1~5를 쓰며 쌓인 runs로 rafikx inspect → 리포트 생성·저장·출력
리포트에 실제 실패 패턴(예: edit_file old_str 불일치 다발)이 잡히는지
inspect --apply → lessons list에 제안 교훈 추가 / report last 재출력
Inspector가 파일을 쓰거나 명령을 실행하는 경로가 코드상 존재하지 않는지 확인(도구 강제 제거 로직 리뷰)

함정: runs가 적을 때(<10건) 리포트는 "데이터 부족" 명시(과잉 해석 금지) / 리포트에 API 키·토큰·전체 파일 내용 포함 금지 / 통계는 반드시 코드로 계산(모델에게 산수 시키지 말 것 — 환각 방지).

📱 Phase 7 — 텔레그램 모바일 원격 (필수) (난이도 ★★★ / AI 세션 2회)
목표: 폰에서 /ask·/status·/report 사용 + (옵션) 원격 도구 승인 버튼 + Inspector 자동 푸시.

[features] default=["telegram"], telegram=["dep:teloxide"] 구성 — 코어 빌드(--no-default-features)와 분리 유지
telegram 데몬: 봇 폴링 + Inspector 스케줄러(tokio interval, auto_interval_hours, 0=끔) + --with-watch 옵션(Obsidian watch 동시 구동)
화이트리스트: allowed_user_ids 외 완전 무응답 미들웨어
명령: /ask <질문>(Harness 경유) /obsidian <검색어> /status(최근 run 5건+오늘 토큰) /report(마지막 리포트 요약) /lesson <문장>(수동 교훈 등록) — 응답 4,096자 분할 전송
원격 승인 흐름(allow_agent=true일 때만): 도구 요청 → "도구: bash / 명령: …" + 인라인 버튼 ✅승인/❌거부 → callback 처리(tokio oneshot) → approval_timeout_secs 초과 시 자동 거부. 원격 yolo는 코드 수준 금지
Inspector 자동 실행 결과 → notify_telegram=true면 요약 푸시(전문은 reports/*.md 안내)

검증:

폰에서 /ask 안녕 응답 / /status / /report
다른 계정(가족 폰 등)에서 메시지 → 완전 무응답 확인
allow_agent=true 설정 후 /ask hello.txt 만들어줘 → 승인 버튼 → ✅ → 생성 / 버튼 방치 → 타임아웃 자동 거부
auto_interval_hours=0.02(약 1분, 테스트용)로 잠시 설정 → 자동 리포트 푸시 확인 → 원복
데몬 켠 채 CLI에서 ask 동시 실행 → DB 잠금 에러 없음(WAL 확인)

함정: 같은 봇 두 곳 동시 폴링 → 409 충돌(데몬 1개만) / 봇 토큰 로그 노출 금지 / teloxide로 바이너리 커지는 건 정상(코어 빌드와 분리 측정) / 원격 응답 지연 시 "작업 시작…" 선전송 후 결과 전송.

⚪ Phase 8 — 최적화·마무리 (난이도 ★ / AI 세션 1회)
크기·cold start·RSS 측정(/usr/bin/time -l target/release/rafikx ask "hi") → 2장 표와 비교, PROGRESS.md 기록 — 기본 빌드/코어 빌드 각각
cargo tree 의존성 감사 / (선택) cargo bloat --release
cargo install --path . → 전역 실행 확인
README.md(비개발자 기준 재현 가능하게: 설치→config→텔레그램 연결→일상 사용 흐름)
(선택) 크로스 빌드는 실제 필요 시에만. TUI는 기본 feature.

9. 공통 함정 목록 (모든 Phase에서 참조)
크레이트는 추가 시점 최신 안정판 고정, 이유 없는 업그레이드 금지, Cargo.lock 커밋.
panic="abort"는 release 전용 — cargo test(dev)에는 영향 없음. catch_unwind 의존 코드 금지.
rusqlite는 동기 — 짧은 질의는 그대로, 대량 인덱싱만 spawn_blocking.
데몬+CLI 동시 사용은 반드시 WAL + busy_timeout(5.8). "database is locked"가 보이면 이 둘부터 확인.
한국어 토큰은 문자수 근사로 충분 — 토크나이저 크레이트 금지.
AI의 "리팩터링하자" 제안은 기본 거절. 동작 우선, 정리는 Phase 8 이후.
에러·로그·리포트에 API 키/토큰 절대 미포함.
문서 속 모델명은 예시 — 진실은 config.toml.
SSE 종료 신호 차이: Anthropic message_stop vs OpenAI 호환 data: [DONE].
새 기능마다 doctor에 진단 항목 추가 — 비개발자의 최고 디버깅 도구는 doctor다.
리플렉션·분류(LLM 모드)는 small 모델만 — 보조 호출이 본 작업보다 비싸지면 본말전도.
lessons는 "명령형 1~2문장"만 저장 — 장문 회고록이 되면 주입 상한을 금방 초과한다.
Inspector에 파일쓰기/bash 도구를 주는 실수 금지 — 자기수정 사고의 지름길. 도구 제거를 코드로 강제할 것.
빌드가 갑자기 깨지면: cargo clean && cargo build → 안 되면 git checkout ..

10. 전체 인수 테스트 (모두 통과하면 v1.0 완성)
doctor 전 항목 OK (키·경로·FTS5·프로바이더 ping·분류→프로파일→모델 바인딩 표)
Harness 분류 4종 각 1건: simple/medium/advanced/dev가 의도한 프로파일·모델로 실행됨
dev Harness 실전 1건: "숫자 맞추기 게임 파이썬 스크립트 작성→실행→오류 수정→검증까지" 완수 (계획→실행→verify 루프 확인)
능력 적응: 도구 미지원 모델만 남기면 dev 정중 거부, simple/medium은 계속 동작
승인 n 거부 시 무변경 / 경로 jail / bash 차단목록 동작
폴백 시나리오(주 프로바이더 차단 → 자동 전환) 통과
자기학습: 의도적 실패 → lesson 자동 생성 → 동일 지시 재실행 시 교훈 주입 + 행동 개선 관찰
Inspector: inspect 리포트에 실제 실패 패턴 포착, --apply로 교훈 반영, report last 동작
모바일: 폰에서 /ask·/status·/report 동작, 미등록 계정 완전 무응답, 원격 승인 버튼·타임아웃 동작, Inspector 자동 푸시 수신
Obsidian: index/search/ask --obsidian/watch 통과
chat 저장·재개 통과
성능 합격선(2장) 충족 — 측정값 PROGRESS.md 기록

— 끝. 이 문서와 PROGRESS.md만 있으면 어떤 AI 세션에서든 이어서 작업할 수 있다.
