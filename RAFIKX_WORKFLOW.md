# RafikX 전체 워크플로

이 문서는 지금까지 설계·구현된 **RafikX**의 실행 흐름을 한곳에 모은 것입니다.  
명령 이름은 `rafikx` 입니다. 설정 폴더는 `%USERPROFILE%\.rafikx\` (예전 `.agent-harness`만 있으면 그쪽을 그대로 씁니다).

---

## 1. 한눈에 보는 전체 그림

```mermaid
flowchart TB
    U[사용자] --> CLI["rafikx 명령"]

    CLI --> FR{처음인가?<br/>클라우드 연결 없음}
    FR -->|예| WIZ[번호 메뉴 마법사]
    FR -->|아니오| CMD{어떤 명령?}

    WIZ --> SET[(설정·비밀·계정)]
    SET --> CMD

    CMD --> ASK["TUI / ask / agent / chat"]
    CMD --> ADM["settings / doctor"]
    CMD --> NOTE["index / search / watch"]
    CMD --> MEM["lessons / inspect / report"]
    CMD --> TG["telegram"]

    ASK --> PIPE[하네스 파이프라인]
    ADM --> SET
    NOTE --> VAULT[(Obsidian + FTS5)]
    MEM --> DB[(data.db)]
    TG --> PIPE
    TG --> VAULT
    TG --> MEM

    PIPE --> OUT[답변 · 파일 변경 · 하단 사용량]
```

---

## 2. 설치 후 첫 실행 · 설정

연결이 없으면 `rafikx` / `ask` / `chat` / `doctor`가 **번호 메뉴**를 엽니다. 서비스·모델 이름은 타이핑하지 않습니다. 키와 봇 토큰만 붙여넣습니다.

```mermaid
flowchart TD
    A["rafikx doctor<br/>rafikx settings<br/>rafikx ask"] --> B{클라우드 연결이 있나?}
    B -->|없음| C[환영 화면 + 서비스 번호 목록]
    B -->|있음| D[상태 점검 후 설정 메뉴]

    C --> E["번호 선택  예: 1 또는 1,2,3"]
    E --> F{인증 방식}
    F -->|OAuth<br/>Anthropic / OpenAI / Gemini| G[브라우저 로그인]
    F -->|API 키<br/>Grok / OpenRouter 등| H[키 붙여넣기]
    F -->|로컬 Ollama| I[키 없음]

    G --> J[auth.json 에 계정 저장]
    H --> K[secrets.toml 에 계정 저장]
    I --> L[accounts.json 등록]
    J --> L
    K --> L

    L --> M[모델 번호 목록]
    M --> N{"자동 하네스  또는  특정 모델"}
    N --> O[config.toml 기록]
    O --> D

    D --> P[설정 메뉴]
    P --> P1[서비스·계정 연결/추가/해제]
    P --> P2[모델]
    P --> P3[하네스 자동/수동]
    P --> P4[텔레그램]
    P --> P5[옵시디언]
    P --> P6[모델 순위 보기·갱신]
    P --> P7[하단 사용량 표시]
```

같은 서비스에 계정을 더 넣을 때: **설정 → 서비스·계정 → 같은 서비스에 계정 하나 더**.

---

## 3. 질문·작업의 중심 흐름 (`ask` / `agent` / `chat`)

`agent`는 `ask`와 같고, 분류를 강제로 `dev`로 둡니다.  
`chat`은 같은 파이프라인을 반복하며 세션을 저장합니다.

```mermaid
flowchart TD
    S[지시문] --> C1{분류}
    C1 -->|규칙 또는 LLM| C2[simple / medium / advanced / dev]
    C2 --> B1[프로파일 바인딩]

    B1 --> B2{하네스 모드}
    B2 -->|자동 기본| B3[연결한 모델 중 순위표로 고름]
    B2 -->|수동| B4[역할별 지정 모델]

    B3 --> CTX
    B4 --> CTX

    CTX[컨텍스트 조립] --> LSN[과거 교훈 주입]
    LSN --> OBS{--obsidian 또는 /obsidian on?}
    OBS -->|예| OBS2[Vault FTS5 검색 결과 첨부]
    OBS -->|아니오| PLN
    OBS2 --> PLN

    PLN{plan_first?} -->|예 advanced/dev| PLN2[계획만 먼저 출력]
    PLN -->|아니오| RUN
    PLN2 --> RUN

    RUN[에이전트 루프] --> PACK[컨텍스트 예산 팩커]
    PACK --> ACC[계정 선택 + API 호출]
    ACC --> TOOL{도구 호출?}
    TOOL -->|없음| DONE[답변]
    TOOL -->|있음| APPR{승인 필요?}
    APPR -->|로컬 y/n/a| EXEC
    APPR -->|텔레그램 버튼| EXEC
    APPR -->|거부| DENY[denied]
    EXEC[도구 실행 · 워크스페이스 감옥] --> RUN

    RUN --> VER{verify?}
    VER -->|예 coder| VER2[cargo check 등]
    VER -->|아니오| SAVE
    VER2 -->|실패 최대 2회| RUN
    VER2 -->|성공/포기| SAVE

    SAVE[runs 기록] --> FOOT[하단 사용량]
    DENY --> FOOT
    DONE --> FOOT
    FOOT --> REFL[비동기 교훈 후보]
