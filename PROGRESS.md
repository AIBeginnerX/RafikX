완료: v1.1.9 터미널 CLI가 이전 턴의 lifecycle을 후속 명령에 재사용하지 않고 broadcast lag를 복구하며 Booting·Ready-configured·Ready-unconfigured를 명시하고, PTY 실행 홈을 실행별로 정리한다.
검증: TUI 62개, 실제 xterm.js 9개 화면의 단일 실행과 동시 2회가 통과했고 디지털 시작 화면·한글 정렬·reduced-motion 및 실행 홈·임시 config·promotion lock 무잔존을 직접 확인했다.
다음: 기본·최소 기능 전체 테스트와 브라우저 게임 복구 E2E·릴리스 바이너리를 검증하고 문서를 갱신한 뒤 새 정확 SHA 전 레인 검토와 CLI 배포를 진행한다. 데스크탑은 계속 제외한다.
