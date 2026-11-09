//! The `zj_radar.exit.v1` pipe: a shell hook reporting that the command it
//! just ran in a pane exited non-zero. No zellij-tile dependency.
//!
//! Zellij reports an exit code only for a *held* pane whose root command
//! exited (`zellij run`); a command run at an ordinary shell prompt just hands
//! the foreground back to the shell. The opt-in shell hook (`zj-radar setup
//! shell`) fills that gap from `precmd`: when `$?` is non-zero it sends one
//! report, and the plugin applies it to the pane's *observed* command row
//! (`CommandStore::on_exit_report`), never to the status store. It is a
//! separate pipe, not a `zj_radar.status.v1` payload, because anything on the
//! status pipe takes over the pane's row (status wins over command in
//! `resolve`) and would hide the pane's later observed commands.
//!
//! Wire shape: `{"v":1,"pane_id":N,"exit":N,"program":"cargo"}`. `program` is
//! the peeled exe basename of the command line ([`program_of_command_line`]):
//! the plugin applies the report only when it names the program of the row
//! it would mark, which drops a late report that lost the race to a newer
//! command.

use crate::command::program_of;

/// The pipe name, and the version authority for its payload.
pub const EXIT_PIPE_NAME: &str = "zj_radar.exit.v1";

/// Payloads larger than this are dropped unparsed. A report is tiny; the cap
/// only bounds a runaway or hostile sender.
pub const MAX_EXIT_PAYLOAD_BYTES: usize = 1024;

/// Cap on the `program` field, in chars. An exe basename longer than this is
/// not one the plugin could have displayed as a row's first token anyway.
pub const MAX_PROGRAM_CHARS: usize = 64;

/// One parsed exit report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitReport {
    pub pane_id: u32,
    /// The command's exit code. Always non-zero after [`parse`]: a zero exit
    /// is no news (the observed lifecycle already renders a success).
    pub exit: i32,
    pub program: String,
}

impl ExitReport {
    /// Serialize for the pipe. `program` is capped the same way [`parse`]
    /// caps it, so a producer cannot send what the plugin would drop.
    pub fn to_wire(&self) -> String {
        let program: String = self.program.chars().take(MAX_PROGRAM_CHARS).collect();
        serde_json::json!({"v": 1, "pane_id": self.pane_id, "exit": self.exit, "program": program}).to_string()
    }
}

/// Parse one `zj_radar.exit.v1` payload. `None` for anything malformed, an
/// unknown `v`, a zero exit code, or an empty / non-printable program.
pub fn parse(raw: &str) -> Option<ExitReport> {
    if raw.len() > MAX_EXIT_PAYLOAD_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if value.get("v")?.as_u64()? != 1 {
        return None;
    }
    let pane_id = u32::try_from(value.get("pane_id")?.as_u64()?).ok()?;
    let exit = i32::try_from(value.get("exit")?.as_i64()?).ok()?;
    if exit == 0 {
        return None;
    }
    let program = value.get("program")?.as_str()?;
    if program.is_empty()
        || program.chars().count() > MAX_PROGRAM_CHARS
        || program.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    Some(ExitReport { pane_id, exit, program: program.to_string() })
}

/// The program a shell command line runs, peeled the way the plugin peels an
/// observed argv (wrappers like `sudo`/`env`/`time`, `VAR=value` prefixes, a
/// path, a login shell's `-`), so it matches the first token of the row the
/// plugin shows for that command. Split on whitespace only: this is a match
/// key, not a parser, and a quoted first word is rare enough to just not
/// match. Empty for an empty line.
pub fn program_of_command_line(line: &str) -> String {
    let argv: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    program_of(&argv).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_wire() {
        let r = ExitReport { pane_id: 7, exit: 101, program: "cargo".into() };
        assert_eq!(parse(&r.to_wire()), Some(r));
    }

    #[test]
    fn rejects_zero_exit_bad_version_and_bad_program() {
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":0,"program":"cargo"}"#), None);
        assert_eq!(parse(r#"{"v":2,"pane_id":1,"exit":1,"program":"cargo"}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":1,"program":""}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":1,"program":"a b"}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":1,"program":"a\u001b"}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":-1,"exit":1,"program":"cargo"}"#), None);
        assert_eq!(parse("not json"), None);
        let long = format!(r#"{{"v":1,"pane_id":1,"exit":1,"program":"{}"}}"#, "x".repeat(2000));
        assert_eq!(parse(&long), None);
    }

    #[test]
    fn negative_exit_codes_parse() {
        let r = parse(r#"{"v":1,"pane_id":3,"exit":-1,"program":"make"}"#).unwrap();
        assert_eq!(r.exit, -1);
    }

    #[test]
    fn program_peels_wrappers_env_prefixes_and_paths() {
        assert_eq!(program_of_command_line("cargo build --release"), "cargo");
        assert_eq!(program_of_command_line("  sudo make install"), "make");
        assert_eq!(program_of_command_line("RUST_LOG=debug cargo test"), "cargo");
        assert_eq!(program_of_command_line("./target/debug/app --flag"), "app");
        assert_eq!(program_of_command_line(""), "");
    }
}
