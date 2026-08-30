완료: CLI가 중첩 child lifecycle을 추적하고 검증된 마지막 root 답변만 한 번 공개하며 Answering 이후 늦은 취소와 TUI Esc handoff 경쟁을 차단하게 했다.
검증: lifecycle·취소·최종 답변 회귀와 cargo check, 라이브러리 통합 실행, 실제 PTY의 Answering 첫 프레임과 Succeeded 종료 화면이 통과했다.
다음: 기본·최소 전체 테스트와 브라우저 게임 복구 E2E·실제 PTY·release build를 최종 재검증하고 문서 수치를 갱신한다. 데스크탑은 제외한다.
