완료: `750a589` 정확 SHA의 7개 독립 검증 결과를 기록하고, 포화된 프로세스 정리가 8초 예산 안에서 재대기하지 않도록 정리 세션 허가와 물리 스캔 허가를 분리했으며 중복 pre-KILL 전체 스캔을 제거했다.
검증: 새 포화 회귀를 포함한 process-tree 18개와 기본 병렬 library 573개, cargo check가 통과했고 취소된 Linux blocking scan은 기존 회귀에서 실제 스캔 permit을 완료까지 유지한다.
다음: Anthropic·OpenAI 오류 본문과 PTY 환경 전달의 비밀·메모리 경계를 닫고, 0-message 요청과 ULW epoch·Ready·Answering 차단점을 별도 커밋으로 순차 해결한다. 데스크탑은 계속 제외한다.
