# 완료 순간 화면 점프("결과 보이다 멈춤") 수정 (2026-08-26 오전)

증상: 0.7.2 에서 작업 완료 직후 결과가 사라진 듯 멈춰 보임.
진단: 프로세스 유휴·턴 ok — hang 아님. finish_turn 의 scroll=u16::MAX ·
follow=false 가 원인: transcript.clear() 시절(화면에 답변만 남음)의 잔재로,
누적 스크롤백에서는 완료 순간 화면을 과거 대화 꼭대기로 점프시켰다.

- [x] finish_turn: scroll=0 · follow=true — 완료 시 최신 결과(맨 아래) 표시
- [x] 보너스: 스트리밍 중 마지막 답변은 경량 렌더(render_streaming_tail —
      파싱·syntect 생략, 완료 시 풀 렌더로 캐시) — 긴 코딩 답변에서 청크마다
      전체 재하이라이팅하던 CPU 폭증 제거
- [x] 129 테스트, 실전 태스크 ok, 설치본 갱신 (실행 중 사용자 프로세스는
      건드리지 않음 — 재시작 시 적용)

---

# 승인 UI 방향키 + 권한무시(YOLO) (2026-08-26 오전)

- [x] 도구 승인 인라인 UI: ←/→(Tab)로 Yes/No/Always 이동, 현재 선택은
      ▸ 화살표+역상 하이라이트, Enter 확정, y/n/a·Esc 단축키 유지,
      푸터에 "←→ 이동 · Enter 확정" 힌트 표시
- [x] /yolo 권한무시: 토글(on/off 인자 지원) → session.yes + config
      [general] approval="yolo"/"ask" 영속 → 다음 실행부터 자동 승인 시작.
      죽어 있던 approval 설정 필드를 실제로 연결 (open_session 에서 반영)
- [x] 푸터에 YOLO 배지(적색 역상) 상시 표시 — 자동 승인 중임을 항상 인지
- [x] 129 테스트 통과, /yolo 왕복·영속 스모크, 설치본 갱신
- 안전장치 유지: 텔레그램 원격에서는 effective_yes 가 자동 승인을 강제 차단

---

# 턴 종료 후 멈춤(부하) 수정 (2026-08-26 오전)

증상: 코딩 완료 직후 UI 가 굳는 느낌. 원인 2건 실측:
1) 턴마다 자동 저장이 TUI 메인 루프에서 동기 실행 — 세션 JSON 130KB 실측,
   직렬화+SQLite 쓰기 동안 이벤트 루프 정지.
2) pi 스크롤백 개편으로 대화가 누적되는데, draw_transcript 가 매 프레임
   전체 트랜스크립트를 마크다운 파싱+syntect 하이라이트 (캐시 없음) —
   코딩 답변(코드블록)이 쌓일수록 프레임당 비용 증가.

- [x] 자동 저장을 chat::run_turn(턴 백그라운드 태스크) 내부로 이동 —
      메인 루프 무정지, CLI REPL 도 매 턴 저장으로 일관 (실측: 세션 수 증가)
- [x] 엔트리 단위 렌더 캐시 — render_entry 로 추출(파싱·하이라이트·줄바꿈
      포함), (kind,text,width,theme) 해시로 App.render_cache(RefCell) 재사용.
      확정된 과거 턴은 재파싱 없음 → 프레임 비용 O(전체 파싱) → O(보이는 행)
- [x] 129 테스트 통과, TUI 실구동 CPU 0.0%, 설치본 갱신
- 참고: 스모크 중 pgrep 오인으로 사용자의 구버전 rafikx 프로세스를 종료함
      (자동 저장 덕에 대화 보존; 새 버전 적용에 재시작 필요했음)

---

# tool-call 텍스트 누출 수정 (2026-08-26 오전)

증상: "마크다운 파일을 업그레이드 하면 좋지 않을까?" 에 모델이
`]<]minimax[>[<tool_call> {"name":"run_command"...` 원시 텍스트를 뱉고 종료.

원인 사슬: 분류기가 simple 로 오분류(.md 확장자·"업그레이드"·"파일" 키워드
부재) → quick 프로파일 = 도구 0개 → 도구 전제 시스템 프롬프트를 받은 모델이
tool call 문법을 텍스트로 흉내(가짜 도구 run_command) → RafikX 는 텍스트라
최종 답변으로 화면에 출력하고 정상 종료(사용자에겐 '멈춤'으로 보임).

- [x] 분류기 보강: exts 에 .md/.yml/.yaml, dev 키워드에 업그레이드·만들어·
      생성해·작성해·적용해, medium 키워드에 파일·마크다운·폴더·디렉토리·
      워크스페이스
