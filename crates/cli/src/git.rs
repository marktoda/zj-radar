//! Repo name and branch for the wire payload: what `git rev-parse
//! --git-common-dir` and `git branch --show-current` would say about a cwd,
//! resolved natively by walking up to `.git` and reading `HEAD`, with the two
//! `git` spawns kept as the fallback for anything the walk does not
//! positively recognize.
//!
//! Two spawns cost ~11 ms of a hook for two file reads' worth of information.
//! The native path claims only positives — a valid git dir whose `HEAD` it
//! can read — and defers to git for everything else, so a negative is always
//! git's verdict.
//!
//! What the payload carries (unchanged from the spawn-only implementation):
//!
//! - `repo`: the MAIN repo's name from the common dir — `pinky` from
//!   `/…/pinky/.git`, also from any linked worktree of it (whose own directory
//!   name is what `--show-toplevel` would give); `acme` from a bare
//!   `acme.git`. Empty when not in a repo.
//! - `branch`: the checked-out branch, or empty on a detached HEAD (exactly
//!   `git branch --show-current`'s output).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Repo name and branch for `cwd` (see the module docs for the semantics).
pub fn repo_branch(cwd: &str) -> (String, String) {
    native_repo_branch(Path::new(cwd)).unwrap_or_else(|| spawn_repo_branch(cwd))
}

/// Environment that changes where git looks. Any of these set → let git
/// decide; reproducing their semantics is not worth the surface.
const DISCOVERY_ENV: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// The native walk. `Some` only for a positively identified, valid git dir
/// whose `HEAD` parses; `None` means "ask git".
fn native_repo_branch(cwd: &Path) -> Option<(String, String)> {
    if DISCOVERY_ENV.iter().any(|k| std::env::var_os(k).is_some()) {
        return None;
    }
    // Real path, as git's own discovery works from `getcwd()`: a cwd reached
    // through a symlink must name the repo the link points at. Also resolves
    // a relative cwd against the process cwd, like `git -C`.
    let start = std::fs::canonicalize(cwd).ok()?;
    let git_dir = discover(&start)?;
    let common = common_dir(&git_dir)?;
    if !common.join("objects").is_dir() || !common.join("refs").is_dir() {
        return None; // not what git calls a git directory
    }
    if common.join("reftable").is_dir() {
        // reftable-format repos (git ≥ 2.45, `--ref-format=reftable`) keep
        // HEAD as the placeholder `ref: refs/heads/.invalid`; the real refs
        // live in the reftable store. Git's call.
        return None;
    }
    let branch = branch_from_head(&git_dir)?;
    let repo = repo_name_from_common_dir(common.to_str()?)?;
    Some((repo, branch))
}

/// Walk up from `start` to the git dir governing it, git's way: at each
/// level a `.git` entry (directory, or a `gitdir:` file for worktrees and
/// submodules) wins. `None` when nothing is found or a gitfile cannot be
/// resolved. A bare repository (or a
/// cwd inside a `.git` dir) has no `.git` entry and is left to the spawn
/// fallback — rare for an agent, and not worth three extra stats per level
/// on every hook.
fn discover(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let dot_git = dir.join(".git");
        if let Ok(meta) = std::fs::metadata(&dot_git) {
            if meta.is_dir() {
                return Some(dot_git);
            }
            if meta.is_file() {
                return read_gitfile(&dot_git, dir);
            }
        }
        dir = dir.parent()?;
    }
}

/// A `.git` FILE: `gitdir: <path>`, relative to the file's directory.
/// Resolved to its real path, as git does.
fn read_gitfile(gitfile: &Path, base: &Path) -> Option<PathBuf> {
    let body = std::fs::read_to_string(gitfile).ok()?;
    let target = body.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    std::fs::canonicalize(base.join(target)).ok()
}

/// The common dir for a git dir: `<git_dir>/commondir` names it (relative to
/// the git dir) inside a linked worktree; absent that, the git dir is its own
/// common dir.
fn common_dir(git_dir: &Path) -> Option<PathBuf> {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(body) => {
            let rel = body.trim();
            if rel.is_empty() {
                return None;
            }
            std::fs::canonicalize(git_dir.join(rel)).ok()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(git_dir.to_path_buf()),
        Err(_) => None,
    }
}

