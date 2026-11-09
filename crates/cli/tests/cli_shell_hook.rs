//! The failed-command shell hook end to end: `setup shell` writes the rc
//! block, a real shell runs it, and `notify exit` sends the
//! `zj_radar.exit.v1` report. Each shell's test spawns that shell and is
//! skipped (with a note on stderr) when it isn't installed.

mod support;

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use support::ShimDir;
use tempfile::TempDir;

fn have(shell: &str) -> bool {
    std::process::Command::new(shell)
        .args(["-c", "exit 0"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `setup shell <shell> -y` under an isolated HOME; returns the rc file.
fn install_hook(home: &Path, shell: &str) -> PathBuf {
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "shell", shell, "-y"])
        .env("HOME", home)
        .env("ZDOTDIR", home)
        .env_remove("XDG_CONFIG_HOME")
        .assert()
        .success();
    let rc = match shell {
        "bash" => home.join(".bashrc"),
        "zsh" => home.join(".zshrc"),
        _ => home.join(".config/fish/conf.d/zj-radar.fish"),
    };
    assert!(std::fs::read_to_string(&rc).unwrap().contains("zj-radar notify exit"), "hook written to {rc:?}");
    rc
}

/// A fake `zj-radar` that appends its argv (one `|`-joined line per call)
/// to `<dir>/zj-radar.log`. It never reads stdin: the shell under test reads
/// its script from the same pipe.
fn fake_cli(shims: &ShimDir) -> PathBuf {
    let log = shims.dir.path().join("zj-radar.log");
    let bin = shims.dir.path().join("zj-radar");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s|' \"$a\"; done >> {log:?}\necho >> {log:?}\n"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    log
}

/// The recorded calls once `want` of them have landed (the hook sends in
/// the background), or whatever is there after a short grace.
fn calls(log: &Path, want: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let body = std::fs::read_to_string(log).unwrap_or_default();
        let lines: Vec<String> = body.lines().map(str::to_string).collect();
        if lines.len() >= want || Instant::now() > deadline {
            // Let any unexpected extra send land too. The sends run in the
            // background, so they can land in either order: sort.
            std::thread::sleep(Duration::from_millis(300));
            let mut lines: Vec<String> =
                std::fs::read_to_string(log).unwrap_or_default().lines().map(str::to_string).collect();
            lines.sort();
            return lines;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The calls every shell must produce for: a failing command, a success, an
/// empty line, a Ctrl-C exit (130), and a second failure. Sorted, like
/// [`calls`]' result.
const EXPECTED: [&str; 2] = ["notify|exit|--code|1|--cmd|false|", "notify|exit|--code|3|--cmd|sh -c 'exit 3'|"];

fn shell_command(shell: &str, home: &Path, shims: &ShimDir) -> std::process::Command {
    let mut cmd = std::process::Command::new(shell);
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "0")
        .env("ZELLIJ_PANE_ID", "7")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

#[test]
fn bash_hook_reports_only_failed_commands() {
    if !have("bash") {
        eprintln!("bash not installed — skipping");
        return;
    }
    let home = TempDir::new().unwrap();
    let rc = install_hook(home.path(), "bash");
    let shims = ShimDir::new();
    let log = fake_cli(&shims);
    // An interactive bash reading its commands from a pipe still runs PS0
    // and PROMPT_COMMAND around each line, like a terminal session.
    let script = "sh -c 'exit 3'\ntrue\n\nsh -c 'exit 130'\nfalse\n\n\n";
    let mut cmd = shell_command("bash", home.path(), &shims);
    cmd.arg("--rcfile").arg(&rc).arg("-i").stdin(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    }
    child.wait().unwrap();
    assert_eq!(calls(&log, 2), EXPECTED);
}

#[test]
fn zsh_hook_reports_only_failed_commands() {
    if !have("zsh") {
        eprintln!("zsh not installed — skipping");
        return;
    }
    let home = TempDir::new().unwrap();
    let rc = install_hook(home.path(), "zsh");
    let shims = ShimDir::new();
    let log = fake_cli(&shims);
    // Drive the hooks the way zsh's line editor does: preexec with the line,
    // the command, then precmd.
    let run = |line: &str| format!("__zj_radar_preexec {line:?}; {line}; __zj_radar_precmd; ");
    let script = format!(
        "source {rc:?}; {}{}__zj_radar_precmd; {}{}",
        run("sh -c 'exit 3'"),
        run("true"),
        run("sh -c 'exit 130'"),
        run("false"),
    );
    shell_command("zsh", home.path(), &shims).args(["-f", "-c", &script]).status().unwrap();
    assert_eq!(calls(&log, 2), EXPECTED);
}

#[test]
fn fish_hook_reports_only_failed_commands() {
    if !have("fish") {
        eprintln!("fish not installed — skipping");
        return;
    }
    let home = TempDir::new().unwrap();
    let rc = install_hook(home.path(), "fish");
    let shims = ShimDir::new();
    let log = fake_cli(&shims);
    // fish fires `fish_postexec` after each interactive command line; emit
    // it the same way, right after the command so `$status` is its exit.
    let run = |line: &str| format!("{line}; emit fish_postexec {line:?}; ");
    let script =
        format!("source {rc:?}; {}{}{}{}", run("sh -c 'exit 3'"), run("true"), run("sh -c 'exit 130'"), run("false"),);
    shell_command("fish", home.path(), &shims).args(["--no-config", "-c", &script]).status().unwrap();
    assert_eq!(calls(&log, 2), EXPECTED);
}

#[test]
fn notify_exit_sends_the_program_on_the_exit_pipe() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["notify", "exit", "--code", "101", "--cmd", "RUST_LOG=1 sudo cargo build --release"])
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "0")
        .env("ZELLIJ_PANE_ID", "terminal_4")
        .assert()
        .success();
    let argv = shims.sole_pipe_argv();
    assert!(argv.contains("--name zj_radar.exit.v1"), "{argv}");
    for field in [r#""exit":101"#, r#""pane_id":4"#, r#""program":"cargo""#, r#""v":1"#] {
        assert!(argv.contains(field), "{field} in {argv}");
    }
}