```

### 분류 → 프로파일

| 분류 | 프로파일 | 쓰는 때 | 도구 | 계획 | 검증 |
| --- | --- | --- | --- | --- | --- |
| simple | quick | 짧은 질문 | 없음 | 아니오 | 아니오 |
| medium | worker | 요약·검색·노트 | 읽기·검색 | 아니오 | 아니오 |
| advanced | thinker | 설계·구성 | 읽기+쓰기 | 예 | 아니오 |
| dev | coder | 코딩·디버깅·검증 | 전체 | 예 | 예 |

자동 모드에서 설계·구성·검증·디버깅은 **이미 연결한 모델 안**에서 추론 순위가 높은 것을 고릅니다. 상위권이 없으면 등록분 중 가장 나은 것을 씁니다. 연결하지 않은 서비스는 호출하지 않습니다.

---

## 4. 계정 선택 · 리밋 자동 전환

```mermaid
flowchart TD
    P[프로바이더 확정] --> A[accounts.json 에서 해당 서비스 계정 목록]
    A --> R{지금 쓸 수 있는 계정?}
    R -->|있음| S[리밋 창이 먼저 끝나는 계정<br/>동률이면 오늘 사용량 적은 쪽]
    R -->|전부 리밋| W[가장 빨리 풀리는 계정]
    W --> W2{대기 20초 이하?}
    W2 -->|예| WAIT[잠깐 기다림]
    W2 -->|아니오 + 다른 계정 있음| NEXT[그 계정은 건너뜀]
    WAIT --> CALL
    S --> CALL
    NEXT --> CALL

    CALL[그 계정으로 API] --> OK{성공?}
    OK -->|예| REC[usage.json 에 토큰·남은 요청 기록]
    OK -->|429 리밋| MARK[limited_until 기록]
    MARK --> SW[다음 계정으로 전환]
    SW --> R
    OK -->|다른 오류| FB[다음 프로바이더 fallback]
    REC --> BAR[화면 하단: 사용중 / 대기 / 리밋 N분]
```

원격 텔레그램에서는 `--yes`(무조건 승인)가 **코드에서 금지**됩니다.

---

## 5. 에이전트 루프와 안전장치

```mermaid
flowchart TD
    M[메시지 + 시스템 프롬프트] --> PK[팩커]
    PK --> PK1[시스템 · 도구 스키마 · 최근 대화 유지]
    PK1 --> PK2{모델 컨텍스트 한도 초과?}
    PK2 -->|예| PK3[오래된 턴 삭제<br/>도구 쌍은 깨지 않음]
    PK2 -->|아니오| API
    PK3 --> API[모델 호출]

    API --> T{tool_use?}
    T -->|아니오| END[종료]
    T -->|예| JAIL[경로가 워크스페이스 안인가?]
    JAIL -->|밖| BLOCK[차단]
    JAIL -->|안| SAFE{위험한 bash?}
    SAFE -->|sudo / rm -rf / 등| BLOCK
    SAFE -->|승인 필요| ASK[사용자 승인]
    SAFE -->|안전| GO
    ASK -->|허용| GO[실행]
    ASK -->|거부| REASON[거부 이유 기록]
    GO --> RES[tool_result 를 대화에 추가]
    RES --> CAP{반복 상한?}
    CAP -->|미만| M
    CAP -->|도달| STOP[여기까지 결과]
    BLOCK --> RES
    REASON --> RES
