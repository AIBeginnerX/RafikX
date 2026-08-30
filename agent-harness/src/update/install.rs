use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(windows)]
use std::process::Stdio;

use super::{ValidatedCommit, ValidatedTag, GIT_URL};

pub(super) fn perform_install(raw_tag: &str, raw_commit: &str) -> anyhow::Result<()> {
    let tag = ValidatedTag::parse(raw_tag)?;
    let commit = ValidatedCommit::parse(raw_commit)?;
    let root = TempRoot::new()?;
    let source = prepare_source(tag, commit, root.path())?;
    let manifest = source.join("agent-harness/Cargo.toml");

    let mut fetch = sanitized_network_command("cargo");
    fetch
        .arg("fetch")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--locked");
    run_status(&mut fetch, "cargo fetch")?;

    #[cfg(not(windows))]
    {
        let mut cargo = sanitized_command("cargo");
        cargo
            .args(["install", "--offline", "--path"])
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
            .args(["install", "--offline", "--root"])
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
    sanitized_command_with_profile(program, std::env::vars_os(), EnvironmentProfile::Build)
}

fn sanitized_network_command(program: impl AsRef<OsStr>) -> Command {
    sanitized_command_with_profile(program, std::env::vars_os(), EnvironmentProfile::Network)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnvironmentProfile {
    Build,
    Network,
}

fn sanitized_command_with_profile<I, K, V>(
    program: impl AsRef<OsStr>,
    vars: I,
    profile: EnvironmentProfile,
) -> Command
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command.env_clear();
    for (name, value) in vars {
        if is_allowed_environment_name(name.as_ref(), profile) {
            command.env(name, value);
        }
    }
    command
}

const COMMON_ENVIRONMENT_NAMES: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SYSTEMROOT",
    "WINDIR",
    "SYSTEMDRIVE",
    "COMSPEC",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "RUSTC",
    "RUSTDOC",
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET",
    "CARGO_BUILD_JOBS",
    "CARGO_BUILD_PIPELINING",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTDOC",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_INCREMENTAL",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_TERM_COLOR",
    "CARGO_TERM_VERBOSE",
    "CC",
    "CXX",
    "AR",
    "RANLIB",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "CPATH",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "OBJC_INCLUDE_PATH",
    "LIBRARY_PATH",
    "LD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "PKG_CONFIG",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_LIBDIR",
    "PKG_CONFIG_SYSROOT_DIR",
    "BINDGEN_EXTRA_CLANG_ARGS",
    "LIBCLANG_PATH",
    "SDKROOT",
    "MACOSX_DEPLOYMENT_TARGET",
    "DEVELOPER_DIR",
    "VCPKG_ROOT",
    "VCPKG_DEFAULT_TRIPLET",
    "CMAKE_PREFIX_PATH",
    "CMAKE_GENERATOR",
    "CMAKE_TOOLCHAIN_FILE",
    "CMAKE_BUILD_PARALLEL_LEVEL",
    "VSINSTALLDIR",
    "VCINSTALLDIR",
    "VCToolsInstallDir",
    "VCToolsVersion",
    "WindowsSdkDir",
    "WindowsSDKVersion",
    "UniversalCRTSdkDir",
    "UCRTVersion",
    "WindowsLibPath",
    "INCLUDE",
    "LIB",
    "LIBPATH",
    "CL",
    "LINK",
    "Platform",
    "VisualStudioVersion",
    "PreferredToolArchitecture",
    "DevEnvDir",
];

const NETWORK_ENVIRONMENT_NAMES: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_CA_BUNDLE",
    "RUSTUP_DIST_SERVER",
    "RUSTUP_UPDATE_ROOT",
    "CARGO_HTTP_PROXY",
    "CARGO_HTTP_TIMEOUT",
    "CARGO_HTTP_LOW_SPEED_LIMIT",
    "CARGO_HTTP_MULTIPLEXING",
    "CARGO_HTTP_CAINFO",
    "CARGO_HTTP_CHECK_REVOKE",
    "CARGO_NET_RETRY",
    "CARGO_NET_GIT_FETCH_WITH_CLI",
    "CARGO_REGISTRIES_CRATES_IO_PROTOCOL",
    "CARGO_REGISTRIES_CRATES_IO_INDEX",
];

fn environment_name_matches(name: &OsStr, allowed: &str, windows_case_insensitive: bool) -> bool {
    if windows_case_insensitive {
        name.to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(allowed))
    } else {
        name == OsStr::new(allowed)
    }
}

