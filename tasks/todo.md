# Self-Harness 엔진 적용 (arXiv:2606.09498)

논문 "Self-Harness: Harnesses That Improve Themselves"의 3단계 루프
(Weakness Mining → Harness Proposal → Proposal Validation)를 RafikX의
`/engine self` 엔진으로 구현한다.

## 논문 → RafikX 번역 결정

- 논문은 벤치마크(held-in/held-out 분할) 오프라인 루프. RafikX는 대화형이므로
  **온라인 순차 trial** 방식으로 번역:
  - held-in 대응 = trial 중 타깃 실패 시그니처 재발 여부 (Δ_in ≥ 0)
  - held-out 대응 = trial 중 전체 성공률이 기준선 대비 비저하 (Δ_ho ≥ 0)
  - 논문의 K개 병렬 후보 평가 → 후보 K개 생성 후 순차 trial (동시에 활성
    하네스는 하나뿐이므로)
- failure signature φ=(c,q,m): c는 AgentOutcome에서 결정적 추출,
  m은 고정 어휘(controlled vocabulary)에서 소형 모델이 선택 → 논문의
  "정확 일치 클러스터링" 유지
- editable surfaces = 논문 Figure 3의 build_* 함수 대응:
  bootstrap/execution/verification/failure_recovery 지시문 + runtime_policy

## 체크리스트

- [x] 논문 31p 정독·분석
- [x] RafikX 구조 파악 (engine 등록 경로, run_pipeline, lessons와의 차이)
- [x] src/self_harness.rs 신규 모듈 (상태·마이닝·제안·검증·주입)
- [x] src/db.rs: sh_episodes / sh_evidence / sh_candidates 테이블+메서드
- [x] src/config.rs: [self_harness] 설정 (임계값 외부화)
- [x] src/harness.rs: run_pipeline에 engine_self 통합 + observe 훅
      (훅은 run_pipeline 끝 — TUI/CLI/텔레그램 3경로 모두 커버)
- [x] src/chat.rs: /engine self 등록 + 상태 표시 + 테스트
- [x] src/api.rs: set_engine note + boot normalizer에 self 추가
- [x] src/lib.rs: mod self_harness
- [x] README.md 엔진 목록 갱신
- [x] 검증: cargo check → cargo test (129 passed) → cargo build --release

## 리뷰 (2026-08-25)

- 신규: `agent-harness/src/self_harness.rs` — 하네스 상태(h_t) 로드/저장(원자적),
  φ=(c,q,m) 추출(c 결정적 + q,m 소형모델·고정어휘), proposer 호출(K후보,
  최소성·no-op 거부 검증), trial 판정(재발 0 && 성공률 비저하 → promote,
  version+1·lineage 기록), /engine 상태 표시.
- rusqlite Connection이 !Send라 observe를 (await 추론) → (동기 DB) →
  (await proposer) → (동기 DB 저장) 4단계로 분리 — lessons.rs와 같은 패턴.
- runtime_policy.max_iterations_override는 메인 에이전트 루프에만 적용,
  verify 수리 루프는 기존 예산 유지 (의도된 선택).
- denied 에피소드는 관찰 제외 — 사용자 판단이지 하네스 실패가 아님
  (논문 3.3 addressability 기준).
- lessons(단문 교훈 주입)와 별개 레이어로 공존: lessons는 응답 전략 메모리,
  self-harness는 선언된 surface의 검증된 상태 전이.
- 데스크탑 UI 라디오 버튼(index.html)은 미적용 — 터미널 요구사항 범위 밖.
  참고: index.html:602에 기존 stale allowlist 버그(dk/pi도 미표시) 존재.
- 설치본(~/.cargo/bin/rafikx)은 구버전 — `cargo install --path agent-harness`
  로 재설치해야 /engine self가 반영됨 (작업 트리의 다른 미커밋 변경도 함께
  들어가므로 사용자 판단에 맡김).

## 실전 테스트 (2026-08-25 저녁, minimax-m3)

풀 사이클 실증 완료: verify_fail 2회(실제 cargo check 실패) →
`verify_fail|contributing|other` 클러스터 support 2 → minimax-m3 proposer가
후보 3개 생성(각각 다른 surface) → 후보 #1 trial → 성공 3에피소드(재발 0,
성공률 78%→100%) → **v0→v1 승격**, lineage 기록, 나머지 후보 stale.

테스트 중 발견·수정:
- [x] 단발 CLI(rafikx agent) 종료 시 백그라운드 관찰 abort로 실패 채굴 유실
      (실측 확인) → `flush_observations` 추가, main.rs cmd_ask에서 대기
- [x] 소형 모델이 note를 비워 보내면 증상 노트가 빈 채 proposer에 감
      → 트레이스 오류 라인 폴백
- [x] pick_single이 opencode_zen/minimax-m3(만료 키 401)를 먼저 잡고
      fallback에 minimax가 없어 전멸 → config.toml fallback 맨 앞에 minimax 추가
- [x] cargo test 129개 통과, 설치본(~/.cargo/bin, 이미 0.6.1) 수정 반영 재설치

남은 관찰(수정 안 함):
- minimax는 verify 되먹임("고쳐라")과 사용자 지시("고치지 마라") 충돌 시
  비일관(1회차 지시 준수→fail, 2회차 되먹임 우선→보호 파일 수정). 승격된 v1
  지시문(표적 검증 우선)이 이런 충돌 자체를 줄이는 방향.
- 기존 lessons::maybe_spawn도 같은 단발 CLI 유실 문제 보유 (기존 동작, 미수정)
- chat 단위테스트가 실제 config를 대상으로 실행됨 — 병렬 실행 시 engine 값이
  테스트 값으로 남을 수 있음 (이번에 실제 발생, 기존 테스트 설계)
