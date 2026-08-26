# 릴리스 (관리자용)

기본 원격 저장소는 `https://github.com/AIBeginnerX/RafikX.git`, 기본 브랜치는 `master` 입니다.

1. 버전이 `agent-harness/Cargo.toml`, `desktop/src-tauri/Cargo.toml`, `desktop/src-tauri/tauri.conf.json` 에서 일치하는지 확인합니다.
2. 전체 테스트를 통과시키고 비밀 파일이 커밋에 없는지 확인합니다.
3. 변경을 커밋한 뒤 `git push origin master` 합니다.
4. 같은 커밋에 `vX.Y.Z` 태그와 GitHub Release를 생성합니다.
5. `gh api repos/AIBeginnerX/RafikX/releases/latest` 가 새 태그를 반환하는지 확인합니다.
6. 이전 버전에서 `rafikx update`를 실행해 설치 버전과 공개 최신 태그가 일치하는지 확인합니다.
