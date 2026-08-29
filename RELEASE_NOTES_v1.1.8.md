# RafikX v1.1.8 — 핫픽스: 브라우저 스모크 게이트 (게이트 자기 성장)

실사용 실패("슈퍼마리오 게임이 실행되지 않는다")를 분석해, 놓친 결함을 잡는 게이트를 추가한 핫픽스입니다.

## 사건과 원인

- Executor(MiniMax-M3)가 만든 HTML/JS 게임(1056줄)이 **첫 프레임에서 Uncaught ReferenceError** 로 사망 — `game.js:230` 에 카메라 코드 고치고 남긴 잔재 변수가 있었습니다.
- 놓친 이유 3겹: ① HTML/JS 프로젝트는 자동 검증 명령이 없어 **검증이 생략**됨 ② 런타임 오류를 잡는 게이트 부재(구문 검사는 통과·eslint 미설치·스캐너는 보안 전용) ③ fresh-context Claude 리뷰도 1056줄에서 한 줄 런타임 버그 놓침.

## 수정

- **S4-smoke 브라우저 스모크 게이트** — headless Chrome 으로 엔트리 HTML 을 로드해 콘솔의 Uncaught/Reference/TypeError 를 감지, 있으면 게이트 실패. (실측: 결함 복원본 감지 exit 1, 수정본 통과)
- **S3-syntax** — 변경 .js 파일 node --check 구문 게이트.
- **검증 생략 경로 폐쇄** — 빌드 명령 자동 감지가 없는 프로젝트(HTML/JS 등)도 변경 파일이 있으면 품질 게이트를 실행합니다. 실패 시 status=fail.
- 사용자 게임 자체도 수정 완료 (잔재 1행 삭제, 콘솔 오류 0건).

## 검증

- 테스트 **399개 전부 통과** (+3)
- 재발 방지 실증: 동일 결함 재주입 → `✗ [S5-browser-error] camTarget is not defined` 감지

## 업그레이드

기존 설치는 `rafikx update` 로 최신 태그를 따라갑니다.