- [x] 자동 승격 방어: 도구 없는 바인딩의 응답에서 leaked_tool_call
      (<tool_call>·]<]·name+arguments) 감지 시 오염 응답을 걷어내고 coder 로
      1회 승격 재실행 (run_pipeline, Box::pin 재귀 — 승격 후 tools 비지
      않으므로 1회 보장)
- [x] 검증: 단위 2개 추가(129 통과) + 원래 질문 실전 재현 → dev/coder,
      status=ok, 누출 없음. 설치본 갱신.

---

# pi 미니멀 UI/UX 대개편 (2026-08-26 오전)

earendil-works/pi 와 code-yeongyu/oh-my-openagent 분석 후, pi 의 핵심 장점
(마찰 없는 흐름: 팝업 없음·노이즈 없음·항상 저장·인라인 UI)을 이식.
사전에 Explore 에이전트로 RafikX UX 전수 조사(마찰 상위 10개, 파일:라인)를 떠서
전부 해소.

## 화면·스크롤백
- [x] 턴 종료 시 transcript.clear() 로 대화 전체가 사라지던 것 →
      collapse_turn_noise: 이전 대화 보존, 이번 턴 작업 노이즈만 접음
- [x] 상태 스트립+푸터 2줄 → 단일 푸터 통합 (모드·상태·모델·토큰·Todo·ctx)
      — 중복 표시(모델 2회·상태 2회·Todo 2회·스피너 2개 위상 어긋남) 해소
- [x] 도구 출력 원문 전량 투척 → 요약 한 줄 (원문은 모델에게만)
- [x] "[모델 작업] 반복 N" 매 반복 배너, "[Harness] …" 매 턴 배너, 검증 성공
      원문 출력 제거 · 슬래시 팔레트 오타 시 26개 전체 재표시 → 숨김
- [x] 버전 확인: 새 버전 있을 때만 표시 (최신/실패 시 매번 붉은 경고 제거)
- [x] 80ms 무조건 전체 리드로우 → busy 일 때만 (유휴 CPU 0.0% 실측)

## 팝업 제거 (pi: no permission popups)
- [x] 도구 승인 전체화면 팝업 삭제 → 푸터 인라인 [Yes][No][Always] + y/n/a 키
- [x] 마우스 캡처 인프라 전부 제거 — /connect 후 드래그·복사·휠이 죽던
      캡처 누수, 보이지 않는 클릭 타깃 오승인 버그 동시 해소
- [x] Always 가 턴 한정이던 것 → 세션 전체 지속 (session.yes 승격)
- [x] 슬래시 명령 Enter 2회 확인 → 1회 즉시 실행 (slash_armed 제거)

## 시작 시퀀스
- [x] 시작 경로의 ranks 동기 네트워크 대기(최대 30초) 제거 — 백그라운드
      spawn_weekly_refresh 만 유지 (중복 요청도 해소)
- [x] goal 무확인 자동 재개(켜자마자 토큰 소모·세션 덮어쓰기) → 안내 한 줄
      + /goal resume(명시 재개) · /goal clear(해제; Esc 고착 탈출구, DB
      clear_active_goal 추가)
- [x] CLI 로그인 자동 임포트 println 이 AlternateScreen 전환으로 유실 →
      TUI 에서는 침묵 · 시작 안내문은 연결 없을 때만

## 세션 (pi: auto-save · picker · -c)
- [x] 턴마다 자동 저장 (이전: 종료 시 1회 → 크래시 시 전량 유실)
- [x] /resume·/sessions 인자 없으면 세션 picker (id 손 타이핑 제거)
- [x] rafikx -c — 최근 세션 이어서 시작
- [x] /new 의 minimax-m3 하드코딩 리셋 → 마지막 사용 조합 유지

검증: cargo test 127 통과(마우스 승인 테스트 3개 제거·신규 2개), minimax
실전 태스크 ok(저소음 확인), pty TUI 유휴 CPU 0.0%, 설치본 갱신.
참고: 오늘 아침 사용자가 target 을 외장 RAID 심링크로 오프로드 →
Claude 셸은 TCC 로 외장 볼륨 차단이라 CARGO_TARGET_DIR 로 로컬 우회 빌드.
미채용(과설계 방지): OmO 구조화 메모리(facts/journal)는 기존
lessons+obsidian 과 중복이라 도입하지 않음.

---

# TUI 멈춤(CPU 96% 스핀) 원인 분석·수정 (2026-08-26 새벽)

증상: 벤치마크 작업 중 TUI 멈춤. PID 6987 이 52분 경과에 CPU 시간 51분(96%).

