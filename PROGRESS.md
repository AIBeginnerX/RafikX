완료: CLI 공급자 fallback이 같은 provider의 다른 모델과 모든 계정을 순회하고 빈·손상·실패 스트림과 Codex terminal을 fail-closed로 처리하게 했다.
검증: OpenAI 호환 스트림 20개·fallback combo 6개·Anthropic 스트림 6개와 cargo check·git diff 검사가 통과했다.
다음: 프로세스 정리·동기 Bash 취소와 병렬 lifecycle·검증 후 최종 답변 경계를 마무리하고 기본·최소 전체 검증을 진행한다. 데스크탑은 제외한다.
