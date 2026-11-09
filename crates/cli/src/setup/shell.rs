//! `zj-radar setup shell [bash|zsh|fish]` — opt-in: a marked block in the
//! shell's rc file that reports a command's non-zero exit to the radar.
//!
//! Zellij exposes no exit code for a command run at an ordinary shell
//! prompt, so the hook sends one `zj-radar notify exit` per *failed* command,
//! detached, from the shell's post-command hook (bash `PROMPT_COMMAND`, zsh
//! `precmd`, fish `fish_postexec`). Successful commands send nothing. The
//! plugin applies the report to the pane's observed command row only, and
//! only when the row's program matches (`CommandStore::on_exit_report`), so
//! the hook itself needs no command list: agents never have a row, and a
//! command that failed inside the debounce window never earned one. The hook
//! skips Ctrl-C/Ctrl-Z exits (130, 148) and does nothing outside Zellij.
//!
//! Never part of the bare `setup` sweep: it edits the user's own rc files.
//! The block sits between [`SHELL_BLOCK_BEGIN`] and [`SHELL_BLOCK_END`];
//! install rewrites a stale block in place, uninstall removes it.

use super::*;

/// First line of the managed rc block — the install/uninstall marker.
pub(crate) const SHELL_BLOCK_BEGIN: &str = "# >>> zj-radar shell hook >>>";
/// Last line of the managed rc block.
pub(crate) const SHELL_BLOCK_END: &str = "# <<< zj-radar shell hook <<<";

/// The shells `setup shell` supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    pub(crate) const ALL: [Shell; 3] = [Shell::Bash, Shell::Zsh, Shell::Fish];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Shell> {
        Shell::ALL.into_iter().find(|s| s.name() == name)
    }

    /// The hook body (between the markers).
    pub(crate) fn hook(self) -> &'static str {
        match self {
            Shell::Bash => BASH_HOOK,
            Shell::Zsh => ZSH_HOOK,
            Shell::Fish => FISH_HOOK,
        }
    }
}

// bash has no preexec: `PS0` (bash ≥ 4.4) is expanded in the shell itself
// right before a command line runs, and an arithmetic assignment inside it
// (the `${v:0:$((v=1,0))}` idiom, which expands to nothing) marks "a command
// ran". So an empty line (which keeps the previous `$?`) never re-reports. The
// command line comes from `history 1`, read only after a failure. A command
// kept out of history (`HISTCONTROL=ignorespace`, `HISTIGNORE`) reports
// the previous line instead, and the plugin's program-name match drops it.
// The precmd function runs FIRST in PROMPT_COMMAND and returns the status it
// found, so prompt hooks after it still see `$?`.
const BASH_HOOK: &str = r#"if [ -n "${ZELLIJ_PANE_ID:-}" ] && [ -z "${__zj_radar_hooked:-}" ]; then
  __zj_radar_hooked=1
  __zj_radar_ran=0
  __zj_radar_precmd() {
    local s=$? line
    if [ "$__zj_radar_ran" = 1 ] && [ "$s" -ne 0 ] && [ "$s" -ne 130 ] && [ "$s" -ne 148 ]; then
      line=$(HISTTIMEFORMAT= builtin history 1)
      [[ $line =~ ^[[:space:]]*[0-9]+\*?[[:space:]]+(.*)$ ]] && line=${BASH_REMATCH[1]}
      ( command zj-radar notify exit --code "$s" --cmd "$line" >/dev/null 2>&1 & )
    fi
    __zj_radar_ran=0
    return "$s"
  }
  PS0="${PS0:-}"'${__zj_radar_ran:0:$((__zj_radar_ran=1,0))}'
  PROMPT_COMMAND="__zj_radar_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi"#;