- [x] 원인 1 (근본): tokio::select! 의 닫힌 채널 스핀 — 버전 확인 태스크가
      upd_tx 를 drop 하면 upd_rx.recv() 가 매 반복 즉시 None → 메인 루프가
      CPU 스핀 → 런타임 포화로 스트리밍 청크 처리가 굶어 "스트림 출력 중단"
      fail 로 연쇄. 수정: None 받은 채널(upd/live/ask)은 `, if open` 가드로
      비활성화. 검증: pty 실구동 17초에 CPU 0.2% (수정 전 96%).
- [x] 원인 2 (기여): anthropic 429 retry_after=45 미존중 — 단일 계정이면
      리밋 중에도 대기 없이 재호출해 3~10초 간격 429 폭풍 (debug.log 도배).
      수정: try_accounts·stream_with_fallback 모두 리밋(>20s) 계정은 마지막
      계정이어도 건너뛰고 다음 연결로 폴백.
- [x] 멈춘 6987 TERM 정상 종료, 설치본 갱신 (0.6.3, 커밋 전 워킹트리)
- 잔여(기록만): Esc 인터럽트 시 finish_run 미호출 → runs 에 미종결 행 잔존
      (run-9). goals 는 failed/complete 라 auto-resume 위험 없음.

---

# oh-my-pi 답변 형식 + /engine provider mode + 역할 자동 배정 (2026-08-25 밤)

- [x] 'Choose next action' 제거 — tui.rs CompletionAction 메뉴·Improve 서사 전부
      삭제, Run summary 통계만 유지, 숫자 선택 로직·pending_actions 제거
- [x] oh-my-pi (github.com/can1357/oh-my-pi) system-prompt.md + default
      personality 분석 → harness.rs system_prompt() 전면 재작성:
      RFC 2119 규약 / 엔지니어링 원칙 / 증거 우선 간결 페르소나(결론 먼저·
      [INFERENCE] 표기·선택지 메뉴 금지) / 6단계 워크플로 / 전달 계약 / 안전
- [x] /engine 2단계 흐름: 엔진 선택 → Provider mode [1] Single [2] Multi
      (CLI 는 번호 입력, TUI 는 /engine single <연결> · /engine multi 안내)
- [x] /engine single <연결>: default_provider 고정 + strategy=single
      (pick_single 이 default 를 minimax-m3 특례보다 우선하도록 수정)
- [x] /engine multi: 신규 auto_assign_roles — 연결별 원격 모델 조회(12s 타임아웃,
      실패 시 등록 폴백) → ranks 점수화 → 역할별 배정(simple=최저비용,
      medium=cheap 최고점, advanced/dev=최고점+도구, verify=차순위) →
      [harness] manual_* 저장 + selection=manual + strategy=multi.
      ':batch/:free' 라우터 변형 제외.
- [x] 실측 배정: simple=minimax/M2, medium=openrouter/qwen3.7-flash,
      dev·advanced=openrouter/claude-opus-5-fast, verify=claude-opus-5
- [x] 판정 버그 수정: todo 미등록 + ok 종료를 incomplete 로 강등하던 로직 제거
      (todo 를 등록하고도 못 끝낸 경우만 미완료) — minimax 실측 재현·해소
- [x] engine_slash_end_to_end 가 실제 config 의 engine 을 오염시키던 문제 수정
      (원래 값 저장·복원) — 사용자 self 설정이 rafikx 로 되돌아가던 3번째 발생
- [x] cargo test 129 통과, 설치본 갱신, engine=self 보존 확인

---

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
    Harness는 하나뿐이므로)
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

- 신규: `agent-harness/src/self_harness.rs` — Harness 상태(h_t) 로드/저장(원자적),
  φ=(c,q,m) 추출(c 결정적 + q,m 소형모델·고정어휘), proposer 호출(K후보,
  최소성·no-op 거부 검증), trial 판정(재발 0 && 성공률 비저하 → promote,
  version+1·lineage 기록), /engine 상태 표시.
- rusqlite Connection이 !Send라 observe를 (await 추론) → (동기 DB) →
  (await proposer) → (동기 DB 저장) 4단계로 분리 — lessons.rs와 같은 패턴.
- runtime_policy.max_iterations_override는 메인 에이전트 루프에만 적용,
  verify 수리 루프는 기존 예산 유지 (의도된 선택).
- denied 에피소드는 관찰 제외 — 사용자 판단이지 Harness 실패가 아님
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

---

# .omo/ 디렉토리 정책 (2026-08-27 C1)

- [x] .omo/ 를 .gitignore에 추가 — 개인 작업 증거(plans/evidence/qa)는 레포에 올리지 않음
- [ ] F4 Phase에서 .omo/ulw/<run-id>/ 산출물을 제품 기능으로 정식 지원 (v5 기획서 참조)
