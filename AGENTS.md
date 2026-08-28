# AGENTS.md — RafikX

- 스택: Rust 2024 (agent-harness 크레이트) + Tauri 2 (desktop)
- 역할: 터미널·데스크탑·텔레그램에서 쓰는 개인용 AI 코딩 에이전트의 단일 레포. 두뇌는 전부 `agent-harness/` 의 rafikx 크레이트에 있고, 나머지는 얇은 껍데기다.
- 관습: 기존 코드 스타일·명명·구조를 따른다. 새 외부 크레이트 추가는 금지 — 꼭 필요하면 사유와 대안을 먼저 보고하고 승인을 받는다 (SPEC 운영 규칙). Phase 작업은 한 세션 한 Phase, Phase마다 git 커밋, PROGRESS.md 3줄 갱신.
- 금지: 빌드 산출물(target/)·비밀값(키·토큰)·개인 작업 증거(.omo/)를 커밋하지 않는다. `5장 인터페이스 계약`·`6장 안전장치`(SPEC.md)는 합의 없이 수정하지 않는다.
- 검증: `cd agent-harness && cargo check && cargo test` — 기준선 290개+, 줄면 회귀 의심. 문서(SPEC/README)보다 구현이 기준이다.
