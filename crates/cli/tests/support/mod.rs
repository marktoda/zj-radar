#![allow(dead_code)] // shared across test binaries; each uses a subset

use std::ffi::OsString;
use std::fs;
use tempfile::TempDir;

pub struct ShimDir {
    pub dir: TempDir,
}

#[derive(Debug)]
pub struct Recorded {
    pub args: Vec<String>,
    pub stdin: String,
}

impl ShimDir {
    pub fn new() -> Self {
        ShimDir {
            dir: TempDir::new().unwrap(),
        }
    }

    /// Install a fake `name` binary that records argv + stdin to
    /// `<dir>/<name>.log` (one tab-separated line per invocation) and exits 0.
    ///
    /// All shims are POSIX `#!/bin/sh`: the hermetic Nix sandbox has no
    /// `/usr/bin/env` (nor bash), and a shim that fails to exec makes
    /// `notify`'s spawn a silent no-op — the suite then fails with zero
    /// recorded broadcasts instead of pointing at the real problem.
    pub fn add_recorder(&self, name: &str) {
        let log = self.dir.path().join(format!("{name}.log"));
        let script = format!(
            "#!/bin/sh\nstdin=\"$(cat)\"\n\
             printf '%s\\t%s\\n' \"$*\" \"$(printf '%s' \"$stdin\" | tr '\\n' ' ')\" >> {log:?}\nexit 0\n",
            log = log
        );
        let bin = self.dir.path().join(name);
        fs::write(&bin, script).unwrap();
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
    }

    /// Install a fake `name` binary that records argv like `add_recorder`, then
    /// hangs for `secs` — models a `zellij pipe` blocked by a wedged plugin
    /// (Zellij's CLI-pipe backpressure). `exec` so the shim process IS the
    /// sleeper: a kill from the code under test must reap the hung process
    /// itself, not an intermediate shell.
    #[allow(dead_code)] // each tests/*.rs is its own crate; not all use this
    pub fn add_hanging_recorder(&self, name: &str, secs: u32) {
        let log = self.dir.path().join(format!("{name}.log"));
        let script = format!(
            "#!/bin/sh\nprintf '%s\\t\\n' \"$*\" >> {log:?}\nexec sleep {secs}\n",
            log = log
        );
        let bin = self.dir.path().join(name);
        fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
    }

    /// Install a fake `git` that answers the `-C <cwd>` rev-parse/branch calls
    /// that `cli/notify.rs` makes.
    ///
    /// The CLI invokes:
    ///   `git -C <cwd> rev-parse --show-toplevel`  → repo toplevel path
    ///   `git -C <cwd> branch --show-current`      → branch name
    ///
    /// `$3` is the subcommand after `-C <cwd>`.
    pub fn add_fake_git(&self, repo_toplevel: &str, branch: &str) {
        let script = format!(
            "#!/bin/sh\n\
             # $1=-C  $2=<cwd>  $3=rev-parse|branch  $4=--show-toplevel|--show-current\n\
             case \"$3 $4\" in\n\
               'rev-parse --show-toplevel') echo {repo:?};;\n\
               'branch --show-current') echo {branch:?};;\n\
               *) exit 0;;\nesac\n",
            repo = repo_toplevel,
            branch = branch
        );
        let bin = self.dir.path().join("git");
        fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
    }

    /// PATH value with this shim dir prepended.
    pub fn path_env(&self) -> OsString {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut p = self.dir.path().as_os_str().to_owned();
        p.push(":");
        p.push(existing);
        p
    }

    /// Parse the log written by `add_recorder`. Each line is `args\tstdin`.
    pub fn recorded(&self, name: &str) -> Vec<Recorded> {
        let log = self.dir.path().join(format!("{name}.log"));
        let body = fs::read_to_string(&log).unwrap_or_default();
        body.lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let mut parts = l.splitn(2, '\t');
                Recorded {
                    args: parts
                        .next()
                        .unwrap_or("")
                        .split_whitespace()
                        .map(String::from)
                        .collect(),
                    stdin: parts.next().unwrap_or("").to_string(),
                }
            })
            .collect()
    }
}
