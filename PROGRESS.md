완료: ULW가 같은 observer를 재사용해도 각 루트 실행이 새 lifecycle epoch를 소유하며, boot 완료 뒤 Ready 화면은 tick redraw와 sparkle을 모두 멈춘다.
검증: 이전 루트의 늦은 AnswerStarted 격리와 정착 배너 회귀가 통과했고, 실제 PTY는 Ready 뒤 0.5초 동안 추가 출력이 없었으며 Node 문법 검사도 통과했다.
다음: Anthropic 스트리밍 tool_use를 복원한 뒤 내부 semantic stream 경계로 추론·후보 본문·최종 답변을 구분해 첫 답변보다 Answering을 먼저 표시한다. 데스크탑은 계속 제외한다.
