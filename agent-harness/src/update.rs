//! GitHub 릴리스 확인 — 새 버전이 있으면 업그레이드 안내와 핵심 변경사항을 보여준다.

use anyhow::{Result, anyhow};
use serde::Deserialize;

pub const REPO_API: &str = "https://api.github.com/repos/AIBeginnerX/RafikX/releases/latest";
pub const REPO_TAGS_API: &str = "https://api.github.com/repos/AIBeginnerX/RafikX/tags";

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

async fn get_json(client: &reqwest::Client, url: &str) -> Result<reqwest::Response> {
    let resp = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| anyhow!("조회 실패: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {}", resp.status()));
    }
    Ok(resp)
}

/// 최신 릴리스를 조회한다. 공개 릴리스가 없으면 최신 태그로 폴백한다(변경사항 요약은 없음).
pub async fn latest_release() -> Result<Release> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("RafikX/+update-check")
        .build()?;
    match get_json(&client, REPO_API).await {
        Ok(resp) => {
            let gh: GhRelease = resp
                .json()
                .await
                .map_err(|e| anyhow!("릴리스 해석 실패: {e}"))?;
            Ok(Release {
                tag: gh.tag_name,
                name: gh.name.unwrap_or_default(),
                notes: gh.body.unwrap_or_default(),
                url: gh.html_url,
            })
        }
        Err(_) => {
            // 폴백: 태그 목록의 첫 항목 (GitHub 는 최신순으로 돌려준다)
            #[derive(Deserialize)]
            struct Tag {
                name: String,
            }
            let resp = get_json(&client, REPO_TAGS_API).await?;
            let tags: Vec<Tag> = resp
                .json()
                .await
                .map_err(|e| anyhow!("태그 해석 실패: {e}"))?;
            tags.first()
                .map(|t| Release {
                    tag: t.name.clone(),
                    name: String::new(),
                    notes: format!(
                        "공개 릴리스 노트가 없습니다. 변경사항은 https://github.com/AIBeginnerX/RafikX/compare/{}...{} 에서 볼 수 있습니다.",
                        t.name, "HEAD"
                    ),
                    url: format!("https://github.com/AIBeginnerX/RafikX/releases/tag/{}", t.name),
                })
                .ok_or_else(|| anyhow!("태그가 없습니다"))
        }
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

/// 업그레이드 안내 문장을 만든다. 새 버전이 아니면 None.
pub fn upgrade_notice(release: &Release, current: &str) -> Option<String> {
    if !is_newer(&release.tag, current) {
        return None;
    }
    let mut out = vec![format!(
        "새 버전 {} 이 있습니다 (현재 v{current}). 업그레이드: cargo install --path agent-harness --force",
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
    Some(out.join("\n"))
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
}
