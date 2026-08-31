완료: 세 번째 CI에서 ubuntu 전체가 처음 통과했고, 남은 러너 속도 기인 타임아웃을 닫았다 — 브라우저 준비 프로브 절대 상한을 상수화해 60초로, 테스트 픽스처의 데몬 준비·소멸 폴링 12곳을 30초로 올리되 계약 검증 대기는 그대로 뒀다.
검증: macOS default 전체 746개·no-default 전체 676개와 게임 E2E·Clippy·release, Linux 컨테이너 process_tree 23개가 통과했고 성공 경로 스위트 시간은 상향 전후 노이즈 수준으로 동일했다.
다음: 이 문서를 포함한 정확한 최종 CLI SHA에 PTY 재캡처와 6개 독립 레인을 전부 PASS로 묶은 뒤 push·CI·v1.1.9 태그·GitHub Release·설치본 update를 완료한다. 데스크탑은 제외한다.
