완료: 첫 3-OS CI가 드러낸 이전 후보들의 결함을 마저 닫았다 — Linux procps가 거부하던 scope 환경 조회 ps 인자를 BSD·procps 공용 형태로 고쳐 45개 연쇄 실패의 뿌리를 제거하고, Windows cmd와 충돌하던 bash 계약 테스트 2개를 이식했다.
검증: macOS default 전체 746개·no-default 전체 676개와 게임 E2E, 그리고 실제 Linux 컨테이너(rust:1.96)의 process_tree 23·bash 9·verify 12·packer 15·quality 23이 모두 통과했고 신구 ps 출력의 macOS 동등성을 대조했다.
다음: 이 문서를 포함한 정확한 최종 CLI SHA에 PTY 재캡처와 GPT-5.6 Sol 6개 독립 레인을 전부 PASS로 묶은 뒤 push·CI·v1.1.9 태그·GitHub Release·설치본 update를 완료한다. 데스크탑은 제외한다.