#[test]
fn notify_exit_sends_nothing_for_a_success_or_outside_zellij() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    let bin = || Command::cargo_bin("zj-radar").unwrap();
    bin()
        .args(["notify", "exit", "--code", "0", "--cmd", "make"])
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "0")
        .env("ZELLIJ_PANE_ID", "4")
        .assert()
        .success();
    bin()
        .args(["notify", "exit", "--code", "2", "--cmd", "make"])
        .env("PATH", shims.path_env())
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_PANE_ID")
        .assert()
        .success();
    assert!(shims.recorded("zellij").is_empty());
}

#[test]
fn setup_shell_is_idempotent_and_uninstalls_cleanly() {
    let home = TempDir::new().unwrap();
    let rc = home.path().join(".bashrc");
    std::fs::write(&rc, "export EDITOR=vim\n").unwrap();
    install_hook(home.path(), "bash");
    let once = std::fs::read_to_string(&rc).unwrap();
    install_hook(home.path(), "bash");
    assert_eq!(std::fs::read_to_string(&rc).unwrap(), once, "a second install changes nothing");
    let setup = |args: &[&str]| {
        let mut cmd = Command::cargo_bin("zj-radar").unwrap();
        cmd.arg("setup").args(args).env("HOME", home.path());
        cmd
    };
    setup(&["shell", "bash", "--check"]).assert().success();
    setup(&["shell", "bash", "--uninstall", "-y"]).assert().success();
    assert_eq!(std::fs::read_to_string(&rc).unwrap(), "export EDITOR=vim\n");
    setup(&["shell", "bash", "--check"]).assert().failure();
}
