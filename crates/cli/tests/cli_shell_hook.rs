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
const EXPECTED: [&str; 2] = ["notify|exit|--code|1|--cmd=false|", "notify|exit|--code|3|--cmd=sh -c 'exit 3'|"];

/// The script line that puts the fake `zj-radar` first on PATH. Set from
/// inside the script, after the shell's startup files ran: a distro's global
/// rc can reset PATH under `env_clear()` (NixOS's `/etc/zshenv` and bash's
/// system rc do, since `__NIXOS_SET_ENVIRONMENT_DONE` is unset), which would
/// drop the shim dir set in the environment and leave nothing recorded.
fn path_line(shell: &str, shims: &ShimDir) -> String {
    let dir = shims.dir.path().display().to_string();
    if shell == "fish" {
        format!("set -gx PATH {dir:?} $PATH; ")
    } else {
        format!("export PATH={dir:?}:\"$PATH\"; ")
    }
}

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
    let script = format!("{}\nsh -c 'exit 3'\ntrue\n\nsh -c 'exit 130'\nfalse\n\n\n", path_line("bash", &shims));
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
    // Drive the hooks the way zsh's line editor does: preexec with the line
    // as typed, size-limited and alias-expanded, the command, then precmd.
    let run = |line: &str| format!("__zj_radar_preexec {line:?} {line:?} {line:?}; {line}; __zj_radar_precmd; ");
    let script = format!(
        "{}source {rc:?}; {}{}__zj_radar_precmd; {}{}",
        path_line("zsh", &shims),
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
    let script = format!(
        "{}source {rc:?}; {}{}{}{}",
        path_line("fish", &shims),
        run("sh -c 'exit 3'"),
        run("true"),
        run("sh -c 'exit 130'"),
        run("false"),
    );
    shell_command("fish", home.path(), &shims).args(["--no-config", "-c", &script]).status().unwrap();
    assert_eq!(calls(&log, 2), EXPECTED);
}

#[test]
fn notify_exit_sends_the_program_on_the_exit_pipe() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["notify", "exit", "--code", "101", "--cmd", "cd x && RUST_LOG=1 sudo cargo build --release"])
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "0")
        .env("ZELLIJ_PANE_ID", "terminal_4")
        .assert()
        .success();
    let argv = shims.sole_pipe_argv();
    assert!(argv.contains("--name zj_radar.exit.v1"), "{argv}");
    for field in [r#""exit":101"#, r#""pane_id":4"#, r#""programs":["cd","cargo"]"#, r#""v":1"#] {
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

#[test]
fn setup_shell_refuses_a_read_only_rc_and_points_at_shell_init() {
    // home-manager links the rc file into the read-only /nix/store: a 0444
    // file in a 0555 directory, reached through a symlink from $HOME.
    use std::os::unix::fs::PermissionsExt;
    let home = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let target = store.path().join("zshrc");
    std::fs::write(&target, "export EDITOR=vim\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    std::os::unix::fs::symlink(&target, home.path().join(".zshrc")).unwrap();

    let out = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "shell", "zsh", "-y"])
        .env("HOME", home.path())
        .env("ZDOTDIR", home.path())
        .output()
        .unwrap();
    std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a refused install fails the command");
    assert!(stderr.contains("read-only") && stderr.contains("zj-radar shell-init zsh"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "export EDITOR=vim\n", "left untouched");
    assert!(std::fs::symlink_metadata(home.path().join(".zshrc")).unwrap().file_type().is_symlink());
}

#[test]
fn shell_init_prints_the_hook_alone() {
    for shell in ["bash", "zsh", "fish"] {
        let out = Command::cargo_bin("zj-radar").unwrap().args(["shell-init", shell]).output().unwrap();
        assert!(out.status.success(), "{shell}");
        let hook = String::from_utf8(out.stdout).unwrap();
        assert!(hook.contains("zj-radar notify exit --code"), "{shell}: {hook}");
        assert!(!hook.contains(">>> zj-radar"), "no rc markers: {shell}");
    }
    Command::cargo_bin("zj-radar").unwrap().args(["shell-init", "tcsh"]).assert().failure();
}

#[test]
fn bash_hook_loaded_through_shell_init_reports_failures() {
    if !have("bash") {
        eprintln!("bash not installed — skipping");
        return;
    }
    let home = TempDir::new().unwrap();
    let shims = ShimDir::new();
    let log = fake_cli(&shims);
    let bin = assert_cmd::cargo::cargo_bin("zj-radar");
    let rc = home.path().join("rc");
    std::fs::write(&rc, format!("eval \"$({bin:?} shell-init bash)\"\n")).unwrap();
    let script = format!("{}\nfalse\n\n", path_line("bash", &shims));
    let mut cmd = shell_command("bash", home.path(), &shims);
    cmd.arg("--rcfile").arg(&rc).arg("-i").stdin(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    }
    child.wait().unwrap();
    assert_eq!(calls(&log, 1), ["notify|exit|--code|1|--cmd=false|"]);
}