fn is_allowed_environment_name(name: &OsStr, profile: EnvironmentProfile) -> bool {
    let profile_names: &[&str] = match profile {
        EnvironmentProfile::Build => &[],
        EnvironmentProfile::Network => NETWORK_ENVIRONMENT_NAMES,
    };
    COMMON_ENVIRONMENT_NAMES
        .iter()
        .chain(profile_names.iter())
        .any(|allowed| environment_name_matches(name, allowed, cfg!(windows)))
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

    let mut fetch = sanitized_network_command("git");
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
    use super::super::{GIT_UPD_TOKEN_ENV, GIT_UPD_USER_ENV};
    use super::*;
    use std::ffi::OsString;

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
    fn sanitized_commands_clear_untrusted_environment() {
        let vars = [
            ("PATH", "/synthetic/bin"),
            ("HOME", "/synthetic/home"),
            ("TMPDIR", "/synthetic/tmp"),
            ("LANG", "ko_KR.UTF-8"),
            ("HTTPS_PROXY", "https://proxy.invalid"),
            ("SSL_CERT_FILE", "/synthetic/cert.pem"),
            ("RUSTUP_TOOLCHAIN", "stable"),
            ("CARGO_HOME", "/synthetic/cargo"),
            ("CC", "clang"),
            ("OPENAI_API_KEY", "openai-secret"),
            ("ANTHROPIC_API_KEY", "anthropic-secret"),
            ("TELEGRAM_BOT_TOKEN", "telegram-secret"),
            ("GITHUB_TOKEN", "github-secret"),
            (GIT_UPD_USER_ENV, "update-user"),
            (GIT_UPD_TOKEN_ENV, "update-secret"),
            ("DATABASE_URL", "database-secret"),
            ("CUSTOM_SECRET", "custom-secret"),
            ("CARGO_REGISTRIES_CRATES_IO_TOKEN", "registry-secret"),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));
        let command = sanitized_command_with_profile("git", vars, EnvironmentProfile::Build);
        let retained = environment_map(&command);
        assert_eq!(
            retained,
            [
                ("PATH", "/synthetic/bin"),
                ("HOME", "/synthetic/home"),
                ("TMPDIR", "/synthetic/tmp"),
                ("LANG", "ko_KR.UTF-8"),
                ("RUSTUP_TOOLCHAIN", "stable"),
                ("CARGO_HOME", "/synthetic/cargo"),
                ("CC", "clang"),
            ]
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect()
        );
    }

    #[test]
    fn sanitized_command_get_envs_contains_only_allowlisted_assignments() {
        let vars = [
            ("PATH", "/synthetic/bin"),
            ("CARGO_REGISTRIES_CRATES_IO_PROTOCOL", "sparse"),
            ("CARGO_REGISTRIES_CRATES_IO_TOKEN", "must-drop"),
            ("RAFIKX_UPDATE_ROOT", "/must-drop"),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));
        let command = sanitized_command_with_profile("cargo", vars, EnvironmentProfile::Build);
        assert_eq!(
            environment_map(&command),
            [("PATH", "/synthetic/bin"),]
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect()
        );
    }

    #[test]
    fn network_and_build_profiles_have_distinct_boundaries() {
        let vars = [
            ("PATH", "/synthetic/bin"),
            (
                "HTTPS_PROXY",
                "https://proxy-user:proxy-secret@proxy.invalid",
            ),
            (
                "CARGO_HTTP_PROXY",
                "http://cargo-user:cargo-secret@proxy.invalid",
            ),
            (
                "CARGO_REGISTRIES_CRATES_IO_INDEX",
                "sparse+https://registry-user:registry-secret@registry.invalid",
            ),
            ("CARGO_REGISTRIES_CRATES_IO_PROTOCOL", "sparse"),
            ("CARGO_NET_RETRY", "4"),
            ("SSL_CERT_FILE", "/synthetic/cert.pem"),
            ("CARGO_HTTP_CAINFO", "/synthetic/cargo-cert.pem"),
            ("RUSTUP_DIST_SERVER", "https://rustup.invalid"),
            ("RUSTUP_UPDATE_ROOT", "https://rustup.invalid/update"),
            ("CC", "clang"),
            ("CMAKE_TOOLCHAIN_FILE", "/synthetic/toolchain.cmake"),
            ("VCPKG_ROOT", "/synthetic/vcpkg"),
            ("OPENAI_API_KEY", "openai-secret"),
            ("ANTHROPIC_API_KEY", "anthropic-secret"),
            ("TELEGRAM_BOT_TOKEN", "telegram-secret"),
            ("GITHUB_TOKEN", "github-secret"),
            (GIT_UPD_USER_ENV, "update-user"),
            (GIT_UPD_TOKEN_ENV, "update-secret"),
            ("DATABASE_URL", "database-secret"),
            ("CUSTOM_SECRET", "custom-secret"),
            ("CARGO_REGISTRIES_CRATES_IO_TOKEN", "registry-secret"),
            ("REQUESTS_CA_BUNDLE", "/must-drop"),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect::<Vec<_>>();
        let network =
            sanitized_command_with_profile("cargo", vars.clone(), EnvironmentProfile::Network);
        let build = sanitized_command_with_profile("cargo", vars, EnvironmentProfile::Build);

        let network_env = environment_map(&network);
        let build_env = environment_map(&build);
        assert_eq!(
            network_env.get(&OsString::from("HTTPS_PROXY")),
            Some(&OsString::from(
                "https://proxy-user:proxy-secret@proxy.invalid"
            ))
        );
        assert_eq!(
            network_env.get(&OsString::from("CARGO_REGISTRIES_CRATES_IO_INDEX")),
            Some(&OsString::from(
                "sparse+https://registry-user:registry-secret@registry.invalid"
            ))
        );
        assert_eq!(
            build_env.get(&OsString::from("CC")),
            Some(&OsString::from("clang"))
        );
        for name in [
            "HTTPS_PROXY",
            "CARGO_HTTP_PROXY",
            "CARGO_REGISTRIES_CRATES_IO_INDEX",
            "CARGO_REGISTRIES_CRATES_IO_PROTOCOL",
            "CARGO_NET_RETRY",
            "SSL_CERT_FILE",
            "CARGO_HTTP_CAINFO",
            "RUSTUP_DIST_SERVER",
            "RUSTUP_UPDATE_ROOT",
            "REQUESTS_CA_BUNDLE",
        ] {
            assert!(
                !build_env.contains_key(OsStr::new(name)),
                "{name} leaked into build"
            );
        }
        for name in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "TELEGRAM_BOT_TOKEN",
            "GITHUB_TOKEN",
            GIT_UPD_USER_ENV,
            GIT_UPD_TOKEN_ENV,
            "DATABASE_URL",
            "CUSTOM_SECRET",
            "CARGO_REGISTRIES_CRATES_IO_TOKEN",
        ] {
            assert!(
                !network_env.contains_key(OsStr::new(name)),
                "{name} leaked into network"
            );
            assert!(
                !build_env.contains_key(OsStr::new(name)),
                "{name} leaked into build"
            );
        }
    }

    #[test]
    fn update_credentials_are_only_present_on_authenticated_git_fetch() {
        let vars = [
            ("PATH", "/synthetic/bin"),
            (GIT_UPD_USER_ENV, "ambient-user"),
            (GIT_UPD_TOKEN_ENV, "ambient-token"),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect::<Vec<_>>();

        let mut fetch =
            sanitized_command_with_profile("git", vars.clone(), EnvironmentProfile::Network);
        super::super::apply_git_credentials_with(&mut fetch, Some("fetch-token".into()));
        fetch.args(["fetch", "origin"]);

        let mut rev_parse =
            sanitized_command_with_profile("git", vars.clone(), EnvironmentProfile::Build);
        rev_parse.args(["rev-parse", "HEAD"]);
        let mut checkout =
            sanitized_command_with_profile("git", vars.clone(), EnvironmentProfile::Build);
        checkout.args(["checkout", "HEAD"]);
        let mut cargo_fetch =
            sanitized_command_with_profile("cargo", vars.clone(), EnvironmentProfile::Network);
        cargo_fetch.args(["fetch", "--locked"]);
        let mut cargo_install =
            sanitized_command_with_profile("cargo", vars, EnvironmentProfile::Build);
        cargo_install.args(["install", "--offline"]);

        let credential_names = [GIT_UPD_USER_ENV, GIT_UPD_TOKEN_ENV];
        for name in credential_names {
            assert!(
                environment_map(&fetch).contains_key(OsStr::new(name)),
                "fetch missing {name}"
            );
            for command in [&rev_parse, &checkout, &cargo_fetch, &cargo_install] {
                assert!(
                    !environment_map(command).contains_key(OsStr::new(name)),
                    "{name} leaked into a non-fetch command"
                );
            }
        }
        assert!(fetch
            .get_args()
            .all(|argument| argument != OsStr::new("fetch-token")));
    }

    fn environment_map(command: &Command) -> std::collections::BTreeMap<OsString, OsString> {
        command
            .get_envs()
            .filter_map(|(name, value)| value.map(|value| (name.to_owned(), value.to_owned())))
            .collect()
    }

    #[test]
    fn environment_name_matcher_respects_explicit_case_mode() {
        assert!(environment_name_matches(OsStr::new("PATH"), "PATH", false));
        assert!(!environment_name_matches(OsStr::new("pAtH"), "PATH", false));
        assert!(environment_name_matches(OsStr::new("pAtH"), "PATH", true));
        assert!(!environment_name_matches(
            OsStr::new("OpenAI_API_KEY"),
            "PATH",
            true
        ));
    }

    #[cfg(windows)]
    #[test]
    fn allowlist_matches_environment_names_case_insensitively_on_windows() {
        assert!(is_allowed_environment_name(
            OsStr::new("pAtH"),
            EnvironmentProfile::Build
        ));
        assert!(is_allowed_environment_name(
            OsStr::new("rUsTuP_tOoLcHaIn"),
            EnvironmentProfile::Build
        ));
        assert!(!is_allowed_environment_name(
            OsStr::new("OpenAI_API_KEY"),
            EnvironmentProfile::Network
        ));
    }

    #[test]
    fn sanitized_cargo_can_discover_its_toolchain_when_available() {
        let mut command = sanitized_command("cargo");
        command.arg("--version");
        let output = match command.output() {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                panic!("cargo --version was not found: {error}")
            }
            Err(error) => panic!("cargo --version failed to start: {error}"),
        };
        assert!(
            output.status.success(),
            "cargo --version failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).starts_with("cargo "));
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
