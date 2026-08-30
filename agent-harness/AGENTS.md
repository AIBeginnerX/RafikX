# AGENTS.md — agent-harness

- 스택: Rust (Cargo), 에디션 2024, Rust 1.96+
- 역할: 제품의 두뇌 전부 — 에이전트 루프·도구 24종·Harness(엔진 카탈로그·분야·팀 모드)·메모리(lessons·facts)·ulw 자율 루프·스킬·MCP·LSP·텔레그램 데몬. CLI 바이너리와 라이브러리를 함께 제공한다.
- 관습: 모듈 경계를 지킨다 — harness/ 는 분류·바인딩·프로파일·팀·실행만, 도구는 tools/, 표면 로직(tui/api/telegram)은 두뇌 함수를 호출만 한다. 동기 Tool::run 안에서 비동기는 block_in_place 패턴을 따른다.
- 금지: 승인 게이트·경로 jail·bash 차단목록을 우회하는 코드. 새 외부 크레이트. tests 의 실제 config/DB 오염(임시 디렉터리를 쓴다).
- 검증: `cargo check && cargo test` (기준선 290개+), 릴리즈는 `cargo build --release`.
