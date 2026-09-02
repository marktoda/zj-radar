//! `zj-radar update` — move the CLI and the sidebar wasm to a newer release
//! together.
//!
//! The two halves share the status contract and setup expectations, so they
//! must move as one: the CLI is replaced in place, then the *new* binary is
//! re-executed to run `setup zellij --download` (fetching the wasm built from
//! its own version) and the idempotent producer setups. `--check` reports both
//! halves without writing — the wasm half by comparing the installed file's
//! sha256 against the release's published sidecar, so a wasm that drifted from
//! the CLI shows up even when the CLI itself is current.
//!
//! `update` moves what is *installed*: a missing sidebar is a `setup` job, and
//! a binary owned by Nix or cargo is handed back to that tool rather than
//! overwritten behind its back (see [`InstallKind`]).

use std::path::{Path, PathBuf};

pub(crate) struct UpdateOptions {
    pub check: bool,
}

/// Where the installed sidebar wasm stands relative to the target release.
#[derive(Debug, PartialEq, Eq)]
enum WasmState {
    NotInstalled,
    /// A symlink (home-manager / Nix): the same guard `setup zellij` applies to
    /// a symlinked config.kdl — writing through it is reverted on the next
    /// switch, and the bytes are that build's, not the release's.
    Managed,
    Current,
    Stale,
    /// Couldn't tell (no config dir, no published checksum, no sha256 tool).
    Unknown(String),
}

/// Entry point for `zj-radar update`.
pub fn run(options: UpdateOptions) {
    let current = env!("CARGO_PKG_VERSION");
    let target = match target_version() {
        Ok(v) => v,
        Err(e) => {
            crate::exit::fail_report("update", e);
            return;
        }
    };
    let cli_behind = is_newer(&target, current);
    // Forward only: a pin older than this CLI would refresh the wasm to the
    // pin while the binary stays put — exactly the version split `update`
    // exists to prevent. Downgrades go through the installer, which replaces
    // the CLI first (then `setup zellij --download` follows its version).
    if !cli_behind && target != current {
        crate::exit::fail_report(
            "update",
            format!(
                "ZJ_RADAR_VERSION=v{target} is older than this CLI (v{current}) — update only moves forward. \
                 To downgrade, reinstall that release: ZJ_RADAR_VERSION=v{target} …/install.sh | sh, \
                 then `zj-radar setup zellij --download`"
            ),
        );
        return;
    }
    if cli_behind {
        println!("cli:  v{current} → v{target} available");
    } else {
        println!("cli:  v{current} (up to date)");
    }

    let wasm = wasm_state(&target);
    match &wasm {
        WasmState::NotInstalled => {
            println!("wasm: not installed — run `zj-radar setup zellij --download` to add the sidebar")
        }
        WasmState::Managed => {
            println!("wasm: a symlink (managed by Nix / home-manager) — update it through your Nix config")
        }
        WasmState::Current => println!("wasm: matches v{target} (up to date)"),
        WasmState::Stale => println!("wasm: differs from v{target} — will be refreshed"),
        WasmState::Unknown(why) => println!("wasm: could not compare ({why})"),
    }

    let wasm_stale = wasm == WasmState::Stale;
    if options.check {
        if cli_behind {
            println!("run `zj-radar update` to apply");
            crate::exit::fail_report("update", format!("v{target} is available"));
        } else if wasm_stale {
            println!("run `zj-radar update` to apply");
            crate::exit::fail_report("update", format!("the installed sidebar wasm does not match v{target}"));
        }
        return;
    }

    if !cli_behind && !wasm_stale {
        println!("zj-radar: everything is up to date");
        return;
    }

    // Which binary runs the follow-up setup steps: the freshly installed one
    // when the CLI moved (so the wasm it fetches matches ITS version), else
    // this one.
    let exe = if cli_behind {
        match self_replace(&target) {
            Ok(Some(exe)) => exe,
            Ok(None) => return, // handed off to Nix/cargo — printed already
            Err(e) => {
                crate::exit::fail_report("update", e);
                return;
            }
        }
    } else {
        match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                crate::exit::fail_report("update", format!("could not locate this executable — {e}"));
                return;
            }
        }
    };

    // The pin travels with the re-exec so a release landing between the
    // lookup above and this step can't split the two halves across versions.
    let mut refreshed_wasm = false;
    if wasm != WasmState::NotInstalled {
        refreshed_wasm = rerun(&exe, &target, &["setup", "zellij", "--download", "-y"]);
    }
    if cli_behind {
        // Producer wiring is idempotent and cheap; a release may have changed
        // the codex hook shape or the vendored opencode bridge.
        rerun(&exe, &target, &["setup", "-y"]);
    }
    if refreshed_wasm {
        println!("zj-radar: restart Zellij (or open a new session) to load the updated sidebar");
    }
    if cli_behind {
        println!("zj-radar: the Claude Code plugin updates from inside Claude — `/plugin update zj-radar-claude@zj-radar`");
    }
    // The doctor's exit code grades install completeness (a machine with no
    // producer wired reads "missing"), not this update — its items are the
    // report; only a failure to *run* it is ours.
    let _ = std::process::Command::new(&exe)
        .args(["setup", "--check"])
        .env("ZJ_RADAR_VERSION", &target)
        .status()
        .map_err(|e| crate::exit::fail_report("update", format!("could not run {} — {e}", exe.display())));
}

