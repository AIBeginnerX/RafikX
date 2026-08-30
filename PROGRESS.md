완료: Anthropic·OpenAI 호환 오류 응답은 상태 확인 뒤 임의 body를 읽거나 저장하지 않으며, Anthropic 스트림 오류도 원문을 폐기하고 PTY 자식 환경은 터미널·locale·PATH allowlist만 전달한다.
검증: provider 관련 16개 테스트, Node 문법 검사와 실제 9-state PTY/xterm 실행이 통과했고 실행별 home·config·staging 정리도 유지됐다. TypeScript LSP는 workspace 설치 부재와 사용자 설치 보류 때문에 실행하지 않았다.
다음: 출력 여유가 양수여도 메시지 예산이 0이면 현재 사용자 작업을 지운 채 provider를 호출하지 않도록 패커와 세 호출 경계를 고정한다. 데스크탑은 계속 제외한다.
