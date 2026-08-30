완료: v1.1.9 터미널 CLI 최종 후보 — 초과 도구 쌍 원자 제외, Linux blocking 탐색 허가 수명, macOS 세대 재검증, 명시적 lifecycle 상태 문구와 안정된 취소 PTY를 완성했다.
검증: 기본 591개·최소 기능 530개 테스트, Chrome 50개, process-tree 16개, 게임 복구 E2E 29.43초와 실제 xterm.js 9개 화면·동시 2회 실행을 통과했다. LSP 3개와 릴리스 E2E 1개는 기본 실행에서 ignore다.
다음: 현재 후보의 정확 SHA를 GPT-5.6 Sol 코드·보안·아키텍처·계약·수동 QA 5/5와 시각·CJK 게이트에 통과시킨 뒤 master push → Rust 1.96·Ubuntu·Windows·macOS CI → v1.1.9 태그·GitHub Release → 설치본 update를 완료한다. 데스크탑은 계속 제외한다.