/// Run a follow-up `zj-radar` step through `exe`, inheriting stdio so its own
/// output lands in the user's terminal. `false` (with the failure reported)
/// when it didn't exit cleanly — steps after it still run, since the doctor
/// report is most useful exactly when something went wrong.
fn rerun(exe: &Path, target: &str, args: &[&str]) -> bool {
    println!("zj-radar: running `zj-radar {}`", args.join(" "));
    match std::process::Command::new(exe)
        .args(args)
        .env("ZJ_RADAR_VERSION", target)
        .status()
    {
        Ok(s) if s.success() => true,
        Ok(s) => {
            crate::exit::fail_report("update", format!("`zj-radar {}` exited with {s}", args.join(" ")));
            false
        }
        Err(e) => {
            crate::exit::fail_report("update", format!("could not run {} — {e}", exe.display()));
            false
        }
    }
}

/// The release to move to: `ZJ_RADAR_VERSION` when pinned (no network), else
/// whatever GitHub's `releases/latest` redirects to.
fn target_version() -> Result<String, String> {
    if let Ok(v) = std::env::var("ZJ_RADAR_VERSION") {
        let v = v.trim_start_matches('v');
        if !v.is_empty() {
            return Ok(v.to_string());
        }
    }
    latest_release_version()
}

/// Ask GitHub for the latest release tag by following the `releases/latest`
/// redirect (HEAD only, no API call — so no token, no rate-limit budget).
fn latest_release_version() -> Result<String, String> {
    use std::process::Command;
    let url = format!("https://github.com/{}/releases/latest", crate::setup::repo_slug());
    let effective = if crate::setup::which("curl") {
        let out = Command::new("curl")
            .args(["--proto", "=https", "--proto-redir", "=https", "--tlsv1.2", "-fsSLI", "-o", "/dev/null", "-w", "%{url_effective}"])
            .arg(&url)
            .output()
            .map_err(|e| format!("failed to run curl — {e}"))?;
        if !out.status.success() {
            return Err(format!("could not reach {url} to find the latest release"));
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else if crate::setup::which("wget") {
        // `--spider -S` prints the response headers (stderr); the last
        // `Location:` is the redirect target.
        let out = Command::new("wget")
            .args(["--https-only", "--spider", "-S", "-q"])
            .arg(&url)
            .output()
            .map_err(|e| format!("failed to run wget — {e}"))?;
        let headers = String::from_utf8_lossy(&out.stderr);
        headers
            .lines()
            .filter_map(|l| l.trim().strip_prefix("Location: "))
            .next_back()
            .map(|s| s.trim().to_string())
            .ok_or_else(|| format!("could not reach {url} to find the latest release"))?
    } else {
        return Err("need curl or wget on PATH to look up the latest release".to_string());
    };
    tag_from_latest_redirect(&effective)
        .ok_or_else(|| format!("could not read a release version from {effective}"))
}

/// Compare the installed wasm's sha256 with the digest published for `target`.
fn wasm_state(target: &str) -> WasmState {
    let Some(config_dir) = crate::setup::zellij_config_dir() else {
        return WasmState::Unknown("no Zellij config dir — set $HOME".to_string());
    };
    let installed = crate::setup::zellij_wasm_dest(&config_dir);
    if crate::setup::path_is_managed(&installed) {
        return WasmState::Managed;
    }
    if !installed.is_file() {
        return WasmState::NotInstalled;
    }
    let staging = match crate::setup::private_download_dir() {
        Ok(dir) => dir.join(crate::WASM_FILE_NAME),
        Err(e) => return WasmState::Unknown(e),
    };
    let Some(expected) = crate::setup::fetch_published_sha256(&crate::setup::wasm_checksum_url(target), &staging)
    else {
        return WasmState::Unknown(format!("no published checksum for v{target}"));
    };
    let Some(actual) = crate::setup::compute_sha256(&installed) else {
        return WasmState::Unknown("no sha256 tool (sha256sum/shasum) on PATH".to_string());
    };
    if actual.eq_ignore_ascii_case(&expected) {
        WasmState::Current
    } else {
        WasmState::Stale
    }
}

/// Replace this executable with the `target` release build. `Ok(None)` when
/// the binary belongs to a package manager and the hand-off was printed;
/// `Ok(Some(path))` with the (unchanged) executable path once the new binary
/// sits there.
fn self_replace(target: &str) -> Result<Option<PathBuf>, String> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("could not locate this executable — {e}"))?;
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match classify_install(&exe, cargo_home.as_deref(), home.as_deref()) {
        InstallKind::Nix => {
            println!(
                "zj-radar: this binary is managed by Nix ({}) — update the zj-radar input in your flake instead",
                exe.display()
            );
            return Ok(None);
        }
        InstallKind::Cargo => {
            println!(
                "zj-radar: this binary is managed by cargo ({}) — run `cargo install zj-radar` \
                 (or `cargo binstall zj-radar`), then `zj-radar setup zellij --download`",
                exe.display()
            );
            return Ok(None);
        }
        InstallKind::SelfManaged => {}
    }
    let Some(triple) = target_triple() else {
        return Err(
            "no prebuilt binary is published for this platform — build from source: \
             cargo install zj-radar, then zj-radar setup zellij --download"
                .to_string(),
        );
    };
    let staging = crate::setup::private_download_dir()?.join(format!("cli-{target}"));
    let tarball = staging.join(format!("zj-radar-{triple}.tar.gz"));
    let url = cli_release_url(target, triple);
    eprintln!("zj-radar: downloading zj-radar v{target} ({triple}) from {url}");
    let installed = crate::setup::download_verified_asset(&url, &tarball, &format!("zj-radar v{target} ({triple})"))
        .and_then(|()| extract_cli_tarball(&tarball, &staging))
        .and_then(|bin| replace_binary(&bin, &exe));
    let _ = std::fs::remove_dir_all(&staging);
    installed?;
    println!("zj-radar: installed v{target} to {}", exe.display());
    Ok(Some(exe))
}