// zsh's preexec receives the command line as typed; precmd sees its `$?`.
// `&!` starts the send already disowned (no job notice at the next prompt).
const ZSH_HOOK: &str = r#"if [[ -n "${ZELLIJ_PANE_ID:-}" && -z "${__zj_radar_hooked:-}" ]]; then
  __zj_radar_hooked=1
  typeset -g __zj_radar_cmd=
  __zj_radar_preexec() { __zj_radar_cmd=$1 }
  __zj_radar_precmd() {
    local s=$?
    if [[ -n $__zj_radar_cmd && $s -ne 0 && $s -ne 130 && $s -ne 148 ]]; then
      command zj-radar notify exit --code "$s" --cmd "$__zj_radar_cmd" >/dev/null 2>&1 &!
    fi
    __zj_radar_cmd=
    return $s
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook preexec __zj_radar_preexec
  add-zsh-hook precmd __zj_radar_precmd
fi"#;

// fish fires `fish_postexec` with the command line as `$argv[1]` and the
// command's `$status` intact; an empty line fires nothing.
const FISH_HOOK: &str = r#"if set -q ZELLIJ_PANE_ID; and not set -q __zj_radar_hooked
    set -g __zj_radar_hooked 1
    function __zj_radar_postexec --on-event fish_postexec
        set -l s $status
        if test $s -ne 0 -a $s -ne 130 -a $s -ne 148
            command zj-radar notify exit --code $s --cmd "$argv[1]" >/dev/null 2>&1 &
            disown 2>/dev/null
        end
    end
end"#;

/// The full managed block for `shell`, markers included, newline-terminated.
pub(crate) fn shell_block(shell: Shell) -> String {
    format!(
        "{SHELL_BLOCK_BEGIN}\n# Managed by `zj-radar setup shell`: marks a failed command red in the radar.\n\
         # Remove with `zj-radar setup shell {} --uninstall`.\n{}\n{SHELL_BLOCK_END}\n",
        shell.name(),
        shell.hook()
    )
}

/// The rc file the block goes in: `~/.bashrc`, `${ZDOTDIR:-~}/.zshrc`, or
/// fish's auto-sourced `${XDG_CONFIG_HOME:-~/.config}/fish/conf.d/zj-radar.fish`.
/// Pure (env passed in) so the precedence is unit-tested.
pub(crate) fn rc_path_from(
    shell: Shell,
    home: Option<&str>,
    zdotdir: Option<&str>,
    xdg_config: Option<&str>,
) -> Option<PathBuf> {
    let home = home.filter(|h| !h.is_empty()).map(PathBuf::from);
    match shell {
        Shell::Bash => home.map(|h| h.join(".bashrc")),
        Shell::Zsh => zdotdir.filter(|d| !d.is_empty()).map(PathBuf::from).or(home).map(|d| d.join(".zshrc")),
        Shell::Fish => xdg_config
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".config")))
            .map(|d| d.join("fish").join("conf.d").join("zj-radar.fish")),
    }
}

fn rc_path(shell: Shell) -> Option<PathBuf> {
    let var = |k: &str| std::env::var(k).ok();
    rc_path_from(shell, var("HOME").as_deref(), var("ZDOTDIR").as_deref(), var("XDG_CONFIG_HOME").as_deref())
}

/// Pure editor: `existing` with our block installed (replacing a stale one in
/// place) or removed. `Unchanged` when there is nothing to do. A begin marker
/// without its end marker is refused: we can't tell where our block stops.
pub(crate) fn edit_rc(existing: &str, block: &str, install: bool) -> Result<Outcome, String> {
    let lines: Vec<&str> = existing.split_inclusive('\n').collect();
    let begin = lines.iter().position(|l| l.trim_end() == SHELL_BLOCK_BEGIN);
    let span = match begin {
        None => None,
        Some(b) => match lines[b..].iter().position(|l| l.trim_end() == SHELL_BLOCK_END) {
            Some(len) => Some((b, b + len + 1)),
            None => return Err(format!("found `{SHELL_BLOCK_BEGIN}` without `{SHELL_BLOCK_END}`; fix it by hand")),
        },
    };
    let new = match (span, install) {
        (None, false) => return Ok(Outcome::Unchanged),
        (None, true) => {
            let mut s = existing.to_string();
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(block);
            s
        }
        (Some((b, e)), install) => {
            let mut s: String = lines[..b].concat();
            if install {
                s.push_str(block);
            } else if s.ends_with("\n\n") {
                // Drop the blank separator line install added before the block.
                s.pop();
            }
            s.push_str(&lines[e..].concat());
            s
        }
    };
    if new == existing {
        Ok(Outcome::Unchanged)
    } else {
        Ok(Outcome::Changed(new))
    }
}

