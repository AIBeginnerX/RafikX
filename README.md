# RafikX

터미널과 텔레그램에서 쓰는 **개인용 AI 코딩 에이전트**입니다.

할 일을 말하면 난이도를 나누고, 연결해 둔 모델과 계정 중에서 실행합니다. 같은 모델에 계정을 여러 개 두면 리밋이 난 쪽을 쉬게 하고 다른 계정으로 넘어갑니다.

```text
rafikx
rafikx ask "이 폴더 README를 짧게 요약해줘"
```

Windows · macOS · Linux. 명령 이름은 `rafikx` 입니다.

[설치](docs/INSTALL.md) · [보안](SECURITY.md) · [기여](CONTRIBUTING.md) · [운영 흐름](RAFIKX_WORKFLOW.html)

---

## 한 줄 설치

**macOS / Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/AIBeginnerX/rafikx/master/install.sh | bash
```

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/AIBeginnerX/rafikx/master/install.ps1 | iex
```

설치 후:

```bash
rafikx --version
rafikx
```

Rust가 없으면 스크립트가 rustup을 넣습니다. 첫 설치는 컴파일이라 몇 분 걸릴 수 있습니다.  
저장소가 아직 공개 전이면 [docs/INSTALL.md](docs/INSTALL.md) 의 **로컬 설치**를 쓰세요.

### 사설 저장소에서 다른 PC에 설치 (private 동안)

원라이너는 공개 저장소 전용입니다. private 동안에는 git 인증 후 클론으로 설치하세요.

```powershell
# Windows
git clone https://github.com/AIBeginnerX/RafikX.git
cd RafikX
cargo install --path agent-harness --force          # 터미널 (rafikx)
powershell -ExecutionPolicy Bypass -File scripts/build-desktop.ps1   # 데스크탑(선택)
```

```bash
# macOS / Linux
git clone https://github.com/AIBeginnerX/RafikX.git
cd RafikX && cargo install --path agent-harness --force
```

GitHub 인증이 필요하면 `gh auth login` 또는 자격증명 관리자에 PAT를 등록해 두세요.  
설치 확인: `rafikx --version` · 상태: `rafikx status` · 연결: `rafikx login` 또는 대화 화면 `/connect`.

---

## 환경

