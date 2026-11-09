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
//! Wire shape: `{"v":1,"pane_id":N,"exit":N,"programs":["cd","cargo"]}`.
//! `programs` holds the peeled exe basename of each segment of the command
//! line, split on `&&`, `||`, `;` and `|` ([`programs_of_command_line`]), so
//! `cd crates/cli && cargo test` still names `cargo`. The plugin applies the
//! report only when one of them is the program of the row it would mark,
//! which drops a late report that lost the race to a newer command.

use crate::command::program_of;

/// The pipe name, and the version authority for its payload.
pub const EXIT_PIPE_NAME: &str = "zj_radar.exit.v1";

/// Payloads larger than this are dropped unparsed. A report is tiny; the cap
/// only bounds a runaway or hostile sender.
pub const MAX_EXIT_PAYLOAD_BYTES: usize = 1024;

/// Cap on the number of `programs`: segments past it are not sent.
pub const MAX_PROGRAMS: usize = 8;

/// Cap on each `programs` entry, in chars. An exe basename longer than this is
/// not one the plugin could have displayed as a row's first token anyway.
pub const MAX_PROGRAM_CHARS: usize = 64;

/// One parsed exit report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitReport {
    pub pane_id: u32,
    /// The command's exit code. Always non-zero after [`parse`]: a zero exit
    /// is no news (the observed lifecycle already renders a success).
    pub exit: i32,
    /// The peeled program of each command-line segment, deduplicated, in
    /// order, never empty after [`parse`].
    pub programs: Vec<String>,
}

impl ExitReport {
    /// Serialize for the pipe. `programs` is capped the same way [`parse`]
    /// caps it, so a producer cannot send what the plugin would drop.
    pub fn to_wire(&self) -> String {
        let programs: Vec<String> = self
            .programs
            .iter()
            .filter(|p| valid_program(p))
            .take(MAX_PROGRAMS)
            .map(|p| p.chars().take(MAX_PROGRAM_CHARS).collect())
            .collect();
        serde_json::json!({"v": 1, "pane_id": self.pane_id, "exit": self.exit, "programs": programs}).to_string()
    }
}

fn valid_program(p: &str) -> bool {
    !p.is_empty() && !p.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Parse one `zj_radar.exit.v1` payload. `None` for anything malformed, an
/// unknown `v`, a zero exit code, or a `programs` list that is empty, too
/// long, or holds an empty / over-long / non-printable entry.
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
    let list = value.get("programs")?.as_array()?;
    if list.is_empty() || list.len() > MAX_PROGRAMS {
        return None;
    }
    let mut programs = Vec::with_capacity(list.len());
    for p in list {
        let p = p.as_str()?;
        if !valid_program(p) || p.chars().count() > MAX_PROGRAM_CHARS {
            return None;
        }
        programs.push(p.to_string());
    }
    Some(ExitReport { pane_id, exit, programs })
}

/// The program a shell command line (or one segment of it) runs, peeled the
/// way the plugin peels an observed argv (wrappers like `sudo`/`env`/`time`,
/// `VAR=value` prefixes, a path, a login shell's `-`), so it matches the
/// first token of the row the plugin shows for that command. Split on
/// whitespace only: this is a match key, not a parser, and a quoted first
/// word is rare enough to just not match. Empty for an empty line.
pub fn program_of_command_line(line: &str) -> String {
    let argv: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    program_of(&argv).to_string()
}

/// The programs a command line runs: the line split on `&&`, `||`, `;`, `|`
/// (and newlines), each segment peeled by [`program_of_command_line`],
/// empties dropped, deduplicated in order, at most [`MAX_PROGRAMS`]. So
/// `cd crates/cli && cargo test` gives `["cd", "cargo"]`. A redirect such as
/// `2>&1` is not a separator (a lone `&` never splits), and bash's `|&`
/// leaves a leading `&` that is trimmed. Quotes are not understood: a
/// separator inside quotes splits too, which at worst adds a name that
/// matches nothing.
pub fn programs_of_command_line(line: &str) -> Vec<String> {
    let mut programs: Vec<String> = Vec::new();
    for segment in line.split("&&").flat_map(|s| s.split(['|', ';', '\n'])) {
        let program = program_of_command_line(segment.trim_start().trim_start_matches('&'));
        if !program.is_empty() && !programs.contains(&program) {
            programs.push(program);
        }
        if programs.len() == MAX_PROGRAMS {
            break;
        }
    }
    programs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_wire() {
        let r = ExitReport { pane_id: 7, exit: 101, programs: vec!["cd".into(), "cargo".into()] };
        assert_eq!(parse(&r.to_wire()), Some(r));
    }

    #[test]
    fn rejects_zero_exit_bad_version_and_bad_program() {
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":0,"programs":["cargo"]}"#), None);
        assert_eq!(parse(r#"{"v":2,"pane_id":1,"exit":1,"programs":["cargo"]}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":1,"programs":[]}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":1,"programs":[""]}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":1,"programs":["cd","a b"]}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":1,"programs":["a\u001b"]}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":1,"programs":[1]}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":1,"exit":1,"program":"cargo"}"#), None);
        assert_eq!(parse(r#"{"v":1,"pane_id":-1,"exit":1,"programs":["cargo"]}"#), None);
        assert_eq!(parse("not json"), None);
        let nine = r#"{"v":1,"pane_id":1,"exit":1,"programs":["a","b","c","d","e","f","g","h","i"]}"#;
        assert_eq!(parse(nine), None);
        let long = format!(r#"{{"v":1,"pane_id":1,"exit":1,"programs":["{}"]}}"#, "x".repeat(2000));
        assert_eq!(parse(&long), None);
    }

    #[test]
    fn negative_exit_codes_parse() {
        let r = parse(r#"{"v":1,"pane_id":3,"exit":-1,"programs":["make"]}"#).unwrap();
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

    #[test]
    fn compound_lines_name_every_segment_program() {
        let p = |l: &str| programs_of_command_line(l);
        assert_eq!(p("cd crates/cli && cargo test"), ["cd", "cargo"]);
        assert_eq!(p("FOO=1 make && ./run"), ["make", "run"]);
        assert_eq!(p("make 2>&1 | tee log"), ["make", "tee"]);
        assert_eq!(p("make |& tee log"), ["make", "tee"]);
        assert_eq!(p("false || sudo make; cargo build;"), ["false", "make", "cargo"]);
        assert_eq!(p("cargo fmt && cargo test"), ["cargo"], "deduplicated");
        assert_eq!(p("   "), Vec::<String>::new());
        assert_eq!(p("a;b;c;d;e;f;g;h;i;j").len(), MAX_PROGRAMS);
    }

    #[test]
    fn to_wire_drops_what_parse_would_refuse() {
        let r = ExitReport { pane_id: 1, exit: 1, programs: vec!["".into(), "make".into()] };
        assert_eq!(parse(&r.to_wire()).unwrap().programs, ["make"]);
    }
}
