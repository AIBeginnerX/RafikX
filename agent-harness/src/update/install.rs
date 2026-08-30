use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(windows)]
use std::process::Stdio;

use super::{GIT_UPD_TOKEN_ENV, GIT_UPD_USER_ENV, GIT_URL, ValidatedCommit, ValidatedTag};

pub(super) fn perform_install(raw_tag: &str, raw_commit: &str) -> anyhow::Result<()> {
    let tag = ValidatedTag::parse(raw_tag)?;
    let commit = ValidatedCommit::parse(raw_commit)?;
    let root = TempRoot::new()?;
    let source = prepare_source(tag, commit, root.path())?;

    #[cfg(not(windows))]
    {
        let mut cargo = sanitized_command("cargo");
        cargo
            .args(["install", "--path"])
            .arg(source.join("agent-harness"))
            .args(["--locked", "--force"]);
        run_status(&mut cargo, "cargo install")?;
        println!();
        println!("업그레이드 완료 — `rafikx` 를 다시 실행하세요.");
        Ok(())
    }

    #[cfg(windows)]
    {
        let install_root = root.path().join("install");
        let mut cargo = sanitized_command("cargo");
        cargo
            .args(["install", "--root"])
            .arg(&install_root)
            .args(["--path"])
            .arg(source.join("agent-harness"))
            .args(["--locked", "--force"]);
        run_status(&mut cargo, "cargo install")?;
        let staged = install_root.join("bin/rafikx.exe");
        if !staged.is_file() {
            anyhow::bail!(
                "업데이트 실행 파일을 찾을 수 없습니다: {}",
                staged.display()
            );
        }
        let current = std::env::current_exe()?;
        spawn_windows_finalizer(std::process::id(), &staged, &current, root.path())?;
        let _ = root.persist();
        println!();
        println!("업데이트 파일 준비 완료 — 현재 프로세스 종료 후 자동 교체됩니다.");
        Ok(())
    }
}

struct TempRoot {
    path: PathBuf,
    cleanup: bool,
}

