# 진행·대기 작업 (todo.md)

> 완료된 항목은 docs/CHANGELOG-작업기록.md 로 이동한다. 여기에는 미완료만 둔다.
> 상태: 대기 / 진행 / 보류

## v5 기획 진행

- [ ] (대기) Phase F7 — 규칙 주입 + /init-deep
- [ ] (대기) Phase F8 — 콤보 폴 fallback + /quota
- [ ] (대기) Phase F9 — 팁 시스템

## 잔여 관찰 (수정 보류)

- [ ] (보류) Esc 인터럽트 시 finish_run 미호출 → runs 에 미종결 행 잔존
      (2026-08-26 기록. goals 는 failed/complete 라 auto-resume 위험 없음)
- [ ] (보류) minimax verify 되먹임 vs 사용자 지시 충돌 시 비일관
      (2026-08-25 Self-Harness 실측 관찰. 승격된 v1 지시문이 완화 방향)
- [ ] (보류) lessons::maybe_spawn 도 단발 CLI 종료 시 교훈 유실 가능
      (기존 동작, Self-Harness 의 flush_observations 와 같은 항목)
- [ ] (보류) chat 단위테스트가 실제 config 를 대상으로 실행됨 — 병렬 실행 시
      engine 값 오염 가능 (기존 테스트 설계)
- [ ] (보류) 데스크탑 index.html:602 stale allowlist 버그 (dk/pi 미표시)