/// `git branch --show-current` from `HEAD` alone: `ref: refs/heads/<name>` →
/// `<name>`; a detached (hex) HEAD or a symbolic ref outside `refs/heads/` →
/// empty. Anything else — a symlinked HEAD (`core.preferSymlinkRefs`), a body
/// git would reject — defers to git.
fn branch_from_head(git_dir: &Path) -> Option<String> {
    let head = git_dir.join("HEAD");
    if std::fs::symlink_metadata(&head).ok()?.file_type().is_symlink() {
        return None;
    }
    let body = std::fs::read_to_string(&head).ok()?;
    let body = body.trim();
    if let Some(target) = body.strip_prefix("ref:") {
        let target = target.trim();
        if !target.starts_with("refs/") {
            return None;
        }
        return Some(target.strip_prefix("refs/heads/").unwrap_or("").to_string());
    }
    let detached = matches!(body.len(), 40 | 64) && body.bytes().all(|b| b.is_ascii_hexdigit());
    detached.then(String::new)
}

/// Derive the repository NAME from a git "common dir" path — the output of
/// `git rev-parse --git-common-dir` made absolute, or the native walk's
/// equivalent. The common dir always points at the MAIN repo's git dir, even
/// from inside a linked worktree, so this yields the repo name (e.g. `pinky`)
/// rather than the worktree's own directory name (e.g. `reply-register`,
/// which is what `--show-toplevel` returns in a worktree).
///
///   /Users/m/dev/pinky/.git        → "pinky"      (normal checkout or any worktree of it)
///   /Users/m/dev/pinky/.git/       → "pinky"
///   /Users/m/dev/acme.git          → "acme"       (bare repo)
///   .git                           → None         (relative — caller falls back)
fn repo_name_from_common_dir(common_dir: &str) -> Option<String> {
    let trimmed = common_dir.trim().trim_end_matches('/');
    // git < 2.31 doesn't know `--path-format`; rev-parse ECHOES the unknown
    // flag to stdout (exit 0), so the captured "dir" is the flag text plus a
    // relative `.git` on a second line. Require a single-line absolute path —
    // anything else falls back to `--show-toplevel` in the caller.
    if trimmed.is_empty() || !trimmed.starts_with('/') || trimmed.contains('\n') {
        return None;
    }
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if base == ".git" {
        // Repo root is the parent of the ".git" dir.
        let parent = trimmed[..trimmed.len() - base.len()].trim_end_matches('/');
        parent.rsplit('/').find(|s| !s.is_empty()).map(str::to_string)
    } else if let Some(stripped) = base.strip_suffix(".git") {
        // Bare repo "name.git".
        (!stripped.is_empty()).then(|| stripped.to_string())
    } else {
        // Unusual: a common dir not ending in .git — use its basename.
        Some(base.to_string())
    }
}

/// Run `git -C <cwd> <args…>` and return its trimmed stdout, or `None` when git
/// can't be spawned, exits non-zero, or produces only whitespace. The single
/// shape behind every git probe here, so the success/trim/empty handling can't
/// drift across calls.
fn git_output(cwd: &str, args: &[&str]) -> Option<String> {
    let trimmed = Command::new("git")
        .args(["-C", cwd])
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    (!trimmed.is_empty()).then_some(trimmed)
}

