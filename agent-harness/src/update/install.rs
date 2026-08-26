use std::process::Command;

use super::GIT_URL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ValidatedTag<'a>(&'a str);

impl<'a> ValidatedTag<'a> {
    fn parse(raw: &'a str) -> anyhow::Result<Self> {
        let Some(version) = raw.strip_prefix('v') else {
            anyhow::bail!("릴리스 태그는 정확한 vX.Y.Z 형식이어야 합니다: {raw:?}");
        };
        let mut parts = version.split('.');
        let (Some(major), Some(minor), Some(patch), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            anyhow::bail!("릴리스 태그는 정확한 vX.Y.Z 형식이어야 합니다: {raw:?}");
        };
        let valid_component = |part: &str| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'))
        };
        if ![major, minor, patch].into_iter().all(valid_component) {
            anyhow::bail!("릴리스 태그는 정확한 vX.Y.Z 형식이어야 합니다: {raw:?}");
        }
        Ok(Self(raw))
    }

    fn as_str(self) -> &'a str {
        self.0
    }
}

pub(super) fn perform_install(raw_tag: &str) -> anyhow::Result<()> {
    let tag = ValidatedTag::parse(raw_tag)?;
    let status = Command::new("sh").args(install_args(tag)).status()?;
    if !status.success() {
        anyhow::bail!("업그레이드 실패 (exit {})", status.code().unwrap_or(-1));
    }
    println!();
    println!("업그레이드 완료 — `rafikx` 를 다시 실행하세요.");
    Ok(())
}

fn install_args<'a>(tag: ValidatedTag<'a>) -> [&'a str; 5] {
    [
        "-c",
        install_script(),
        "rafikx-updater",
        tag.as_str(),
        GIT_URL,
    ]
}

fn install_script() -> &'static str {
    r#"
set -eu
TAG="$1"
REPO="$2"
TAG_REF="refs/tags/$TAG"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/rafikx-update.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
SRC="$TMP_ROOT/source"
git init -q "$SRC"
git -C "$SRC" remote add origin "$REPO"
git -C "$SRC" fetch --depth 1 origin "$TAG_REF:$TAG_REF"
git -C "$SRC" checkout -q --detach "$TAG_REF"
HEAD=$(git -C "$SRC" rev-parse --verify "HEAD^{commit}")
TAG_HEAD=$(git -C "$SRC" rev-parse --verify "$TAG_REF^{commit}")
if [ "$HEAD" != "$TAG_HEAD" ]; then
  echo "릴리스 태그 검증 실패: $TAG" >&2
  exit 1
fi
cargo install --path "$SRC/agent-harness" --locked --force
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_tag_when_stable_release_is_exact() {
        for raw in ["v0.0.0", "v1.2.3", "v10.20.30"] {
            let tag = ValidatedTag::parse(raw).expect("stable release tag must be accepted");
            assert_eq!(tag.as_str(), raw);
        }
    }

    #[test]
    fn rejects_tag_when_format_is_not_exact() {
        for raw in [
            "",
            "1.2.3",
            "V1.2.3",
            "v1.2",
            "v1.2.3.4",
            "v01.2.3",
            "v1.02.3",
            "v1.2.03",
            "v1.2.3-rc.1",
            "v1.2.3+build",
            " v1.2.3",
            "v1.2.3 ",
            "master",
            "v1.2.3;echo pwned",
        ] {
            let error = ValidatedTag::parse(raw).expect_err("unsafe tag must be rejected");
            assert!(error.to_string().contains("vX.Y.Z"));
        }
    }

    #[test]
    fn targets_exact_ref_when_installing_discovered_tag() {
        let tag = ValidatedTag::parse("v1.2.3").expect("valid release tag");
        let args = install_args(tag);
        let script = install_script();

        assert_eq!(args, ["-c", script, "rafikx-updater", "v1.2.3", GIT_URL]);
        assert!(script.contains("TAG=\"$1\""));
        assert!(script.contains("REPO=\"$2\""));
        assert!(script.contains("refs/tags/$TAG"));
        assert!(script.contains("fetch --depth 1 origin \"$TAG_REF:$TAG_REF\""));
        assert!(script.contains("checkout -q --detach \"$TAG_REF\""));
        assert!(script.contains("rev-parse --verify \"$TAG_REF^{commit}\""));
        assert!(script.contains("mktemp -d"));
        assert!(script.contains("trap 'rm -rf"));
        assert!(script.contains("cargo install --path"));
        let mktemp = script
            .find("mktemp -d")
            .expect("temporary checkout creation");
        let cleanup = script.find("trap 'rm -rf").expect("cleanup trap");
        let git = script.find("git init").expect("git initialization");
        let cargo = script.find("cargo install").expect("cargo installation");
        assert!(mktemp < cleanup && cleanup < git && git < cargo);
        assert!(!script.contains("master"));
        assert!(!script.contains(".rafikx-src"));
    }
}