/// How this binary got onto the machine — decides whether `update` may
/// overwrite it. Classified from the executable's path alone (no package
/// database is consulted), so it is unit-testable and cheap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallKind {
    /// Under `/nix/store`: immutable, owned by the user's flake — never write.
    Nix,
    /// Under cargo's bin dir: `cargo install` tracks this binary in
    /// `.crates.toml`; overwriting it behind cargo's back would leave cargo's
    /// record lying about the version.
    Cargo,
    /// Anything else (the curl|sh installer's `~/.local/bin`, a hand copy):
    /// ours to replace in place.
    SelfManaged,
}

/// Classify `exe` (already canonicalized by the caller) against the two
/// package-manager locations we refuse to write into. `cargo_home` is
/// `$CARGO_HOME` when set; the default `~/.cargo` derives from `home`.
pub(crate) fn classify_install(exe: &Path, cargo_home: Option<&Path>, home: Option<&Path>) -> InstallKind {
    if exe.starts_with("/nix/store") {
        return InstallKind::Nix;
    }
    let cargo_bin = match (cargo_home, home) {
        (Some(c), _) => Some(c.join("bin")),
        (None, Some(h)) => Some(h.join(".cargo").join("bin")),
        (None, None) => None,
    };
    // `exe` arrives canonical (symlinks resolved), so the candidate must be
    // compared in the same form — `$HOME` through a symlink (macOS `/var` →
    // `/private/var`, a symlinked home dir) would otherwise never prefix-match
    // and a cargo-managed binary would be overwritten. A dir that doesn't exist
    // can't contain `exe`, so the raw path is fine there.
    let cargo_bin = cargo_bin.map(|b| b.canonicalize().unwrap_or(b));
    if cargo_bin.is_some_and(|b| exe.starts_with(b)) {
        return InstallKind::Cargo;
    }
    InstallKind::SelfManaged
}

