#!/usr/bin/env bash
# RafikX one-line install for macOS and Linux.
#   curl -fsSL https://raw.githubusercontent.com/AIBeginnerX/RafikX/master/install.sh | bash
set -euo pipefail

REPO="${RAFIKX_REPO:-AIBeginnerX/RafikX}"
BRANCH="${RAFIKX_BRANCH:-master}"
SRC="${RAFIKX_SRC:-$HOME/.rafikx-src}"

say() { printf '\n==> %s\n' "$*"; }
die() { printf '오류: %s\n' "$*" >&2; exit 1; }

need_cmd() { command -v "$1" >/dev/null 2>&1; }

if [[ "$(uname -s)" == "Darwin" ]] || [[ "$(uname -s)" == "Linux" ]]; then
  :
else
  die "이 스크립트는 macOS와 Linux용입니다. Windows는 install.ps1 을 쓰세요."
fi

say "Rust 확인"
if ! need_cmd cargo; then
  say "Rust가 없어 rustup을 설치합니다 (https://rustup.rs)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi
# shellcheck disable=SC1091
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"
need_cmd cargo || die "cargo 를 찾지 못했습니다. 터미널을 다시 연 뒤 재시도하세요."
rustup toolchain install stable >/dev/null
rustup default stable >/dev/null

say "소스 받기 ($REPO @$BRANCH)"
if [[ -d "$SRC/.git" ]]; then
  git -C "$SRC" fetch --depth 1 origin "$BRANCH"
  git -C "$SRC" checkout -q "$BRANCH"
  git -C "$SRC" pull --ff-only origin "$BRANCH"
elif [[ -e "$SRC" ]]; then
  die "$SRC 가 Git 저장소가 아니어서 덮어쓰지 않았습니다. 경로를 옮기거나 RAFIKX_SRC를 지정하세요."
else
  need_cmd git || die "git 이 필요합니다. (macOS: xcode-select --install)"
  git clone --depth 1 --branch "$BRANCH" "https://github.com/${REPO}.git" "$SRC"
fi

say "rafikx 설치"
cargo install --path "$SRC/agent-harness" --locked --force

BIN="$HOME/.cargo/bin/rafikx"
[[ -x "$BIN" ]] || die "설치는 끝났지만 $BIN 을 찾지 못했습니다."

say "설치 완료"
"$BIN" --version || true
echo
echo "새 터미널을 열거나 아래를 실행하세요:"
echo "  source \"\$HOME/.cargo/env\""
echo "  rafikx --version"
echo "  rafikx"
echo
echo "설정 폴더: ~/.rafikx"


# ── 보안 감사 도구 (best-effort) ─────────────────────────────────────────────
# 품질 게이트(S5)의 의존성 감사에 사용. 실패해도 설치는 계속된다.
if command -v cargo >/dev/null 2>&1; then
  if ! command -v cargo-audit >/dev/null 2>&1; then
    echo "[install] cargo-audit 설치 중 (보안 감사 — 몇 분 걸릴 수 있음)…"
    cargo install cargo-audit --locked >/dev/null 2>&1 \
      && echo "[install] cargo-audit 설치 완료" \
      || echo "[install] cargo-audit 설치 실패 — 나중에 'cargo install cargo-audit' 로 설치하세요"
  fi
fi
