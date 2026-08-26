# 설치

명령 이름은 **`rafikx`** 입니다. Windows, macOS, Linux에서 같은 소스를 씁니다.

기본 저장소는 `https://github.com/AIBeginnerX/RafikX` 입니다. 포크를 쓰려면 `RAFIKX_REPO` 로 바꿀 수 있습니다.

## 한 줄 설치

### macOS / Linux

터미널:

```bash
curl -fsSL https://raw.githubusercontent.com/AIBeginnerX/RafikX/master/install.sh | bash
```

끝나면:

```bash
source "$HOME/.cargo/env"
rafikx --version
rafikx
```

### Windows

PowerShell:

```powershell
irm https://raw.githubusercontent.com/AIBeginnerX/RafikX/master/install.ps1 | iex
```

끝나면 **새 PowerShell**을 열거나:

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path
rafikx --version
rafikx
```

스크립트는 Rust가 없으면 rustup을 넣고, 소스를 `~/.rafikx-src` 에 받은 뒤 `cargo install` 합니다. 첫 설치는 컴파일 때문에 **수 분** 걸릴 수 있습니다.

다른 저장소/브랜치를 쓰려면:

```bash
RAFIKX_REPO=내계정/rafikx RAFIKX_BRANCH=master bash install.sh
```

```powershell
$env:RAFIKX_REPO = "내계정/rafikx"
irm https://raw.githubusercontent.com/내계정/rafikx/master/install.ps1 | iex
```

## 환경

| 항목 | 요구 |
| --- | --- |
| OS | Windows 10+, macOS 12+, 최근 Linux |
| 디스크 | 소스+컴파일 중 약 2GB, 설치 후 실행 파일은 수 MB |
| 네트워크 | 설치 시 crates.io, 사용 시 선택한 AI API |
| Rust | rustc 1.85 이상 (에디션 2024). 스크립트가 설치해 줍니다 |
| Git | 한 줄 설치에 필요 |
| Windows 추가 | Visual Studio Build Tools C++ (rustup이 안내할 수 있음) |
| macOS 추가 | Xcode Command Line Tools: `xcode-select --install` |

선택:

- 텔레그램 봇 토큰 (폰에서 쓸 때)
- Anthropic / OpenAI / Gemini 로그인 또는 API 키
- OpenCode Zen (`OPENCODE_API_KEY`) / OpenCode Go (`OPENCODE_GO_API_KEY`) — 키는 [opencode.ai/auth](https://opencode.ai/auth)
- 로컬만 쓰려면 [Ollama](https://ollama.com)

## Rust가 이미 있을 때

```bash
git clone https://github.com/AIBeginnerX/RafikX.git
cd rafikx
cargo install --path agent-harness --locked --force
rafikx --version
```

텔레그램 없이 더 작게:

```bash
cargo install --path agent-harness --locked --force --no-default-features
```

## 설정이 생기는 곳

| OS | 폴더 |
| --- | --- |
| Windows | `%USERPROFILE%\.rafikx\` |
| macOS / Linux | `~/.rafikx/` |

예전에 `~/.agent-harness` 만 있으면 그걸 그대로 씁니다. 직접 지정: 환경 변수 `RAFIKX_HOME`.

키는 `secrets.toml`, 로그인은 `auth.json` 에만 저장됩니다. `config.toml` 에 키를 적지 마세요.

이미 있는 설정 폴더에는 없는 프로바이더(OpenCode Zen/Go 등)가 다음 실행 때 `config.toml` 끝에 자동으로 붙습니다.

| 환경변수 | 서비스 |
| --- | --- |
| `OPENCODE_API_KEY` | OpenCode Zen (Go도 이 키로 가능) |
| `OPENCODE_GO_API_KEY` | OpenCode Go |
| `ANTHROPIC_API_KEY` | Anthropic |
| `OPENAI_API_KEY` | OpenAI |
| `OPENROUTER_API_KEY` | OpenRouter |

PowerShell 예:

```powershell
$env:OPENCODE_API_KEY = "여기에-키"
rafikx
```

또는 `rafikx login` 에서 zen / go 를 고르고 붙여넣기.

이미 있는 설정 폴더에는 없는 프로바이더(OpenCode Zen/Go 등)가 다음 실행 때 `config.toml` 끝에 자동으로 붙습니다.

| 환경변수 | 서비스 |
| --- | --- |
| `OPENCODE_API_KEY` | OpenCode Zen (Go도 이 키로 가능) |
| `OPENCODE_GO_API_KEY` | OpenCode Go |
| `ANTHROPIC_API_KEY` | Anthropic |
| `OPENAI_API_KEY` | OpenAI |
| `OPENROUTER_API_KEY` | OpenRouter |

PowerShell 예:

```powershell
$env:OPENCODE_API_KEY = "여기에-키"
rafikx
```

또는 `rafikx login` 에서 zen / go 를 고르고 붙여넣기.

## 데스크탑 앱

터미널 없이 같은 RafikX를 쓰려면 Tauri 창을 빌드합니다. 에이전트 루프·도구·승인·`~/.rafikx` 설정은 CLI와 하나입니다.

필요: Rust 1.85+, Windows는 WebView2 (Windows 10에 보통 포함), macOS는 Xcode CLT, Linux는 WebKitGTK 4.1 개발 패키지.

아이콘은 저장소 스크립트가 만듭니다. `tauri-cli` 2 가 없으면 스크립트가 `cargo install` 합니다.

### Windows — 설치 파일 (setup.exe)

```powershell
powershell -File scripts/build-desktop.ps1
```

NSIS 설치 파일:

`desktop/src-tauri/target/release/bundle/nsis/RafikX_*_x64-setup.exe`

MSI 도 만들려면:

```powershell
powershell -File scripts/build-desktop.ps1 nsis,msi
```

개발 실행 (설치 없이):

```powershell
cd desktop/src-tauri
cargo install tauri-cli --locked --version "^2"   # 한 번만
cargo tauri dev
```

### macOS — DMG

```bash
chmod +x scripts/build-desktop.sh
./scripts/build-desktop.sh dmg
```

유니버설 바이너리는 Apple Silicon + Intel 양쪽에서 `rustup target add` 후 Tauri `bundle.macOS.targets` / `cargo tauri build --target universal-apple-darwin` 을 쓰면 됩니다. 한 아키텍처 DMG가 기본입니다.

### Linux — AppImage · deb · rpm

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf
./scripts/build-desktop.sh appimage,deb,rpm
```

