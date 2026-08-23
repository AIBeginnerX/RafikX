#!/usr/bin/env bash
# Build RafikX desktop installers on macOS / Linux.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

python3 "$ROOT/scripts/gen-desktop-icons.py"

if ! command -v tauri >/dev/null 2>&1; then
  echo "Installing tauri-cli 2..."
  cargo install tauri-cli --locked --version "^2"
fi

OS="$(uname -s)"
case "$OS" in
  Darwin) BUNDLES="${1:-dmg}" ;;
  Linux)  BUNDLES="${1:-appimage,deb,rpm}" ;;
  *)      BUNDLES="${1:-}" ;;
esac

cd "$ROOT/desktop/src-tauri"
if [ -n "$BUNDLES" ]; then
  cargo tauri build --bundles "$BUNDLES"
else
  cargo tauri build
fi

echo
echo "Bundles under desktop/src-tauri/target/release/bundle/"
find "$ROOT/desktop/src-tauri/target/release/bundle" -type f 2>/dev/null | head -50
