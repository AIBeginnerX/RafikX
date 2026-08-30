완료: CLI 정리가 admission 대기 전에 retained-marker 자손을 정지시키고 ScopedProcess가 Bash·브라우저·품질 명령의 취소·조기 오류를 소유하게 했다.
검증: process-tree 20개, 실제 동기 Bash 취소와 품질 명령 abort 회귀, default·minimal cargo check와 rustfmt·git diff 검사가 통과했다.
다음: 병렬 lifecycle·검증 후 최종 답변·TUI Esc 경계를 마무리하고 기본·최소 전체 테스트와 실제 브라우저·PTY 검증을 진행한다. 데스크탑은 제외한다.