결과물은 `desktop/src-tauri/target/release/bundle/` 아래 `appimage/`, `deb/`, `rpm/` 입니다.

### 수동 (`cargo tauri build`)

```bash
python scripts/gen-desktop-icons.py    # 또는 python3
cd desktop/src-tauri
cargo tauri build
```

Windows에서는 호스트가 지원하는 번들만 실제로 만들어집니다. 다른 OS 설치 파일은 해당 OS에서 위 명령을 실행하세요.

## 확인

```text
rafikx --version    →  rafikx 1.0.0
rafikx              →  대화 화면 (TTY). 처음이면 짧은 연결 마법사
rafikx login        →  Zen / Go / Claude 등 연결
rafikx settings     →  설정
rafikx ask "안녕"   →  연결한 모델로 답
```

## 제거

```bash
cargo uninstall rafikx
```

설정 폴더(`~/.rafikx`)는 자동으로 지워지지 않습니다. 필요하면 직접 삭제하세요.

## 문제

- `rafikx` 를 찾을 수 없음 → `~/.cargo/bin` 이 PATH에 있는지 확인. 터미널을 다시 엽니다.
- Windows에서 링크 오류 → C++ 빌드 도구를 설치한 뒤 다시 `install.ps1`.
- macOS에서 git/compiler 없음 → `xcode-select --install`.
- 컴파일 오래 걸림 → 정상입니다. 한 번 설치하면 이후는 `rafikx` 만 실행합니다.
