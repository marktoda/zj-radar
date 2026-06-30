//! CI-safe end-to-end: `run --print-cmd` materializes the owned dir and emits a
//! Zellij invocation without execing Zellij. Uses an isolated HOME/XDG so it
//! never touches the developer's real config. The precise attach-vs-create shape
//! is covered by `plan_run` unit tests; this smoke test only asserts the binary
//! runs end to end and prints a session-scoped `zellij` command.
#![cfg(feature = "cli")]
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn run_print_cmd_emits_session_scoped_zellij_invocation() {
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
    // Robust across attach vs create branches (depends on host sessions): both
    // print a `zellij` command referencing the resolved session name.
    assert!(stdout.contains("zellij "), "stdout:\n{stdout}");
    assert!(stdout.contains("proj"), "stdout:\n{stdout}");
}
