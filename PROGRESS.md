완료: Anthropic 스트리밍이 블록 index 순서대로 text와 tool_use를 복원하고, 분할 input_json_delta를 완성된 객체로 만든 뒤에만 도구 호출로 전달한다.
검증: 교차 블록·분할 JSON·8KB 진행·잘못된 입력·max_tokens 절단 회귀를 포함한 Anthropic 테스트 5개와 cargo check가 통과했으며 기존 경고만 남았다.
다음: 공개 StreamEvent 호환성을 유지하는 내부 semantic stream으로 추론·후보 본문·최종 답변을 구분해 첫 답변보다 Answering을 먼저 표시한다. 데스크탑은 계속 제외한다.
