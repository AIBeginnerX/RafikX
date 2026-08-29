# RafikX v1.1.7 — 코드 품질 엔진

어떤 언어로 작업하든 품질 기준을 시스템이 강제하는 **코드 생성 품질 엔진**을 추가한 릴리스입니다. 모든 판단은 코드 근거로 수행했고, 레드팀 10 시나리오 중 7개를 실제 실행으로 차단 증명했습니다.

## 품질 파이프라인 (S0~S8)

- **S0 언어 감지·프로파일** — 8언어(rust·python·typescript·javascript·go·shell·sql·unknown) 자동 감지, 언어별 표준 도구 체계를 데이터화한 프로파일. 이번 변경 파일의 확장자를 우선 판정.
- **S1 알고리즘 설계 노트** — 사소하지 않은 로직은 구현 전 후보 비교(≥2)·시간/공간 복잡도·불변식·정확성 논증(≥2줄)·property 테스트 계획을 강제. 누락 항목 전건 일괄 보고.
- **S3 기계 게이트** — 언어별 최엄격 명령(Rust: `cargo fmt --check` + `clippy -D warnings` + `cargo test`, Python: ruff+mypy --strict, TS: eslint(max 0)+tsc --strict 등). 도구 미설치는 기록 후 내장 스캐너로 최소 보장.
- **S5 보안 게이트 (항상 강제)** — 내장 스캐너 4종: 시크릿 하드코딩(env 조회·테스트 파일 오탐 제외)·SQL 문자열 연결 인젝션·에러 메시지 스택 노출·eval/exec. 의존성 감사 도구 연결. **감사 도구가 없어도 게이트는 fail** — 감사 없이는 통과 없음.
- **S7 리뷰 위원회** — 5개 독립 관점(정확성·보안·성능·가독성·API설계)을 각각 fresh-context 리뷰어가 순차 심사. 그룹별 판정 줄 누락은 실패 처리. 기본 on, `[harness] review_committee` 로 토글.
- **S8 디자인 게이트** — UI 산출물은 렌더링 검사로만 판정. TUI는 tmux 캡처 절차 제공.
- **간결함 게이트** — 3줄 이상 동일 블록 중복 자동 감지.

## 새 명령어

- `rafikx quality-gate <files>` — S0→S3→S5 게이트 실행, exit code 판정
- `rafikx spec-freeze / verify-plan / run-plan / plan-rollback / plan-report / verify-task / calibrate` (v1.1.6 이후 누적)

## 스킬 3종 수확

`security-checklist`(OWASP Cheat Sheets·ASVS 기반 10항목)·`design-checklist`(shadcn/ui·Radix·Tailwind 토큰 체계 기반 8항목 + TUI 렌더링 검증 절차)·`api-design-checklist`(Google AIP·Stripe 관례 기반 5항목). 모두 **패턴 학습만, 코드 차용 없음** — 출처·라이선스 기록 포함.

## 보안

- install.sh 에 cargo-audit best-effort 설치 추가 (본기계 0.22.2 설치 완료)
- 미설치 환경에서도 감사 없는 통과는 차단됩니다 (엄격 해석)

## 검증

- 테스트 **396개 전부 통과** (v1.1.6 대비 +17)
- 레드팀 10 시나리오: 7개 실증 차단(SQL 인젝션·시크릿·중복·비관용·스택 노출·무수정 완료·AC 누락), 3개 설계 추적(자동화 인프라는 M7)
- 실측: Python 인젝션/시크릿/스택 노출 파일 → 게이트 실패 exit 1, Rust 중복+unwrap → idiom·중복 감지, clippy -D warnings 게이트 동작

## 업그레이드

기존 설치는 `rafikx update` 로 최신 태그를 따라갑니다.