/// The fallback: ask git. Resolve the repo name from the COMMON git dir so
/// worktrees report the main repo, not the worktree directory; fall back to
/// `--show-toplevel`'s basename for git versions without `--path-format`
/// (added in 2.31). Empty strings when git fails (not a repo, no git).
fn spawn_repo_branch(cwd: &str) -> (String, String) {
    let repo = git_output(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .and_then(|d| repo_name_from_common_dir(&d))
    .or_else(|| {
        git_output(cwd, &["rev-parse", "--show-toplevel"])
            .map(|p| p.rsplit('/').next().unwrap_or(&p).to_string())
    })
    .unwrap_or_default();
    let branch = git_output(cwd, &["branch", "--show-current"]).unwrap_or_default();
    (repo, branch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // --- repo_name_from_common_dir ---

    #[test]
    fn common_dir_normal_checkout_is_repo_name() {
        // A normal checkout's common dir is "<repo>/.git" → repo basename.
        assert_eq!(
            repo_name_from_common_dir("/Users/m/dev/pinky/.git"),
            Some("pinky".into())
        );
    }

    #[test]
    fn common_dir_bare_repo_strips_dot_git() {
        assert_eq!(
            repo_name_from_common_dir("/srv/git/acme.git"),
            Some("acme".into())
        );
    }

    #[test]
    fn common_dir_relative_or_empty_is_none() {
        // Relative ".git" has no resolvable parent → None (caller falls back).
        assert_eq!(repo_name_from_common_dir(".git"), None);
        assert_eq!(repo_name_from_common_dir(""), None);
        assert_eq!(repo_name_from_common_dir("   "), None);
    }

    #[test]
    fn common_dir_old_git_flag_echo_is_none() {
        // git < 2.31 doesn't know `--path-format`: rev-parse ECHOES the unknown
        // flag to stdout and exits 0, so this exact string is what git_output
        // hands us. It must be rejected (→ --show-toplevel fallback), not
        // surfaced as a repo named "--path-format=absolute".
        assert_eq!(
            repo_name_from_common_dir("--path-format=absolute\n.git"),
            None
        );
        // And any other multi-line or non-absolute output is equally untrusted.
        assert_eq!(repo_name_from_common_dir("/a/.git\n/b/.git"), None);
        assert_eq!(repo_name_from_common_dir("relative/.git"), None);
    }

    // --- native walk over hand-built fixtures (no git needed) ---

    /// A minimal valid git dir: HEAD + objects/ + refs/ — exactly what git's
    /// own `is_git_directory` requires.
    fn make_git_dir(dir: &Path, head: &str) {
        fs::create_dir_all(dir.join("objects")).unwrap();
        fs::create_dir_all(dir.join("refs")).unwrap();
        fs::write(dir.join("HEAD"), head).unwrap();
    }

    /// Repo at `<root>/<name>` on `branch`; returns its work tree.
    fn make_repo(root: &Path, name: &str, head: &str) -> PathBuf {
        let wt = root.join(name);
        make_git_dir(&wt.join(".git"), head);
        wt
    }

    /// A linked worktree of `main_repo` at `<root>/<name>`: `.git` is a
    /// gitfile into `<main>/.git/worktrees/<name>`, which carries its own
    /// HEAD and a `commondir` pointing back — the layout `git worktree add`
    /// produces.
    fn make_worktree(root: &Path, main_repo: &Path, name: &str, head: &str) -> PathBuf {
        let wt_git = main_repo.join(".git").join("worktrees").join(name);
        fs::create_dir_all(&wt_git).unwrap();
        fs::write(wt_git.join("HEAD"), head).unwrap();
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        let wt = root.join(name);
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        wt
    }

    const DETACHED: &str = "3f786850e387550fdab836ed7e6dc881de23001b\n";

    #[test]
    fn native_walk_table() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let pinky = make_repo(root, "pinky", "ref: refs/heads/main\n");
        fs::create_dir_all(pinky.join("src/deep")).unwrap();
        let wt = make_worktree(root, &pinky, "reply-register", "ref: refs/heads/fix/x\n");
        fs::create_dir_all(wt.join("crates")).unwrap();
        let detached = make_repo(root, "detached", DETACHED);
        let bare = root.join("acme.git");
        make_git_dir(&bare, "ref: refs/heads/trunk\n");
        let not_repo = root.join("scratch");
        fs::create_dir_all(&not_repo).unwrap();
        let bisecting = make_repo(root, "bisect", "ref: refs/bisect/bad\n");

        type Case<'a> = (&'a str, PathBuf, Option<(&'a str, &'a str)>);
        let cases: [Case; 9] = [
            ("repo root", pinky.clone(), Some(("pinky", "main"))),
            ("nested subdir", pinky.join("src/deep"), Some(("pinky", "main"))),
            ("worktree root → MAIN repo name, worktree's branch", wt.clone(), Some(("pinky", "fix/x"))),
            ("worktree subdir", wt.join("crates"), Some(("pinky", "fix/x"))),
            ("detached HEAD → empty branch", detached, Some(("detached", ""))),
            ("bare repo → defer to git (no `.git` entry to find)", bare, None),
            ("symbolic ref outside refs/heads → empty branch", bisecting, Some(("bisect", ""))),
            // Negatives are git's call — the walk declines rather than claiming "not a repo".
            ("not a repo → defer to git", not_repo, None),
            ("missing cwd → defer to git", root.join("nope"), None),
        ];
        for (label, cwd, want) in cases {
            let got = native_repo_branch(&cwd);
            let want = want.map(|(r, b)| (r.to_string(), b.to_string()));
            assert_eq!(got, want, "{label} ({})", cwd.display());
        }
    }

    #[test]
    fn native_walk_declines_what_it_cannot_validate() {
        // Each of these is something git handles by its own rules; the walk
        // must return None (spawn git), never a guess.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // A `.git` dir missing objects/refs — git keeps walking; we defer.
        let bogus = root.join("bogus");
        fs::create_dir_all(bogus.join(".git")).unwrap();
        fs::write(bogus.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(native_repo_branch(&bogus), None, "invalid .git dir");

        // HEAD content git would reject.
        let junk = make_repo(root, "junk", "hello\n");
        assert_eq!(native_repo_branch(&junk), None, "unparseable HEAD");

        // A reftable-format repo: HEAD is the `.invalid` placeholder and the
        // branch lives in the reftable store git alone can read.
        let reftable = make_repo(root, "reftable", "ref: refs/heads/.invalid\n");
        fs::create_dir_all(reftable.join(".git/reftable")).unwrap();
        fs::write(reftable.join(".git/reftable/tables.list"), "").unwrap();
        assert_eq!(native_repo_branch(&reftable), None, "reftable repo");

        // A gitfile pointing nowhere.
        let dangling = root.join("dangling");
        fs::create_dir_all(&dangling).unwrap();
        fs::write(dangling.join(".git"), "gitdir: /nonexistent/x\n").unwrap();
        assert_eq!(native_repo_branch(&dangling), None, "dangling gitfile");

        // A symlinked HEAD (core.preferSymlinkRefs) — the file behind it is
        // a sha, which would read as detached; git says the branch name.
        #[cfg(unix)]
        {
            let sym = make_repo(root, "symref", "unused");
            fs::remove_file(sym.join(".git/HEAD")).unwrap();
            fs::create_dir_all(sym.join(".git/refs/heads")).unwrap();
            fs::write(sym.join(".git/refs/heads/main"), DETACHED).unwrap();
            std::os::unix::fs::symlink("refs/heads/main", sym.join(".git/HEAD")).unwrap();
            assert_eq!(native_repo_branch(&sym), None, "symlinked HEAD");
        }
    }

    #[test]
    fn native_walk_follows_a_symlinked_cwd_to_the_real_repo() {
        // git discovers from getcwd(), which is the real path — a cwd reached
        // via a symlink names the repo the link points at, not the link.
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let pinky = make_repo(tmp.path(), "pinky", "ref: refs/heads/main\n");
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(&pinky, &link).unwrap();
            assert_eq!(
                native_repo_branch(&link),
                Some(("pinky".to_string(), "main".to_string()))
            );
        }
    }

    // --- agreement with real git, when one is on PATH ---

    /// Run git hermetically: no global/system config (a developer's
    /// `commit.gpgsign` or `core.hooksPath` must not reach into the fixture —
    /// a pinentry prompt once made this test fail after a 14 s stall).
    fn git(cwd: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"])
            .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
            .current_dir(cwd)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Build real repos with git and check the native answer equals the
    /// spawn answer for every fixture the table covers. Skips silently when
    /// git is not runnable (the hermetic sandbox).
    #[test]
    fn native_agrees_with_git_on_real_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        if !git(root, &["--version"]) {
            eprintln!("skipping: git not runnable");
            return;
        }
        let pinky = root.join("pinky");
        fs::create_dir_all(pinky.join("src/deep")).unwrap();
        assert!(git(&pinky, &["init", "-q"]));
        assert!(git(&pinky, &["commit", "-q", "--allow-empty", "-m", "init"]));
        // Some gits ignore init.defaultBranch from -c; pin the name explicitly.
        assert!(git(&pinky, &["branch", "-M", "main"]));
        let wt = root.join("reply-register");
        assert!(git(&pinky, &["worktree", "add", "-q", "-b", "fix/x", wt.to_str().unwrap()]));
        fs::create_dir_all(wt.join("crates")).unwrap();
        let detached = root.join("detached");
        assert!(git(root, &["clone", "-q", pinky.to_str().unwrap(), "detached"]));
        assert!(git(&detached, &["checkout", "-q", "--detach"]));
        let bare = root.join("acme.git");
        assert!(git(root, &["clone", "-q", "--bare", pinky.to_str().unwrap(), "acme.git"]));

        for cwd in [
            pinky.clone(),
            pinky.join("src/deep"),
            wt.clone(),
            wt.join("crates"),
            detached,
        ] {
            let cwd_s = cwd.to_str().unwrap();
            let native = native_repo_branch(&cwd).unwrap_or_else(|| panic!("native declined a real repo: {cwd_s}"));
            let spawned = spawn_repo_branch(cwd_s);
            assert_eq!(native, spawned, "cwd={cwd_s}");
        }
        // A bare repo is deliberately git's call (no `.git` entry to walk to);
        // the public entry point still answers it, through the spawn.
        let bare_s = bare.to_str().unwrap();
        assert_eq!(native_repo_branch(&bare), None, "bare repos defer to git");
        assert_eq!(repo_branch(bare_s), spawn_repo_branch(bare_s));
        assert_eq!(repo_branch(bare_s).0, "acme");
        // And the positive cases carry the values the table expects.
        assert_eq!(native_repo_branch(&pinky), Some(("pinky".into(), "main".into())));
        assert_eq!(native_repo_branch(&wt), Some(("pinky".into(), "fix/x".into())));
    }
}