/// The shells named on the command line (`bash`/`zsh`/`fish`), or — for a
/// bare `shell` target — the login shell from `$SHELL`.
pub(crate) fn shells_for(named: &[String], login_shell: Option<&str>) -> Result<Vec<Shell>, String> {
    let picked: Vec<Shell> = Shell::ALL.into_iter().filter(|s| named.iter().any(|n| n == s.name())).collect();
    if !picked.is_empty() {
        return Ok(picked);
    }
    let name = login_shell.map(|s| s.rsplit('/').next().unwrap_or(s)).unwrap_or("");
    Shell::from_name(name).map(|s| vec![s]).ok_or_else(|| {
        format!(
            "can't tell which shell to hook from $SHELL ({}); name one: `zj-radar setup shell bash|zsh|fish`",
            if name.is_empty() { "unset" } else { name }
        )
    })
}

/// `--check` for one shell: is our block in its rc file, and current?
/// Returns whether something is missing.
pub(crate) fn check_shell(shell: Shell) -> bool {
    let label = format!("shell ({})", shell.name());
    let Some(path) = rc_path(shell) else {
        println!("{label}: ✗ can't resolve the rc file (set $HOME)");
        return true;
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    match edit_rc(&existing, &shell_block(shell), true) {
        Ok(Outcome::Unchanged) => {
            println!("{label}: ✓ hook installed in {}", path.display());
            false
        }
        Ok(_) if existing.contains(SHELL_BLOCK_BEGIN) => {
            println!(
                "{label}: ✗ hook in {} is out of date — run `zj-radar setup shell {}`",
                path.display(),
                shell.name()
            );
            true
        }
        Ok(_) => {
            println!(
                "{label}: ✗ hook not installed in {} — run `zj-radar setup shell {}`",
                path.display(),
                shell.name()
            );
            true
        }
        Err(e) => {
            println!("{label}: ✗ {} — {e}", path.display());
            true
        }
    }
}

/// Install or uninstall the hook for one shell.
pub(crate) fn setup_shell(shell: Shell, uninstall: bool, opts: BridgeSetupOpts) {
    let label = format!("shell ({})", shell.name());
    let Some(path) = rc_path(shell) else {
        crate::exit::fail_report(&label, "skipped — set $HOME so the rc file can be resolved");
        return;
    };
    // Write through a symlinked rc file (dotfile managers) rather than
    // replacing the link with a regular file.
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            crate::exit::fail_report(&label, format!("refused — can't read {}: {e}", path.display()));
            return;
        }
    };
    let Some(outcome) = edit_or_report(&label, edit_rc(&existing, &shell_block(shell), !uninstall)) else {
        return;
    };
    let new = match outcome {
        Outcome::Changed(new) => new,
        Outcome::Unchanged | Outcome::Conflict => {
            if uninstall {
                println!("{label}: hook not installed ({})", path.display());
            } else {
                println!("{label}: hook already up to date ({})", path.display());
            }
            return;
        }
    };
    if opts.dry_run {
        println!("--- {} (dry-run) ---\n{new}", path.display());
        return;
    }
    let verb = if uninstall { "Remove the zj-radar hook from" } else { "Add the zj-radar hook to" };
    let consent = Consent { yes: opts.yes, is_tty: opts.is_tty, explicit: true };
    let parent = path.parent().map(Path::to_path_buf);
    let written =
        confirm_and_write(&label, &path, &new, consent, &format!("{verb} {}?", path.display()), move || match parent {
            Some(dir) => std::fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display())),
            None => Ok(()),
        });
    if written {
        if uninstall {
            println!("{label}: removed the hook from {} — open a new shell", path.display());
        } else {
            println!("{label}: hooked {} — open a new shell (or re-source it) in a Zellij pane", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(r: Result<Outcome, String>) -> String {
        match r {
            Ok(Outcome::Changed(s)) => s,
            other => panic!("expected a change, got {other:?}"),
        }
    }

    #[test]
    fn install_appends_once_and_uninstall_restores_the_file() {
        let block = shell_block(Shell::Bash);
        let original = "alias ll='ls -l'\nexport EDITOR=vim";
        let installed = changed(edit_rc(original, &block, true));
        assert!(installed.starts_with("alias ll='ls -l'\nexport EDITOR=vim\n\n# >>> zj-radar"));
        assert!(matches!(edit_rc(&installed, &block, true), Ok(Outcome::Unchanged)), "idempotent");
        let removed = changed(edit_rc(&installed, &block, false));
        assert_eq!(removed, "alias ll='ls -l'\nexport EDITOR=vim\n");
        assert!(matches!(edit_rc(&removed, &block, false), Ok(Outcome::Unchanged)));
    }

    #[test]
    fn install_into_an_empty_file_is_just_the_block() {
        let block = shell_block(Shell::Zsh);
        assert_eq!(changed(edit_rc("", &block, true)), block);
        assert_eq!(changed(edit_rc(&block, &block, false)), "");
    }

    #[test]
    fn a_stale_block_is_replaced_in_place_keeping_what_follows() {
        let block = shell_block(Shell::Fish);
        let stale = format!("a\n{SHELL_BLOCK_BEGIN}\nold\n{SHELL_BLOCK_END}\nb\n");
        assert_eq!(changed(edit_rc(&stale, &block, true)), format!("a\n{block}b\n"));
    }

    #[test]
    fn an_unterminated_block_is_refused() {
        let broken = format!("{SHELL_BLOCK_BEGIN}\nstuff\n");
        assert!(edit_rc(&broken, &shell_block(Shell::Bash), true).is_err());
        assert!(edit_rc(&broken, &shell_block(Shell::Bash), false).is_err());
    }

    #[test]
    fn rc_paths_honor_zdotdir_and_xdg() {
        let p = |s, z, x| rc_path_from(s, Some("/h"), z, x).unwrap();
        assert_eq!(p(Shell::Bash, None, None), PathBuf::from("/h/.bashrc"));
        assert_eq!(p(Shell::Zsh, None, None), PathBuf::from("/h/.zshrc"));
        assert_eq!(p(Shell::Zsh, Some("/z"), None), PathBuf::from("/z/.zshrc"));
        assert_eq!(p(Shell::Fish, None, None), PathBuf::from("/h/.config/fish/conf.d/zj-radar.fish"));
        assert_eq!(p(Shell::Fish, None, Some("/x")), PathBuf::from("/x/fish/conf.d/zj-radar.fish"));
        assert_eq!(rc_path_from(Shell::Bash, None, None, None), None);
    }

    #[test]
    fn shells_come_from_the_targets_or_the_login_shell() {
        let named = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(shells_for(&named(&["shell", "zsh", "fish"]), None), Ok(vec![Shell::Zsh, Shell::Fish]));
        assert_eq!(shells_for(&named(&["shell"]), Some("/usr/bin/bash")), Ok(vec![Shell::Bash]));
        assert!(shells_for(&named(&["shell"]), Some("/bin/tcsh")).is_err());
        assert!(shells_for(&named(&["shell"]), None).is_err());
    }

    #[test]
    fn every_hook_skips_interrupts_and_sends_on_the_exit_pipe_command() {
        for shell in Shell::ALL {
            let hook = shell.hook();
            assert!(hook.contains("zj-radar notify exit --code"), "{shell:?}");
            assert!(hook.contains("130") && hook.contains("148"), "{shell:?}");
            assert!(hook.contains("ZELLIJ_PANE_ID"), "{shell:?}");
        }
    }
}
