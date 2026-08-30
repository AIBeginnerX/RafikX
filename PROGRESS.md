완료: v1.1.9 터미널 CLI의 프로세스 정리·요청 예산·도구 쌍·provider 오류 로그·lifecycle 준비 상태·PTY 정리 차단점을 단계별 커밋으로 닫고 릴리스 후보 문서를 현재 구현과 맞췄다.
검증: 기본 605개·최소 기능 539개, process-tree 17개, TUI 62개, 게임 복구 E2E 29.03초, xterm.js 9개 화면 동시 2회, Clippy와 릴리스 `RafikX 1.1.9`가 통과했다. LSP 3개와 릴리스 E2E 1개는 기본 실행에서 ignore다.
다음: 새 후보의 정확 SHA를 GPT-5.6 Sol 코드·보안·아키텍처·계약·수동 QA와 시각·CJK 전 레인에 통과시킨 뒤 push → CLI CI → v1.1.9 Release → 설치본 update를 완료한다. 데스크탑은 계속 제외한다.
