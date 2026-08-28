# AGENTS.md — desktop

- 스택: Tauri 2 (Rust) + 프레임워크 없는 vanilla JS/CSS
- 역할: rafikx 라이브러리의 얇은 데스크탑 껍데기. src-tauri/src/main.rs 는 세션 맵·승인 브리지·라이브 이벤트 중계만 하고, 로직은 전부 rafikx 크레이트(api.rs 파사드)에 둔다. ui/ 는 상태를 state.js 하나에 모은다.
- 관습: 새 기능은 Rust 쪽 로직을 agent-harness 에 두고, 여기서는 명령(Tauri command)과 이벤트 중계만 추가한다. UI 정책 상수(임계값 등)는 agent-harness/src/ui_policy.rs 가 원천 — JS는 ui_policy 명령으로 가져온다.
- 금지: JS 쪽에 정책 상수·비즈니스 로직을 새로 하드코딩하지 않는다. 아이콘 원본 이미지를 레포에 커밋하지 않는다 (원천은 src-tauri/icons/ 뿐).
- 검증: `cd src-tauri && cargo check`, UI 변경은 스플래시~전송~승인 버튼 수동 스모크.
