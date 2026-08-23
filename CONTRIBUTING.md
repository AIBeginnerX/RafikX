# 기여

작은 수정과 문서 개선을 환영합니다. 큰 기능은 이슈로 먼저 이야기해 주세요.

## 개발

필요: [Rust](https://rustup.rs) stable (에디션 2024, rustc 1.85+), Git.

```bash
git clone https://github.com/AIBeginnerX/rafikx.git
cd rafikx
cargo test --manifest-path agent-harness/Cargo.toml
cargo test --manifest-path agent-harness/Cargo.toml --no-default-features
```

Windows PowerShell에서는 같은 명령을 그대로 쓰면 됩니다. `cargo`가 안 보이면:

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path
```

로컬 설치:

```bash
cargo install --path agent-harness --force
```

## 규칙

- 커밋 메시지: `feat:`, `fix:`, `docs:`, `chore:` 처럼 짧게.
- 키, 토큰, `auth.json`, `secrets.toml` 을 커밋하지 않습니다.
- SPEC 5장(인터페이스)·6장(안전)의 동작을 약화시키지 않습니다.
- 새 크레이트는 꼭 필요할 때만 넣습니다.

## PR

`.github/PULL_REQUEST_TEMPLATE.md` 체크리스트를 채워 주세요. CI는 Windows, macOS, Linux에서 테스트를 돌립니다.
