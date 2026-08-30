# 진행·대기 작업 (todo.md)

> 완료된 항목은 docs/CHANGELOG-작업기록.md 로 이동한다. 여기에는 미완료만 둔다.
> 상태: 대기 / 진행 / 보류

## v5 기획 진행


- [ ] (대기) Inspector 리포트의 ulw 섹션 — 다중 워크스페이스 합산 여부 결정

## v1.1.9 터미널 CLI 릴리스 (진행)

- [x] `53c004e` 정확 SHA의 코드·보안·아키텍처·계약·QA·시각·CJK 검증 결과 기록
- [x] 프로세스 탐색 허가 대기와 정리 제한 시간의 경쟁을 제거하고 retained-scope 자손 생존 회귀 고정
- [x] 시스템·도구·출력 예약을 포함한 요청 총예산 강제와 ToolUse/ToolResult ID·개수 무결성 고정
- [x] OpenAI 호환 공급자의 임의 오류 본문이 영구 디버그 로그에 기록되지 않도록 경계 정제
- [x] 후속 명령의 오래된 lifecycle 상태 제거와 booting·ready-unconfigured·ready-configured 표시 구분
- [x] PTY 실행별 `RAFIKX_HOME` 정리와 준비 상태 실제 터미널 증거 추가
- [x] 기본·최소 기능 전체 테스트, 브라우저 게임 복구 E2E, 릴리스 바이너리, 동시 PTY 검증
- [x] `750a589` 정확 SHA의 GPT-5.6 Sol 코드·보안·아키텍처·계약·QA·시각·CJK 결과 기록
- [x] 정리 세션 허가와 실제 프로세스 스캔 허가를 분리하고 중복 pre-KILL 전체 스캔 제거
- [x] Anthropic·OpenAI 오류 본문과 PTY 환경 전달의 비밀·메모리 경계 고정
- [ ] (진행) 메시지 예산 0인 요청의 provider 호출 차단과 현재 사용자 작업 보존
- [ ] (대기) ULW 연속 실행 epoch와 정적인 Ready·실제 Answering lifecycle 고정
- [ ] (대기) 새 정확 SHA의 GPT-5.6 Sol 독립 검증 전 레인 통과
- [ ] (대기) master push → 정확 SHA CLI CI → v1.1.9 태그·GitHub Release → 설치본 `rafikx update`

## 터미널 릴리스 이후

- [ ] (대기) v1.1.9 터미널 릴리스 완료 뒤 데스크탑 전용 분석·개선·검증·배포 Phase 시작
      (현재 CLI Phase에서는 desktop/Tauri 소스와 빌드를 제외)

## 잔여 관찰 (수정 보류)

- [ ] (보류) Esc 인터럽트 시 finish_run 미호출 → runs 에 미종결 행 잔존
      (2026-08-26 기록. goals 는 failed/complete 라 auto-resume 위험 없음)
- [ ] (보류) minimax verify 되먹임 vs 사용자 지시 충돌 시 비일관
      (2026-08-25 Self-Harness 실측 관찰. 승격된 v1 지시문이 완화 방향)
- [ ] (보류) lessons::maybe_spawn 도 단발 CLI 종료 시 교훈 유실 가능
      (기존 동작, Self-Harness 의 flush_observations 와 같은 항목)
- [ ] (보류) chat 단위테스트가 실제 config 를 대상으로 실행됨 — 병렬 실행 시
      engine 값 오염 가능 (기존 테스트 설계)
- [ ] (보류) 저장소 전체 rustfmt·Clippy 기존 부채 정리 — 동작 변경과 분리한
      전용 Phase에서 경고·포맷 차이를 기준선부터 정리
- [ ] (보류) 환경 의존 LSP 실동작 테스트 3개 — rust-analyzer·typescript-language-server
      설치가 허용된 검증 환경에서 ignore 해제 후 교차 플랫폼 실행
- [ ] (보류) PTY 증거 promotion lock의 강제 종료 복구 — owner PID·process start identity를
      기록하고 실제 소유자가 없을 때만 회수하는 교차 플랫폼 계약을 별도 Phase로 설계
- [ ] (보류) 터미널 시각 QA 확장 — NO_COLOR, OPAL/SYNTH/CLAUDE 테마, emoji/ZWJ
      grapheme 폭과 steady EXECUTE 상태를 실제 PTY 증거로 추가
- [ ] (보류) 최초 설치 스크립트도 mutable master 대신 검증된 안정 태그·커밋을 고정하도록
      updater와 같은 불변 설치 계약으로 통합
- [ ] (보류) 데스크탑 index.html:602 stale allowlist 버그 (dk/pi 미표시)
