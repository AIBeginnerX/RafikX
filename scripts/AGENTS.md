# AGENTS.md — scripts

- 스택: Bash / PowerShell
- 역할: 설치(install.sh·install.ps1)·데스크탑 빌드 등 운영 스크립트. 한 줄 설치의 진입점이므로 실패 시 메시지가 사용자가 다음 행동을 알 수 있어야 한다.
- 관습: bash는 `set -euo pipefail`, PowerShell은 `$ErrorActionPreference = "Stop"` 을 지킨다. OS 분기는 스크립트 안에서 처리하고 문서에는 한 줄만 남긴다.
- 금지: 사용자 환경을 묻지 않고 파괴적으로 바꾸는 명령(rm -rf, 기존 설정 덮어쓰기). 비밀값 하드코딩.
- 검증: `bash -n <스크립트>` (구문 검사) + 변경 시 깨끗한 환경 기준으로 한 번 실행.