```

Inspector(점검)는 **코드를 자동으로 고치지 않습니다.** 리포트와 교훈 제안만 합니다. `--apply`를 켜야 제안 교훈이 저장됩니다.

---

## 6. 텔레그램

```mermaid
flowchart TD
    BOT["rafikx telegram"] --> WL{보낸 사람 ID가<br/>allowed_user_ids 인가?}
    WL -->|아니오| SIL[아무 답도 안 함]
    WL -->|예| CMD{명령}

    CMD --> ASK["/ask 또는 일반 글"]
    CMD --> OBS["/obsidian"]
    CMD --> ST["/status  최근 5건 + 오늘 토큰"]
    CMD --> RP["/report"]
    CMD --> LS["/lesson"]

    ASK --> START[작업 시작…]
    START --> AG{allow_agent?}
    AG -->|아니오| QA[도구 없이 답만]
    AG -->|예| PIPE[하네스 파이프라인]
    PIPE --> BTN{도구 승인?}
    BTN --> YES[인라인 승인/거부]
    YES -->|시간 초과| NO[자동 거부]
    QA --> SPLIT
    PIPE --> SPLIT[4096자씩 나눠 전송]

    SCH[주기 Inspector] --> NT{notify_telegram?}
    NT -->|예| PUSH[허용 사용자에게 요약만 푸시]
    NT -->|아니오| SKIP[저장만]
```

`--with-watch`를 주면 Vault 감시(index)를 같이 켭니다.

---

## 7. 노트 · 교훈 · 점검

```mermaid
flowchart LR
    subgraph 노트
      V[Vault 마크다운] --> IX["rafikx index / watch"]
      IX --> FTS[(notes + FTS5)]
      FTS --> SR["search / obsidian_search 도구"]
      SR --> ASK2[ask --obsidian]
    end

    subgraph 기억
      RUN[실행 결과] --> LES[비동기 교훈 후보]
      LES --> LDB[(lessons)]
      LDB --> INJ[다음 작업에 과거 교훈 주입]
      USR["rafikx lessons add/rm"] --> LDB
    end

    subgraph 점검
      HIST[최근 runs] --> INS["rafikx inspect"]
      INS --> STAT[코드가 통계 계산]
      STAT --> LLM[모델은 분석 문장만 · 도구 없음]
      LLM --> REP[(reports + md 파일)]
      REP --> APP["--apply 시에만 교훈 저장"]
    end
```

---

## 8. 모델 순위 (월 1회)

```mermaid
flowchart TD
    BUN[번들 data/model_ranks.json] --> HOME["~/.rafikx 의 로컬 순위"]
    HOME --> AGE{30일 지났나?}
    AGE -->|예| FET[안정 JSON 갱신 시도]
    AGE -->|아니오| USE[로컬 표 사용]
    FET -->|실패| KEEP[번들/로컬 유지]
    FET -->|성공| USE
    USE --> AUTO[자동 하네스가 연결 모델과 별칭 매칭]
```

웹페이지 HTML을 긁지 않습니다. 설정 메뉴의 **지금 갱신** 또는 `rafikx ranks update`로 수동 갱신할 수 있습니다.

---

## 9. 데이터가 모이는 곳

```text
%USERPROFILE%\.rafikx\
  config.toml      모델·워크스페이스·텔레그램 (키 원문 금지)
  secrets.toml     API 키 · 봇 토큰
  auth.json        OAuth 토큰 (계정별)
  accounts.json    같은 서비스의 여러 계정
  usage.json       오늘 토큰 · 리밋 시각
  data.db          실행 이력 · 노트 검색 · 교훈 · 리포트
  reports\         점검 마크다운
  logs\agent.log   동작 기록 (키·토큰 없음)
```

---

## 10. 명령과 흐름 매핑

| 명령 | 들어가는 흐름 |
| --- | --- |
| `rafikx settings` | 2. 설정 메뉴 |
| `rafikx doctor` | 상태 점검 → 2. 설정 메뉴 |
| `rafikx ask "…"` | 3. 하네스 파이프라인 |
| `rafikx agent "…"` | 3. 파이프라인 (분류 고정 `dev`) |
| `rafikx` | 3. TTY 대화 TUI (파이프면 사용법) |
| `rafikx chat` | 3. 같은 TUI · 세션 저장 |
| `rafikx telegram` | 6. 텔레그램 → 3. 파이프라인 |
| `rafikx index` / `search` / `watch` | 7. 노트 |
| `rafikx lessons …` | 7. 교훈 |
| `rafikx inspect` / `report last` | 7. 점검 |
| `rafikx ranks` | 8. 순위 |

---

*이 문서는 구현 기준 스냅샷입니다. 동작이 바뀌면 이 파일도 같이 고치면 됩니다.*
