#!/usr/bin/env bash
# RafikX macOS bootstrap - run this AFTER getting the code (ZIP download or git clone).
# Installs missing prerequisites (Xcode CLT, Rust toolchain) then builds & installs rafikx.
#
# Usage:
#   bash scripts/bootstrap-macos.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

say()  { printf '\n==> %s\n' "$*"; }
ok()   { printf 'OK  %s\n' "$*"; }

ARCH="$(uname -m)"          # arm64 (Apple Silicon) 또는 x86_64 (Intel)
say "RafikX macOS bootstrap ($ARCH)"

# 1) Xcode Command Line Tools (git + clang linker)
if ! xcode-select -p >/dev/null 2>&1; then
  say "Xcode Command Line Tools 설치 창을 띄웁니다 - 설치 후 이 스크립트를 다시 실행하세요"
  xcode-select --install
  exit 1
fi
ok "Command Line Tools ($(xcode-select -p))"

# 2) Rust toolchain
if ! command -v cargo >/dev/null 2>&1; then
  say "Rust 미발견 - rustup 설치"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
fi
# shellcheck disable=SC1091
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"
command -v cargo >/dev/null 2>&1 || { echo "오류: cargo 를 찾지 못했습니다. 새 터미널에서 재시도하세요."; exit 1; }
rustup default stable >/dev/null 2>&1 || true
ok "cargo ($(cargo --version))"

# 3) 빌드 & 설치
say "rafikx 빌드·설치 (몇 분 소요)"
cd "$ROOT/agent-harness"
cargo install --path . --force
cd "$ROOT"

say "완료"
echo "확인 : $HOME/.cargo/bin/rafikx --version"
echo "연결 : rafikx model      (마법사 - 서비스 선택/키 등록/모델 선택)"
echo "상태 : rafikx status     · 진단: rafikx doctor"
echo "대화 : rafikx            (TUI, /connect 로도 연결 가능)"
echo ""
echo "데스크탑 앱이 필요하면:" 
echo "  ./scripts/build-desktop.sh dmg     (RafikX_*.dmg 생성)"