| | |
| --- | --- |
| OS | Windows 10+, macOS 12+, Linux |
| Rust | 1.85+ (에디션 2024). 한 줄 설치가 처리 |
| Git | 한 줄 설치에 필요 |
| 선택 | AI 계정(로그인 또는 키), 텔레그램 봇, [Ollama](https://ollama.com) |

macOS는 Xcode Command Line Tools (`xcode-select --install`)가 필요합니다.  
Windows는 rustup이 C++ 빌드 도구를 요청할 수 있습니다.

설정 폴더: `~/.rafikx` (Windows는 `%USERPROFILE%\.rafikx`).

### 자동 로그인 가져오기 (OAuth)

Claude Code · Codex CLI · Gemini CLI 중 하나라도 로컬에 로그인돼 있으면,
RafikX 실행 시 **별도 설정 없이** 그 로그인 정보를 자동으로 가져와 anthropic · openai · gemini 연결에 사용합니다.
가져오기는 프로세스당 1회만 시도하고, 이미 연결된 서비스는 건드리지 않습니다.
직접 로그인하려면 `rafikx` 첫 화면 또는 `/connect` 에서 브라우저 로그인·키 붙여넣기도 가능합니다.
참고: xAI(Grok)는 공개 OAuth가 없어 콘솔 키 붙여넣기 방식입니다.

### 기본 내장 프로바이더

anthropic · openai · gemini · grok(xAI) · openrouter · opencode_zen · opencode_go · **minimax** · **commandcode** · groq · deepseek · mistral · together · fireworks · moonshot · glm · perplexity · cohere · qwen · local(Ollama).

- **MiniMax**: [platform.minimax.io](https://platform.minimax.io) 에서 키 발급 (`MINIMAX_API_KEY`), OpenAI 호환 `https://api.minimax.io/v1`, 모델 예 `MiniMax-M2`.
- **CommandCode**: [commandcode.ai](https://commandcode.ai) (`COMMANDCODE_API_KEY`), base_url 은 config.toml 에서 조정 가능.

---

## 5분 사용

1. `rafikx` — 터미널에서 대화 화면이 열립니다. 처음이면 Zen / Go / Claude 등 하나를 고르고 키를 붙이거나 로그인합니다.
2. 같은 서비스를 한 번 더 고르면 **계정을 추가**할 수 있습니다 (`rafikx login` 또는 `rafikx settings`).
3. 한 줄로만 질문하려면 `rafikx ask "안녕"`.
4. 파일을 고치려면 대화에서 말하거나 `rafikx agent "hello.txt 만들어줘"`.
5. 폰에서 쓰려면 설정에서 봇 토큰을 넣고 `rafikx telegram`.

| 하고 싶은 일 | 명령 |
| --- | --- |
| 대화 화면 | `rafikx` 또는 `rafikx chat` |
| 연결 | `rafikx login` |
| 설정 | `rafikx settings` |
| 상태 점검 | `rafikx doctor` |
| 한 줄 질문 | `rafikx ask "…"` |
| 코딩 | `rafikx agent "…"` |
| 텔레그램 | `rafikx telegram` |
| 노트 인덱스 | `rafikx index` |
| 하네스 모드·분류별 모델 | `rafikx harness` (`--mode auto\|manual`, `<분류> <모델>`) |
| 사용 가능한 원격 모델 | `rafikx models <서비스>` |
| 지난 세션 검색 | `rafikx find <검색어>` |
| 화면 테마·배경 | `rafikx theme` / `rafikx workspace <경로>` |

**키와 봇 토큰을 README·이슈·채팅에 붙이지 마세요.**

---

## 하는 일

- **프로바이더·모델 선택**: 대화에서 `/model` `/provider` `/connect`. 팝업은 **타이핑으로 바로 검색**되고 ↑↓·Enter 로 고릅니다.
- **모델 자동 조회**: 새 서비스를 연결해 키가 정상이면 사용 가능한 모델을 자동으로 불러와 저장하고, 순위 기준 기본 모델까지 골라 저장합니다. 이후 `/model` 에서 목록이 바로 보입니다.
- **자동 하네스**: 설계·검증·디버깅은 등록한 모델 중 추론 순위가 높은 것을 씁니다. 수동으로 바꿀 수 있습니다.
- **계정 전환**: 리밋이 먼저 끝난 계정을 쓰고, 429면 다음 계정으로 갑니다. 하단에서 사용량을 봅니다.
- **안전**: 워크스페이스 밖 파일 차단, 위험한 bash 차단, 텔레그램 허용 목록 밖은 무응답, 원격 `--yes` 금지.
- **기억**: 교훈 주입, Obsidian 검색. Inspector는 코드를 자동으로 고치지 않습니다.

### opencode급 도구 · 모드 (v0.2)

- **도구 15종**: read_file · list_dir · grep · glob · webfetch · web_search · edit_file · multi_edit · write_file · apply_patch · bash · todo_write · todo_read · obsidian_search · task(서브에이전트 위임).
  - `web_search` 는 키 없이 웹 검색 결과(제목·URL·요약)를 돌려주고, 자세한 본문은 `webfetch` 로 읽습니다.
  - `apply_patch` 는 여러 파일의 추가/수정/삭제를 codex 스타일 패치 한 번으로 적용합니다.
  - `bash` 는 `timeout_secs`(5~600초), `grep` 은 `context`(앞뒤 문맥 줄) 파라미터를 지원합니다.
- **plan / build 모드**: `/mode plan` 은 읽기 전용으로 계획만 세우고, `/mode build` 로 실행합니다.
- **하네스 엔진 선택**: `/engine rafikx|deepseek` — 기본은 **rafikx harness**. **deepseek harness** 는 DeepSeek Harness(dsh)의 단계별 실행 방식을 본뜬 것으로, 모든 도구 작업을 todo 단계로 쪼개 실행하고 단계 상태와 검증 결과를 보고합니다. 데스크탑은 관리자 › 하네스 탭에서 고릅니다.
- **난이도 기반 단계 실행**: 단순 업무는 즉답하고, medium 이상은 자동으로 todo 단계(2~6개)를 등록해 순서대로 처리합니다.
- **TUI 진행바**: 실행 중 파란 그라데이션 디지털 바가 현재 단계(모델 호출 · 도구 실행 · 반복 횟수)를 실시간 표시합니다.
- **명령 팔레트**: 입력창에 `/` 를 치면 하단에 일치하는 명령 최대 5개와 총 개수가 나타납니다.
- **세션 명령**: `/sessions` `/resume <id>` `/compact`(대화 요약 압축) `/undo` `/tools` `/todo`.
- **파일 첨부**: `@src/main.rs` 멘션 또는 `/file <경로>` 로 다음 질문에 파일을 붙입니다.
- **테마**: `/theme rafikx|opal|synth` — config `[ui] theme` 에 저장됩니다.
- `task` 도구와 plan 모드도 같은 하네스 분류·모델 자동선택을 그대로 통과합니다.

상세 흐름: [RAFIKX_WORKFLOW.html](RAFIKX_WORKFLOW.html) (브라우저에서 열기).

---

## 개발

```bash
git clone https://github.com/AIBeginnerX/rafikx.git
cd rafikx
cargo test --manifest-path agent-harness/Cargo.toml
cargo install --path agent-harness --force
```

소스 크레이트 폴더는 `agent-harness/` 입니다. 제품 이름은 RafikX 입니다.

라이선스: [MIT](LICENSE).

---

## 데스크탑 앱

CLI와 **같은 하네스**를 쓰는 가벼운 창입니다 (Tauri 2, Electron 아님). 채팅, 연결/설정, 세션, Obsidian, 실행 그래프. 코드펜스 하이라이트, 메시지 복사 버튼, 파일 드래그&드롭(@경로 첨부)을 지원합니다.

자세한 설치·빌드: [docs/INSTALL.md](docs/INSTALL.md#데스크탑-앱).

**Windows (이 저장소에서)**

```powershell
powershell -File scripts/build-desktop.ps1
```

끝나면 `desktop/src-tauri/target/release/bundle/nsis/` 아래 `RafikX_*_x64-setup.exe` 가 생깁니다.

**macOS**

```bash
chmod +x scripts/build-desktop.sh
./scripts/build-desktop.sh dmg
```

**Linux**

```bash
./scripts/build-desktop.sh appimage,deb,rpm
```

Linux는 WebKitGTK 개발 패키지가 필요합니다 (`libwebkit2gtk-4.1-dev` 등). 개발 실행:

```bash
cargo tauri dev --config desktop/src-tauri/tauri.conf.json
# 또는
cd desktop/src-tauri && cargo tauri dev
```
