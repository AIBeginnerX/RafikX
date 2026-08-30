완료: 공개 StreamEvent 계약을 유지한 내부 semantic stream이 추론·후보 본문·도구 진행을 구분하고, 오염 후보를 차단한 뒤 최종 답변보다 Answering을 먼저 TUI에 투영한다.
검증: provider 22개·agent 10개·runner 16개·TUI 14개·공개 계약 4개와 cargo check가 통과했고, 실제 PTY 9개 상태의 첫 최종 답변 프레임에서 Answering을 확인했다.
다음: 릴리스 문서와 증거 수치를 현재 구현에 맞추고 기본·최소 기능 전체 테스트와 브라우저 게임 E2E를 거친 뒤 정확 SHA 다중 검토를 실행한다. 데스크탑은 계속 제외한다.