/// The version behind GitHub's `releases/latest` redirect: its target is
/// `…/releases/tag/<tag>`, so the tag is the last non-empty path segment.
/// `None` when the URL isn't a tag page (a repo with no releases lands on the
/// releases index) or the tag isn't a plain `MAJOR.MINOR.PATCH` version we can
/// compare against. A leading `v` is optional, matching `ZJ_RADAR_VERSION`.
pub(crate) fn tag_from_latest_redirect(url_effective: &str) -> Option<String> {
    let (_, tag) = url_effective.trim_end_matches('/').rsplit_once("/releases/tag/")?;
    let version = tag.trim_start_matches('v');
    parse_version(version).map(|_| version.to_string())
}

/// `MAJOR.MINOR.PATCH` as numbers; `None` for anything else (prerelease tags,
/// `nightly`, garbage). Releases here are always plain triples, so a stricter
/// parser is simpler than a semver dependency and fails closed on surprises.
fn parse_version(v: &str) -> Option<[u64; 3]> {
    let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
    let out = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(out)
}

/// Is `candidate` strictly newer than `current`? Unparseable input on either
/// side is "not newer" — the safe answer for a tool that would otherwise
/// download and replace a binary.
pub(crate) fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

/// The release tarball for a CLI version and Rust target triple — the same
/// naming `scripts/install.sh` and `[package.metadata.binstall]` use.
pub(crate) fn cli_release_url(version: &str, triple: &str) -> String {
    format!(
        "https://github.com/{}/releases/download/v{version}/zj-radar-{triple}.tar.gz",
        crate::setup::repo_slug()
    )
}

/// The Rust target triple the release workflow publishes for this host, fixed
/// at compile time (no `uname` parsing). `None` where no prebuilt binary
/// exists — Intel macOS and anything the installer doesn't cover.
pub(crate) fn target_triple() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-musl")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-unknown-linux-musl")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else {
        None
    }
}

/// Unpack a release tarball into `out` with the system `tar` (already a hard
/// requirement of the installer, and it keeps this crate free of an archive
/// dependency) and return the path of the `zj-radar` binary it contained.
pub(crate) fn extract_cli_tarball(tarball: &Path, out: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("create {} failed — {e}", out.display()))?;
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(tarball)
        .arg("-C")
        .arg(out)
        .status()
        .map_err(|e| format!("failed to run tar — {e}"))?;
    if !status.success() {
        return Err(format!("tar failed to unpack {}", tarball.display()));
    }
    let bin = out.join("zj-radar");
    if !bin.is_file() {
        return Err(format!("archive {} did not contain a zj-radar binary", tarball.display()));
    }
    Ok(bin)
}

