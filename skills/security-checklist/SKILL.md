---
name: security-checklist
description: 보안 게이트(S5) 체크리스트 — OWASP 기반. 코드 작성·리뷰 전 로드. 발견 항목은 경고가 아니라 fail.
---
# 시큐리티 코딩 체크리스트

trigger: 코드 작성·리뷰·보안 관련 태스크 시작 시.

## rules

- secure-by-default: 보안은 리뷰 항목이 아니라 게이트다. 아래 항목 하나라도 어기면 코드는 존재하지 않는 것으로 취급한다.
- 보안 지적에 "경고 후 진행"은 없다. 수정 또는 명시적 위험 수용 기록(사용자 승인)만 존재한다.
- 의존성은 lockfile 로 고정하고, 알려진 취약점 0개를 유지한다.

## checklist

1. 모든 외부 입력은 검증·정규화 후 사용한다.
2. 인젝션 방지: SQL 은 파라미터화, 셸 명령은 인자 배열화, 파일 경로는 정규화 후 jail 검증.
3. 출력 인코딩: HTML/JSON 응답에는 컨텍스트별 인코딩 적용 (XSS).
4. 인증·세션은 표준 라이브러리/검증된 크레이트 사용 — 자작 금지.
5. 시크릿 하드코딩 금지: 환경변수·시크릿 매니저로만. 로그에 토큰·키 원문 금지.
6. 안전한 기본값: deny-by-default, 최소 권한.
7. 직렬화/역직렬화는 신뢰 데이터만.
8. SSRF·리다이렉트: 외부 URL 은 허용 목록 검증.
9. 에러 메시지에 스택트레이스·내부 경로·버전 정보 노출 금지 — 일반 메시지 + 내부 로그 분리.
10. 의존성 감사: cargo audit / pip-audit / npm audit 통과.

## good/bad

- good: `conn.execute("SELECT .. WHERE name = ?", [name])`
- bad: `conn.execute("SELECT .. WHERE name = '" + name + "'")`
- good: `std::env::var("API_KEY")`
- bad: `const API_KEY = "sk-live-…"`

## sources

- OWASP Cheat Sheet Series (cheatsheetseries.owasp.org) — 인젝션·인증·세션·에러 처리 판
- OWASP ASVS v4 — 검증 항목 수준 기준
- 규칙 요약·한국어 번역: RafikX 스킬 수확 파이프라인 (라이선스: 문서 참조만, 코드 차용 없음)
