# RafikX v1.1.0 — 자율 완수 릴리스

v1.0.x의 하니스 v2 위에, omo(oh-my-openagent)·paperthin 철학을 RafikX 네이티브로 재설계한
"목표만 주면 증거가 모일 때까지 완수하는" 자율 실행 계층 전체를 담은 릴리스입니다.

## 헤드라인

- **`/ulw` 자율 완수 루프** — 목표만 주면 계획→실행→검증을 완료 기준의 증거가 모일 때까지 반복합니다. 산출물은 `.omo/ulw/<id>/` (goal·plan·evidence·state·report) 에 파일로 남고, 진전이 없으면 재촉(최대 3회), 그래도 안 되면 조용한 실패 대신 `blocked` 보고. `/ulw-resume` 으로 재부팅 후에도 미완료 기준부터 재개. CLI·TUI·데스크탑·텔레그램(버튼 승인) 전 표면 지원.
- **품질 게이트** — todo 완료만으론 끝나지 않습니다. 코드 변경이 있으면 ulw 가 빌드·테스트를 **직접 실행**해 통과해야 done. 실패 시 실패 로그를 첨부해 디버그 재촉, 같은 실패 3회 연속이면 독립 리뷰 필요로 중단.
- **Hashline 해시 앵커 편집** — `read_file` 출력의 `N#HASH` 태그로 편집 위치를 검증합니다. 파일이 바뀌었으면 쓰기 전에 원자 거부. edit_file·multi_edit·apply_patch 모두 지원.
- **apply_patch 문맥 앵커** — 중복 블록을 앞 문맥 줄로 구분하고 빈 삭제 블록을 거부합니다 (Inspector 지표가 발견한 0/18 실패율의 근본 수정).
- **팩트 메모리** — 대화 중 발견한 스택·선호·관습을 `remember` 도구로 즉시 기록하고 다음 세션부터 자동 주입. `recall`·`/facts`·`/forget`·`rafikx facts` 지원. **`rafikx mcp-serve` 로 MCP 서버로 노출** — 다른 도구·에이전트와 기억을 공유합니다.
- **IntentGate** — 분류 규칙에 확신도를 추가해 경계값만 소형 모델이 재판정(3초 상한). Contract 계획은 [질문] 절로 진짜 분기점만 묻습니다. 인용(따옴표·백틱) 속 동사는 지시로 오인하지 않습니다.
- **탐색·리서치 레인** — explorer(읽기 전용, 승인 프롬프트 없음)·researcher(웹 조사, 출처 표기)가 일의 성격으로 자동 배정됩니다. 바인딩 단계에서 mutation 도구를 구조적으로 차단.
- **폴 fallback 아키텍트** — 모델이 설계 불확실성으로 거부하면 아키텍트 레인(도구 0개)이 판단하고 실행을 1회 재개합니다. 판단은 facts 에 기록돼 재거부를 예방합니다.
- **콤보 폴 fallback** — `[combos.<이름>] chain = ["provider:model", …]` 가상 모델에 폴 fallback 체인. 전환 시 배지 표시. `/quota` 로 계정별 리밋 상태·오늘 사용량 확인.
- **이상 감지 감시자** — 편집 성공률 급락·실행 성공률 급락·ulw 중단·프로바이더 429 폭풍을 15분 주기로 코드 계산 감시, 전이 시 텔레그램 즉시 알림.
- **규칙 주입 + `/init-deep`** — AGENTS.md·.rafikx/rules/*.md 를 매 요청 자동 주입하고, 없는 프로젝트에는 결정적 초안 생성기가 디렉터리별 AGENTS.md 를 만듭니다 (기존 파일 보호, diff 제안만).
- **Inspector 강화** — ulw 이력(전체 워크스페이스 합산)과 편집 지표(도구별 성공률·실패 유형)를 리포트에 자동 인용.
- **팁 시스템** — 시작 화면 팁 1줄 + `/tips`·`/tip <id>`(구현 코드 발췌 포함). `/tips off` 영속.
- **Python LSP 완성** — pyright/pylsp 자동 탐색, push/pull 이중 진단 모델, workspaceFolders 정확화, 언어별 설치 안내.
- **paperthin 스킬 지원 문서화** — readchk·modelchk·sip·re0-git·catchup·nba 를 `~/.rafikx/skills/` 에 둬 즉시 사용 가능.

## 수정·정리

- 비대화형 stdin EOF에서 승인 프롬프트 무한 스핀 수정 (거부로 안전 종료)
- 단발 `ask` 경로가 config `approval="yolo"`·레인 라우팅을 무시하던 불일치 수정
- harness.rs 4,915라인 → 6개 모듈 분리 (기능 변경 0, 외부 경로 불변)
- 교차-표면 정책 상수를 ui_policy.rs 로 단일 원천화 (데스크탑 JS는 명령으로 수신)
- 문서 전면 동기화 (README·SPEC 배너·CHANGELOG-작업기록.md 분리) + 레포 5곳 AGENTS.md 도그푸딩

## 검증

- 테스트 **345개 전부 통과** (v1.0.1 시점 234개 대비 +111)
- 실모델 종단 검증 17개 시나리오: /ulw 완주·디버그 루프·resume·콤보 전환·메모리 영속·규칙 준수·LSP 진단·경로 jail·레인 배정 등
- 데스크탑 UI Playwright QA 10면 통과 (실행·승인·설정·모바일 375px·라이트)
- 성능: 바이너리 7.2MB · RAM 14.3MB · facts ~0ms (docs/PERFORMANCE.md)
- 알려진 미달: 완전 냉 cold start 610ms (웜은 ~0ms), FTS 검색 50ms 경계 — docs/PERFORMANCE.md 참조

## 업그레이드

기존 설치는 `rafikx update` 로 최신 태그를 따라갑니다. 신규는 README의 한 줄 설치를 사용하세요.
