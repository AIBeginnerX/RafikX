//! GitHub 릴리스 확인 — 새 버전이 있으면 업그레이드 안내와 핵심 변경사항을 보여준다.

use anyhow::{Result, anyhow};
use serde::Deserialize;

mod install;

pub const REPO_API: &str = "https://api.github.com/repos/AIBeginnerX/RafikX/releases/latest";
const GIT_URL: &str = "https://github.com/AIBeginnerX/RafikX.git";

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub name: String,
    pub notes: String,
    pub url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
}

/// REPO_API 에서 저장소 소유자(계정명)를 추출한다.
pub(super) fn repo_owner() -> Option<&'static str> {
    REPO_API.split("/repos/").nth(1)?.split('/').next()
}

/// 저장소 소유자 계정의 PAT 를 gh 에서 직접 얻는다.
/// 다른(활성) 계정으로 로그인돼 있어도 비공개 소유자 저장소 조회가 가능하도록 한다.
pub(super) fn owner_token() -> Option<String> {
    let out = std::process::Command::new("gh")
        .args(["auth", "token", "--user", repo_owner()?])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// git 하위명령에 소유자 자격 인라인 credential helper 를 건다.
/// 기존 helper(osxkeychain/gh 등)를 끄고 env 의 토큰만 쓴다.
/// 토큰은 argv 가 아니라 env 로만 흘러 프로세스 목록에 노출되지 않는다.
fn apply_git_credentials(cmd: &mut std::process::Command) {
    let Some(token) = owner_token() else {
        return;
    };
    cmd.args([
        "-c",
        "credential.helper=",
        "-c",
        r#"credential.helper=!f(){ printf 'protocol=https\nhost=github.com\nusername=%s\npassword=%s\n\n' "$GIT_UPD_USER" "$GIT_UPD_TOKEN"; };f"#,
    ]);
    cmd.env(GIT_UPD_USER_ENV, repo_owner().unwrap_or_default());
    cmd.env(GIT_UPD_TOKEN_ENV, token);
}

/// git 프로세스에 주입할 자격증명 환경변수 이름. install 쪽 스크립트와 공유한다.
pub(super) const GIT_UPD_USER_ENV: &str = "GIT_UPD_USER";
pub(super) const GIT_UPD_TOKEN_ENV: &str = "GIT_UPD_TOKEN";

/// 최신 릴리스를 조회한다 (동기).
/// 비공개 저장소 대응: 소유자 계정 토큰으로 gh CLI releases/latest 를 시도하고,
/// 없으면 같은 토큰을 넣어 `git ls-remote --tags` 에서 최신 semver 태그를 고른다.
pub fn latest_release() -> Result<Release> {
    use std::process::Command;
    let mut api_cmd = Command::new("gh");
    api_cmd.args(["api", REPO_API]);
    if let Some(token) = owner_token() {
        // 활성 계정과 무관하게 소유자 자격으로 조회한다.
        api_cmd.env("GH_TOKEN", token);
    }
    if let Ok(out) = api_cmd.output()
        && out.status.success()
        && let Ok(gh) = serde_json::from_slice::<GhRelease>(&out.stdout)
    {
        return Ok(Release {
            tag: gh.tag_name,
            name: gh.name.unwrap_or_default(),
            notes: gh.body.unwrap_or_default(),
            url: gh.html_url,
        });
    }
    let mut cmd = Command::new("git");
    cmd.arg("ls-remote");
    apply_git_credentials(&mut cmd);
    let out = cmd
        .args(["--tags", GIT_URL])
        .output()
        .map_err(|e| anyhow!("git ls-remote 실패: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "태그 조회 실패: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let best = text
        .lines()
        .filter_map(|l| l.rsplit('/').next())
        .filter_map(|t| t.parse::<SemverKey>().ok().map(|k| (t.to_string(), k)))
        .max_by(|a, b| a.1.cmp(&b.1));
    let Some((tag, _)) = best else {
        return Err(anyhow!("태그가 없습니다"));
    };
    Ok(Release {
        tag,
        name: String::new(),
        notes:
            "공개 릴리스 노트가 없습니다. 전체 변경사항은 https://github.com/AIBeginnerX/RafikX/blob/master/README.md".into(),
        url: "https://github.com/AIBeginnerX/RafikX/releases".into(),
    })
}

/// vX.Y.Z 태그 정렬용 키.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SemverKey(Vec<u64>);

impl std::str::FromStr for SemverKey {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let nums: Vec<u64> = s
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .split('.')
            .take(3)
            .map(|p| {
                p.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .filter(|p| !p.is_empty())
            .map(|p| p.parse::<u64>())
            .collect::<Result<_, _>>()?;
        Ok(SemverKey(nums))
    }
}

/// v 접두사를 제거하고 숫자 3구간(major.minor.patch)으로 비교한다. 최신이 더 크면 true.
pub fn is_newer(latest_tag: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.trim()
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .split('.')
            .map(|p| {
                p.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .filter_map(|p| p.parse::<u64>().ok())
            .collect()
    };
    let l = parse(latest_tag);
    let c = parse(current);
    for i in 0..3 {
        let lv = l.get(i).copied().unwrap_or(0);
        let cv = c.get(i).copied().unwrap_or(0);
        if lv != cv {
            return lv > cv;
        }
    }
    false
}

/// 릴리스 노트에서 핵심 줄만 뽑는다. 마크다운 헤더/빈 줄을 걷어내고 최대 max_lines 줄.
pub fn summarize_notes(notes: &str, max_lines: usize) -> Vec<String> {
    notes
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with('#'))
        .filter(|l| !l.starts_with("<!--"))
        .map(|l| {
            // 목록 불릿 정리
            l.trim_start_matches(['-', '*', '+']).trim().to_string()
        })
        .filter(|l| !l.is_empty())
        .take(max_lines)
        .collect()
}

static LAST_TAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
/// TUI 에서 U 키로 요청하면 기록하고, 에이전트 종료 후 main 이 소비한다.
static UPDATE_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request_update() {
    UPDATE_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn take_update_request() -> bool {
    UPDATE_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// TUI 가 U 키 동작에 쓰도록 감지된 최신 태그를 저장해 둔다.
pub fn last_seen_tag() -> Option<String> {
    LAST_TAG.get().cloned()
}

pub fn upgrade_command() -> &'static str {
    "rafikx update"
}

/// 업그레이드 안내 문장을 만든다. 새 버전이 아니면 None.
pub fn upgrade_notice(release: &Release, current: &str) -> Option<String> {
    if !is_newer(&release.tag, current) {
        return None;
    }
    let _ = LAST_TAG.set(release.tag.clone());
    let mut out = vec![format!(
        "새 버전 {} 이 있습니다 (현재 v{current})",
        release.tag
    )];
    let summary = summarize_notes(&release.notes, 6);
    if !summary.is_empty() {
        out.push("핵심 변경사항:".into());
        for s in &summary {
            out.push(format!("· {s}"));
        }
    }
    if !release.url.is_empty() {
        out.push(release.url.clone());
    }
    out.push("업그레이드 명령어: RafikX update (터미널 입력: rafikx update)".into());
    out.push(
        "지금 업그레이드하려면 U 키를 누르세요 (에이전트가 종료되며 업데이트가 실행됩니다).".into(),
    );
    Some(out.join("\n"))
}

/// rafikx update 서브커맨드 본체 — 에이전트 밖에서 단독 실행된다.
/// GitHub 확인 → 요약 출력 → 정확한 릴리스 태그를 임시 체크아웃한 뒤 cargo install.
pub fn run_update_flow() -> anyhow::Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("현재 버전 v{current} — GitHub 확인 중…");
    let rel = latest_release()?;
    if !is_newer(&rel.tag, current) {
        println!(
            "최신 버전을 사용 중입니다 (설치 v{current} · 공개 최신 {}).",
            rel.tag
        );
        return Ok(());
    }
    println!("새 버전 {} 이 있습니다.", rel.tag);
    for line in summarize_notes(&rel.notes, 8) {
        println!("· {line}");
    }
    println!();
    print!("v{current} → {} 업그레이드를 진행할까요? [Y/n] ", rel.tag);
    use std::io::Write as _;
    std::io::stdout().flush().ok();
    let mut ans = String::new();
    std::io::stdin().read_line(&mut ans)?;
    let ans = ans.trim().to_lowercase();
    if !ans.is_empty() && ans != "y" && ans != "yes" {
        println!("취소했습니다. 나중에 `rafikx update` 로 다시 진행하세요.");
        return Ok(());
    }
    install::perform_install(&rel.tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_owner_parsed_from_api_url() {
        assert_eq!(repo_owner(), Some("AIBeginnerX"));
    }

    #[test]
    fn git_credentials_go_through_env_not_argv() {
        let mut cmd = std::process::Command::new("git");
        apply_git_credentials(&mut cmd);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        match owner_token() {
            // 토큰이 있으면: -c 플래그로 기존 helper 를 끄고 교체하되,
            // 토큰 자체는 argv 에 절대 노출되지 않고 env 이름으로만 참조된다.
            Some(token) => {
                assert!(
                    args.windows(2)
                        .any(|w| w[0] == "-c" && w[1] == "credential.helper=")
                );
                let helper = args
                    .iter()
                    .find(|a| a.starts_with("credential.helper=!f()"))
                    .expect("inline credential helper present");
                assert!(!args.iter().any(|a| a.contains(token.as_str())));
                assert!(helper.contains("$GIT_UPD_USER") && helper.contains("$GIT_UPD_TOKEN"));
            }
            // 토큰을 못 얻으면 기존 동작(ambient helper) 유지.
            None => {
                assert!(args.iter().all(|a| !a.starts_with("credential.helper")));
            }
        }
    }

    #[test]
    fn semver_compare() {
        assert!(is_newer("v0.4.0", "0.3.4"));
        assert!(is_newer("0.3.10", "0.3.9"));
        assert!(!is_newer("v0.3.4", "0.3.4"));
        assert!(!is_newer("0.3.3", "0.3.4"));
        assert!(is_newer("1.0.0-rc1", "0.9.9")); // 접두사 무시, 앞 구간 비교
    }

    #[test]
    fn note_summary_drops_headers_and_bullets() {
        let notes = "# v0.4\n\n- web_search 추가\n* apply_patch 지원\n\n일반 문단";
        assert_eq!(
            summarize_notes(notes, 5),
            vec!["web_search 추가", "apply_patch 지원", "일반 문단"]
        );
    }
}
