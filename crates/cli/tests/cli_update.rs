//! Integration tests for `zj-radar update` — the offline paths only. Pinning
//! `ZJ_RADAR_VERSION` skips the GitHub "latest" lookup, and an isolated HOME
//! with no installed wasm skips the checksum fetch, so nothing here touches the
//! network. The download/replace path is covered by unit tests on its pieces.

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

/// This test binary is built from the same package as the CLI, so this is the
/// version the CLI under test reports.
const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[test]
fn update_check_reports_current_cli_and_missing_wasm_without_network() {
    let home = TempDir::new().unwrap();
    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["update", "--check"])
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ZELLIJ_CONFIG_DIR")
        .env("ZJ_RADAR_VERSION", CURRENT)
        .assert()
        .success() // nothing to update → exit 0
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains(&format!("v{CURRENT}")), "should print the current version: {stdout}");
    assert!(stdout.contains("up to date"), "cli line should say up to date: {stdout}");
    assert!(stdout.contains("not installed"), "wasm line should say not installed: {stdout}");
    assert!(stdout.contains("setup zellij --download"), "should point at the install command: {stdout}");
}

#[test]
fn update_check_exits_nonzero_when_a_newer_release_is_pinned() {
    let home = TempDir::new().unwrap();
    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["update", "--check"])
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ZELLIJ_CONFIG_DIR")
        .env("ZJ_RADAR_VERSION", "99.0.0")
        .assert()
        .failure() // an update is available → exit 1, scriptable
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("99.0.0"), "should name the available version: {stdout}");
    assert!(stdout.contains("zj-radar update"), "should point at the command that applies it: {stdout}");
}

#[test]
fn update_refuses_a_pin_older_than_the_running_cli() {
    // `update` only moves forward: refreshing the wasm to an older pin while
    // the CLI stays put would split the two halves across versions.
    let home = TempDir::new().unwrap();
    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .arg("update")
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ZELLIJ_CONFIG_DIR")
        .env("ZJ_RADAR_VERSION", "0.0.1")
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("older"), "should explain the refusal: {stderr}");
    assert!(stderr.contains("install.sh"), "should point at the installer for downgrades: {stderr}");
}

#[cfg(unix)]
#[test]
fn update_leaves_a_symlinked_wasm_to_its_manager() {
    // home-manager installs the wasm as a symlink into /nix/store. Same rule
    // `setup zellij` applies to a symlinked config.kdl: never write through
    // it (the next `home-manager switch` would revert it anyway) — and no
    // checksum comparison either, so this stays offline.
    let home = TempDir::new().unwrap();
    let plugins = home.path().join(".config/zellij/plugins");
    fs::create_dir_all(&plugins).unwrap();
    let store_copy = home.path().join("store").join("zj_radar.wasm");
    fs::create_dir_all(store_copy.parent().unwrap()).unwrap();
    fs::write(&store_copy, b"\0asm").unwrap();
    std::os::unix::fs::symlink(&store_copy, plugins.join("zj_radar.wasm")).unwrap();

    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["update", "--check"])
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ZELLIJ_CONFIG_DIR")
        .env("ZJ_RADAR_VERSION", CURRENT)
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("managed"), "wasm line should say it is managed elsewhere: {stdout}");
    assert!(!stdout.contains("differs"), "{stdout}");
}

#[test]
fn update_with_nothing_newer_does_not_reinstall_a_missing_wasm() {
    let home = TempDir::new().unwrap();
    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .arg("update")
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ZELLIJ_CONFIG_DIR")
        .env("ZJ_RADAR_VERSION", CURRENT)
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("up to date"), "{stdout}");
    // `update` moves what is installed; a missing sidebar is a setup job.
    assert!(stdout.contains("setup zellij --download"), "{stdout}");
    assert!(!home.path().join(".config/zellij/plugins/zj_radar.wasm").exists());
}

#[test]
fn update_refuses_to_overwrite_a_cargo_installed_binary() {
    // Relocate the built CLI into a fake ~/.cargo/bin so `current_exe()`
    // classifies it as cargo-managed, then pin a newer version so the
    // classification is actually consulted.
    let home = TempDir::new().unwrap();
    let cargo_bin = home.path().join(".cargo").join("bin");
    fs::create_dir_all(&cargo_bin).unwrap();
    let relocated = cargo_bin.join("zj-radar");
    fs::copy(assert_cmd::cargo::cargo_bin("zj-radar"), &relocated).unwrap();

    let out = Command::new(&relocated)
        .arg("update")
        .env("HOME", home.path())
        .env_remove("CARGO_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ZELLIJ_CONFIG_DIR")
        .env("ZJ_RADAR_VERSION", "99.0.0")
        .assert()
        .success() // a redirect, not a failure
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("cargo install zj-radar"), "should hand off to cargo: {stdout}");
    // The binary itself is untouched.
    assert_eq!(
        fs::read(&relocated).unwrap(),
        fs::read(assert_cmd::cargo::cargo_bin("zj-radar")).unwrap()
    );
}
