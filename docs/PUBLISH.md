# 릴리스 (관리자용)

기본 원격 저장소는 `https://github.com/AIBeginnerX/RafikX.git`, 기본 브랜치는 `master` 입니다.

1. CLI 전용 릴리스는 `agent-harness/Cargo.toml` 버전을 확인하고 이전 태그부터 후보까지 `desktop/` 변경이 없는지 확인합니다. 데스크탑 버전 정렬·빌드·배포는 별도 릴리스에서 수행합니다.
2. `cd agent-harness && cargo check --locked && cargo test --locked && cargo test --locked --no-default-features` 를 통과시키고 비밀 파일·빌드 산출물·`.omo/` 가 커밋에 없는지 확인합니다.
3. 커밋된 정확한 SHA를 대상으로 독립 코드·보안·아키텍처·계약·수동 QA 검증이 모두 통과했는지 확인합니다. 추적 파일이 바뀌면 새 SHA에서 다섯 검증을 다시 수행합니다.
4. 검증된 SHA를 `git push origin master` 로 푸시합니다.
5. 푸시한 정확한 SHA의 Rust 1.96 기본·최소 기능 검사와 Ubuntu·Windows·macOS CLI 테스트가 모두 성공할 때까지 기다립니다.
6. CI가 통과한 같은 SHA에 annotated `vX.Y.Z` 태그를 만들고 GitHub Release를 게시합니다.
7. `gh api repos/AIBeginnerX/RafikX/releases/latest` 와 원격 peeled 태그 SHA가 일치하는지 확인한 뒤, 이전 설치본에서 `rafikx update`를 실행해 설치 버전이 새 버전인지 확인합니다.
