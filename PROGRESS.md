완료: 취소와 즉시 최종 답변 공개를 lifecycle 전이 커밋으로 선형화하고, 시도 예산 소진·폴백 전환이 관측된 공급자 오류를 가리던 회귀를 고쳐 PTY 실패 화면과 통합 계약을 새 표면화 계약으로 복원했다.
검증: default 전체 746개, no-default 전체 676개, 게임 E2E 1개, Clippy·release·version·help와 실제 PTY 9상태·release 바이너리 120→60→120 live resize가 모두 통과했다.
다음: 이 문서를 포함한 정확한 최종 CLI SHA에 GPT-5.6 Sol 6개 독립 레인을 전부 PASS로 묶은 뒤 push·CI·v1.1.9 태그·GitHub Release·설치본 update를 완료한다. 데스크탑은 제외한다.
