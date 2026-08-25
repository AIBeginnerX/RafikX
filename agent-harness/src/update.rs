//! GitHub 릴리스 확인 — 새 버전이 있으면 업그레이드 안내와 핵심 변경사항을 보여준다.

use anyhow::{Result, anyhow};
use serde::Deserialize;

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

/// 최신 릴리스를 조회한다 (동기).
/// 비공개 저장소 대응: 먼저 gh CLI 로 releases/latest 를 시도하고,
/// 없으면 git 자격증명으로 `git ls-remote --tags` 에서 최신 semver 태그를 고른다.
pub fn latest_release() -> Result<Release> {
    use std::process::Command;
    if let Ok(out) = Command::new("gh").args(["api", REPO_API]).output() {
        if out.status.success() {
            if let Ok(gh) = serde_json::from_slice::<GhRelease>(&out.stdout) {
                return Ok(Release {
                    tag: gh.tag_name,
                    name: gh.name.unwrap_or_default(),
                    notes: gh.body.unwrap_or_default(),
                    url: gh.html_url,
                });
            }
        }
    }
    let out = Command::new("git")
        .args(["ls-remote", "--tags", GIT_URL])
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
        .filter_map(|t| {
            t.parse::<SemverKey>()
                .ok()
                .map(|k| (t.to_string(), k))
        })
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
            .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
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
            .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
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

/// 표준 설치 경로(~/.rafikx-src)에서 최신 소스를 받아 설치하는 명령.
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
    out.push("지금 업그레이드하려면 U 키를 누르세요 (에이전트가 종료되며 업데이트가 실행됩니다).".into());
    Some(out.join("\n"))
}

/// rafikx update 서브커맨드 본체 — 에이전트 밖에서 단독 실행된다.
/// GitHub 확인 → 요약 출력 → ~/.rafikx-src 갱신 후 cargo install (진행 출력 그대로 스트리밍).
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
    perform_install()
}

fn perform_install() -> anyhow::Result<()> {
    use std::process::Command;
    let status = Command::new("sh")
        .arg("-c")
        .arg(install_script())
        .status()?;
    if !status.success() {
        anyhow::bail!("업그레이드 실패 (exit {})", status.code().unwrap_or(-1));
    }
    println!();
    println!("업그레이드 완료 — `rafikx` 를 다시 실행하세요.");
    Ok(())
}

fn install_script() -> &'static str {
    r#"
set -e
SRC="$HOME/.rafikx-src"
if [ -d "$SRC/.git" ]; then
  git -C "$SRC" fetch --depth 1 origin master
  git -C "$SRC" checkout -q master
  git -C "$SRC" reset -q --hard origin/master
else
  git clone --depth 1 --branch master https://github.com/AIBeginnerX/RafikX.git "$SRC"
fi
cargo install --path "$SRC/agent-harness" --locked --force
"#
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn updater_bootstraps_missing_source_clone() {
        let script = install_script();
        assert!(script.contains("git clone --depth 1"));
        assert!(script.contains("cargo install --path"));
    }
}
