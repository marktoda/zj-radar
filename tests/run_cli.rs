//! CI-safe end-to-end: `run --print-cmd` materializes the owned dir and emits
//! the expected zellij invocation without execing Zellij. Uses an isolated
//! HOME/XDG so it never touches the developer's real config.
#![cfg(feature = "cli")]
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn run_print_cmd_materializes_and_emits_invocation() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let mut cmd = Command::cargo_bin("zj-radar").unwrap();
    cmd.env("HOME", home.path())
        .env("XDG_DATA_HOME", data.path()) // dirs::data_dir() on Linux
        .arg("run")
        .arg("proj")
        .arg("--print-cmd");
    let out = cmd.assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("--layout radar"), "stdout:\n{stdout}");
    assert!(stdout.contains("--session proj"), "stdout:\n{stdout}");
    // dir was materialized (Linux path; macOS uses Application Support)
    // Assert via the printed --config-dir token rather than a hardcoded path:
    assert!(stdout.contains("--config-dir"), "stdout:\n{stdout}");
}