/// Install `fresh` over `current` without a window where `current` is missing
/// or half-written. The staged copy is written as a sibling (`zj-radar.new`)
/// so the final `rename` is a same-filesystem atomic swap; the running process
/// keeps its mapped inode and only the *next* invocation sees the new binary
/// — the property a self-updater needs on both Linux and macOS. Any failure
/// removes the staging file so nothing stale sits next to the binary.
pub(crate) fn replace_binary(fresh: &Path, current: &Path) -> Result<(), String> {
    let Some(name) = current.file_name().and_then(|n| n.to_str()) else {
        return Err(format!("invalid binary path {}", current.display()));
    };
    let staged = current.with_file_name(format!("{name}.new"));
    let result = std::fs::copy(fresh, &staged)
        .map_err(|e| format!("staging into {} failed — {e}", current.parent().unwrap_or(current).display()))
        .and_then(|_| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("chmod {} failed — {e}", staged.display()))?;
            }
            std::fs::rename(&staged, current)
                .map_err(|e| format!("replacing {} failed — {e}", current.display()))
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn tag_from_latest_redirect_parses_the_tag_page_url() {
        assert_eq!(
            tag_from_latest_redirect("https://github.com/marktoda/zj-radar/releases/tag/v0.5.1"),
            Some("0.5.1".to_string())
        );
        // A trailing slash or a tag without the `v` prefix still resolves.
        assert_eq!(
            tag_from_latest_redirect("https://github.com/o/r/releases/tag/0.6.0/"),
            Some("0.6.0".to_string())
        );
    }

    #[test]
    fn tag_from_latest_redirect_rejects_non_tag_urls() {
        // No releases yet: GitHub serves the releases index instead of redirecting.
        assert_eq!(tag_from_latest_redirect("https://github.com/o/r/releases"), None);
        assert_eq!(tag_from_latest_redirect("https://github.com/o/r/releases/latest"), None);
        assert_eq!(tag_from_latest_redirect(""), None);
        // A tag that isn't a version is not something we can compare against.
        assert_eq!(tag_from_latest_redirect("https://github.com/o/r/releases/tag/nightly"), None);
    }

    #[test]
    fn is_newer_compares_numeric_semver_components() {
        assert!(is_newer("0.5.1", "0.5.0"));
        assert!(is_newer("0.10.0", "0.9.9")); // numeric, not lexical
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.5.0", "0.5.0"));
        assert!(!is_newer("0.4.9", "0.5.0"));
    }

    #[test]
    fn is_newer_treats_unparseable_versions_as_not_newer() {
        assert!(!is_newer("nightly", "0.5.0"));
        assert!(!is_newer("0.5.1", "garbage"));
    }

    #[test]
    fn classify_install_recognizes_nix_store_paths() {
        let exe = Path::new("/nix/store/abc123-zj-radar-cli-0.5.0/bin/zj-radar");
        assert_eq!(classify_install(exe, None, Some(Path::new("/home/u"))), InstallKind::Nix);
    }

    #[test]
    fn classify_install_recognizes_cargo_bin_via_cargo_home_then_default() {
        let home = Path::new("/home/u");
        // Explicit CARGO_HOME wins.
        let exe = Path::new("/opt/cargo/bin/zj-radar");
        assert_eq!(classify_install(exe, Some(Path::new("/opt/cargo")), Some(home)), InstallKind::Cargo);
        // Default ~/.cargo/bin when CARGO_HOME is unset.
        let exe = Path::new("/home/u/.cargo/bin/zj-radar");
        assert_eq!(classify_install(exe, None, Some(home)), InstallKind::Cargo);
    }

    #[test]
    fn classify_install_defaults_to_self_managed() {
        let home = Path::new("/home/u");
        assert_eq!(
            classify_install(Path::new("/home/u/.local/bin/zj-radar"), None, Some(home)),
            InstallKind::SelfManaged
        );
        assert_eq!(
            classify_install(Path::new("/usr/local/bin/zj-radar"), None, None),
            InstallKind::SelfManaged
        );
        // A `.cargo` directory elsewhere in the path is not cargo's bin dir.
        assert_eq!(
            classify_install(Path::new("/home/u/.cargo-backup/zj-radar"), None, Some(home)),
            InstallKind::SelfManaged
        );
    }

    #[test]
    fn cli_release_urls_follow_the_release_asset_naming() {
        assert_eq!(
            cli_release_url("0.5.1", "aarch64-apple-darwin"),
            "https://github.com/marktoda/zj-radar/releases/download/v0.5.1/zj-radar-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn replace_binary_swaps_contents_atomically_and_keeps_exec_bit() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("bin").join("zj-radar");
        std::fs::create_dir_all(current.parent().unwrap()).unwrap();
        std::fs::write(&current, b"old").unwrap();
        let fresh = dir.path().join("staged").join("zj-radar");
        std::fs::create_dir_all(fresh.parent().unwrap()).unwrap();
        std::fs::write(&fresh, b"new").unwrap();

        replace_binary(&fresh, &current).unwrap();

        assert_eq!(std::fs::read(&current).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&current).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "installed binary must be executable (mode {mode:o})");
        }
        // No staging leftovers next to the binary.
        let siblings: Vec<_> = std::fs::read_dir(current.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(siblings, vec!["zj-radar".to_string()]);
    }

    #[test]
    fn replace_binary_reports_an_unwritable_destination() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("zj-radar.new");
        std::fs::write(&fresh, b"new").unwrap();
        // A destination whose parent doesn't exist can't be renamed into.
        let current = dir.path().join("missing-dir").join("zj-radar");
        let err = replace_binary(&fresh, &current).unwrap_err();
        assert!(err.contains("missing-dir"), "error should name the destination: {err}");
    }

    /// Build a `.tar.gz` holding the given entries with the system `tar`, the
    /// same tool the extractor shells out to.
    fn make_tarball(dir: &Path, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for (name, bytes) in entries {
            std::fs::write(src.join(name), bytes).unwrap();
        }
        let tarball = dir.join("zj-radar-test.tar.gz");
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(&src)
            .args(entries.iter().map(|(n, _)| *n))
            .status()
            .unwrap();
        assert!(status.success());
        tarball
    }

    #[test]
    fn extract_cli_tarball_returns_the_binary_inside() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = make_tarball(dir.path(), &[("zj-radar", b"#!/bin/sh\necho hi\n")]);
        let out = dir.path().join("out");
        let bin = extract_cli_tarball(&tarball, &out).unwrap();
        assert_eq!(bin, out.join("zj-radar"));
        assert_eq!(std::fs::read(&bin).unwrap(), b"#!/bin/sh\necho hi\n");
    }

    #[test]
    fn extract_cli_tarball_rejects_an_archive_without_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = make_tarball(dir.path(), &[("README", b"nope")]);
        let err = extract_cli_tarball(&tarball, &dir.path().join("out")).unwrap_err();
        assert!(err.contains("zj-radar"), "error should say what was missing: {err}");
    }

    #[test]
    fn target_triple_is_one_of_the_published_release_targets() {
        // Whatever host runs this test must map onto a triple the release
        // workflow publishes (or None where we ship no prebuilt binary).
        let published = [
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
            "aarch64-apple-darwin",
        ];
        let has_prebuilt = cfg!(unix) && !cfg!(all(target_os = "macos", target_arch = "x86_64"));
        assert_eq!(target_triple().is_some(), has_prebuilt);
        if let Some(t) = target_triple() {
            assert!(published.contains(&t), "unexpected triple {t}");
        }
    }
}
