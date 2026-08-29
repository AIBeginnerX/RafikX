---
name: api-design-checklist
description: API·설계 리뷰(S7) 체크리스트 — 최소 표면적·오용 방지·관례 일관성. API 리뷰어가 로드.
---
# API·설계 리뷰 체크리스트

trigger: 공개 함수·모듈·HTTP API·CLI 플래그 설계·리뷰 시.

## rules

- 최소 표면적(YAGNI): 요청에 없는 추상화·옵션·일반화를 추가하지 않는다.
- 오용 어려운 설계: 타입·이름·기본값으로 실수를 막는다.
- 기존 저장소 관례와 일관 — 두 번째 관례를 만들지 않는다.

## checklist

1. 시그니처가 의도를 설명하는가 (인자 순서·이름·타입)
2. 에러는 복구 가능한 형태로 반환되는가 (panic 남발 금지)
3. 네이밍이 저장소 어휘와 일치하는가
4. 버전·호환성 — 공개 API 변경은 breaking 여부 명시
5. 문서 주석에 예시·오류 케이스 포함

## good/bad

- good: `fn open(path: &Path) -> Result<File>` — 실패가 타입으로 드러남
- bad: `fn open(path: &Path) -> File` — 실패 시 panic, 호출자가 통제 불가

## sources

- Google AIP (google.aip.dev) — API 설계 표준 패턴
- Stripe API 문서 — 리소스 설계·오류 형식 관례
- 규칙 요약: RafikX 스킬 수확 파이프라인 (패턴 학습만)
