완료: v1.1.0 릴리스 + 모델 관련 디버깅 3종 — ① /model refresh 자체는 정상(gemini OpenAI-호환 /models 400 → 네이티브 /v1beta/models 폴 fallback 추가, 잔여 403은 자격 증명 범위 문제로 정직 보고) ② commandcode /v1/models 404 → [providers.*] models_url 직접 지정 탈출구 ③ 요청≠응답 모델 검증(ChatResponse.model 캡처 + 불일치 시 경고 — 조용한 모델 대체를 드러냄). 테스트 348개 통과, 릴리즈 빌드 성공.
다음: v1.1.1 패치 릴리스 검토.
이슈: 없음.