impl TempRoot {
    fn new() -> anyhow::Result<Self> {
        let path = std::env::temp_dir().join(format!("rafikx-update-{}", crate::db::Db::new_id()));
        std::fs::create_dir(&path)?;
        Ok(Self {
            path,
            cleanup: true,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(windows)]
    fn persist(mut self) -> PathBuf {
        self.cleanup = false;
        self.path.clone()
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn sanitized_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for name in [
        GIT_UPD_USER_ENV,
        GIT_UPD_TOKEN_ENV,
        "GH_TOKEN",
        "GITHUB_TOKEN",
    ] {
        command.env_remove(name);
    }
    command
}

fn run_status(command: &mut Command, action: &str) -> anyhow::Result<()> {
    let status = command
        .status()
        .map_err(|error| anyhow::anyhow!("{action} 실행 실패: {error}"))?;
    if !status.success() {
        anyhow::bail!("{action} 실패 (exit {})", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn checked_output(command: &mut Command, action: &str) -> anyhow::Result<String> {
    let Output {
        status,
        stdout,
        stderr,
    } = command
        .output()
        .map_err(|error| anyhow::anyhow!("{action} 실행 실패: {error}"))?;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr);
        anyhow::bail!("{action} 실패: {}", detail.trim());
    }
    if stdout.len() > 4096 || stderr.len() > 4096 {
        anyhow::bail!("{action} 출력이 상한을 초과했습니다");
    }
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

fn tag_ref(tag: ValidatedTag<'_>) -> String {
    format!("refs/tags/{}", tag.as_str())
}

fn prepare_source(
    tag: ValidatedTag<'_>,
    commit: ValidatedCommit<'_>,
    root: &Path,
) -> anyhow::Result<PathBuf> {
    prepare_source_from(tag, commit, root, Path::new(GIT_URL), true)
}

fn prepare_source_from(
    tag: ValidatedTag<'_>,
    commit: ValidatedCommit<'_>,
    root: &Path,
    remote_url: &Path,
    authenticated: bool,
) -> anyhow::Result<PathBuf> {
    let source = root.join("source");
    let reference = tag_ref(tag);

    let mut init = sanitized_command("git");
    init.args(["init", "-q"]).arg(&source);
    run_status(&mut init, "git init")?;

    let mut remote = sanitized_command("git");
    remote
        .arg("-C")
        .arg(&source)
        .args(["remote", "add", "origin"])
        .arg(remote_url);
    run_status(&mut remote, "git remote add")?;

    let mut fetch = sanitized_command("git");
    fetch.arg("-C").arg(&source);
    if authenticated {
        super::apply_git_credentials(&mut fetch);
    }
    fetch.args([
        "fetch",
        "--depth",
        "1",
        "origin",
        &format!("{reference}:{reference}"),
    ]);
    run_status(&mut fetch, "git fetch")?;

    let mut tag_head = sanitized_command("git");
    tag_head.arg("-C").arg(&source).args([
        "rev-parse",
        "--verify",
        &format!("{reference}^{{commit}}"),
    ]);
    let tag_head = checked_output(&mut tag_head, "git rev-parse tag")?;
    if !tag_head.eq_ignore_ascii_case(commit.as_str()) {
        anyhow::bail!("릴리스 태그가 확인 이후 이동했습니다: {}", tag.as_str());
    }

    let mut checkout = sanitized_command("git");
    checkout
        .arg("-C")
        .arg(&source)
        .args(["checkout", "-q", "--detach", commit.as_str()]);
    run_status(&mut checkout, "git checkout")?;

    let mut head = sanitized_command("git");
    head.arg("-C")
        .arg(&source)
        .args(["rev-parse", "--verify", "HEAD^{commit}"]);
    let head = checked_output(&mut head, "git rev-parse HEAD")?;
    if !head.eq_ignore_ascii_case(commit.as_str()) {
        anyhow::bail!("릴리스 커밋 checkout 검증 실패: {}", tag.as_str());
    }
    Ok(source)
}

#[cfg(any(windows, test))]
fn windows_finalize_script() -> &'static str {
    r#"$ErrorActionPreference = 'Stop'
$parentId = [int]$env:RAFIKX_UPDATE_PARENT
$source = $env:RAFIKX_UPDATE_SOURCE
$destination = $env:RAFIKX_UPDATE_DESTINATION
$root = $env:RAFIKX_UPDATE_ROOT
Wait-Process -Id $parentId -ErrorAction SilentlyContinue
$copied = $false
for ($attempt = 0; $attempt -lt 20; $attempt++) {
    try {
        Copy-Item -LiteralPath $source -Destination $destination -Force
        $copied = $true
        break
    } catch {
        Start-Sleep -Milliseconds 250
    }
}
if (-not $copied) { throw 'RafikX 실행 파일 교체 실패' }
Remove-Item -LiteralPath $root -Recurse -Force
"#
}

#[cfg(windows)]
fn spawn_windows_finalizer(
    parent: u32,
    source: &Path,
    destination: &Path,
    root: &Path,
) -> anyhow::Result<()> {
    let mut command = sanitized_command("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            windows_finalize_script(),
        ])
        .env("RAFIKX_UPDATE_PARENT", parent.to_string())
        .env("RAFIKX_UPDATE_SOURCE", source)
        .env("RAFIKX_UPDATE_DESTINATION", destination)
        .env("RAFIKX_UPDATE_ROOT", root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn()?;
    Ok(())
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

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn tagged_remote(annotated: bool) -> (PathBuf, PathBuf, String) {
        let root = std::env::temp_dir().join(format!(
            "rafikx-update-tag-fixture-{}",
            crate::db::Db::new_id()
        ));
        let work = root.join("work");
        let remote = root.join("remote.git");
        std::fs::create_dir_all(&root).expect("fixture root");
        let init = Command::new("git")
            .args(["init", "-q"])
            .arg(&work)
            .status()
            .expect("git init work");
        assert!(init.success());
        git(&work, &["config", "user.name", "RafikX Test"]);
        git(&work, &["config", "user.email", "rafikx@example.invalid"]);
        git(&work, &["config", "commit.gpgsign", "false"]);
        git(&work, &["config", "tag.gpgsign", "false"]);
        std::fs::write(work.join("fixture.txt"), "first\n").expect("fixture file");
        git(&work, &["add", "fixture.txt"]);
        git(&work, &["commit", "-q", "-m", "first"]);
        if annotated {
            git(&work, &["tag", "-a", "v1.2.3", "-m", "release"]);
        } else {
            git(&work, &["tag", "v1.2.3"]);
        }
        let commit = git(&work, &["rev-parse", "HEAD^{commit}"]);
        let init = Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&remote)
            .status()
            .expect("git init remote");
        assert!(init.success());
        git(
            &work,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        git(
            &work,
            &[
                "push",
                "-q",
                "origin",
                "HEAD:refs/heads/master",
                "refs/tags/v1.2.3",
            ],
        );
        (root, work, commit)
    }

    #[test]
    fn native_checkout_accepts_lightweight_and_annotated_pinned_tags() {
        for annotated in [false, true] {
            let (root, _, commit) = tagged_remote(annotated);
            let checkout = root.join("checkout");
            let tag = ValidatedTag::parse("v1.2.3").expect("valid tag");
            let commit = ValidatedCommit::parse(&commit).expect("valid commit");
            let source =
                prepare_source_from(tag, commit, &checkout, &root.join("remote.git"), false)
                    .expect("pinned tag checkout");
            assert_eq!(
                git(&source, &["rev-parse", "HEAD^{commit}"]),
                commit.as_str()
            );
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn native_checkout_rejects_a_tag_moved_after_discovery() {
        let (root, work, discovered) = tagged_remote(false);
        std::fs::write(work.join("fixture.txt"), "second\n").expect("updated fixture");
        git(&work, &["add", "fixture.txt"]);
        git(&work, &["commit", "-q", "-m", "second"]);
        git(&work, &["tag", "-f", "v1.2.3"]);
        git(
            &work,
            &["push", "-q", "--force", "origin", "refs/tags/v1.2.3"],
        );

        let error = prepare_source_from(
            ValidatedTag::parse("v1.2.3").expect("valid tag"),
            ValidatedCommit::parse(&discovered).expect("discovered commit"),
            &root.join("checkout"),
            &root.join("remote.git"),
            false,
        )
        .expect_err("moved tag must fail before checkout");
        assert!(error.to_string().contains("이동했습니다"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sanitized_commands_remove_update_credentials() {
        let command = sanitized_command("git");
        let removed = command
            .get_envs()
            .filter_map(|(name, value)| {
                value
                    .is_none()
                    .then_some(name.to_string_lossy().to_string())
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            removed,
            [
                GIT_UPD_USER_ENV.to_string(),
                GIT_UPD_TOKEN_ENV.to_string(),
                "GH_TOKEN".to_string(),
                "GITHUB_TOKEN".to_string(),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn windows_finalizer_waits_for_exit_before_replacing_the_binary() {
        let script = windows_finalize_script();
        let wait = script.find("Wait-Process").expect("parent wait");
        let copy = script.find("Copy-Item").expect("binary replacement");
        let cleanup = script.find("Remove-Item").expect("temporary cleanup");
        assert!(wait < copy && copy < cleanup);
        assert!(!script.contains("GIT_UPD_TOKEN"));
    }
}
