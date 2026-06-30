//! Agent intake — the host-side adapters that turn an agent's hook payload into
//! a Radar status update. Each agent is a peer adapter behind one seam,
//! `Agent::derive(&Intake) -> Option<AgentUpdate>`, so `notify::run` stays a
//! thin, agent-agnostic shell: read input → derive → broadcast.
//!
//! This is the PUSHED modality of the information-source model (see CONTEXT.md):
//! instrumented agents report rich status through the status contract. The
//! OBSERVED modality — uninstrumented commands like `cargo test`, classified by
//! `command.rs::command_source` inside the plugin — is a sibling, not part of
//! this seam. Both converge on the `Kind`/`source` vocabulary.

mod claude;
mod codex;

use crate::status::Status;
use serde_json::Value;

/// Everything an adapter needs to derive an update. Gathered by `run()` so the
/// adapters stay pure — no stdin/env/IO lives behind the seam, which keeps each
/// `derive` a directly-testable `&Intake -> Option<AgentUpdate>` function.
pub struct Intake<'a> {
    /// The hook payload — sourced from argv `input` or stdin by `run()`.
    pub raw: &'a str,
    /// An explicit status the hook chose to pass, overriding event derivation.
    /// Claude's matcher-driven `hooks.json` uses this; other agents may ignore it.
    pub status_arg: Option<&'a str>,
}

/// The agent-derived result that feeds the wire: everything `to_wire` needs that
/// comes out of the hook payload. `None` from a derivation means no-op.
#[derive(Debug, PartialEq, Eq)]
pub struct AgentUpdate {
    pub status: Status,
    pub msg: String,
    /// Session cwd from the payload, if present. `run()` applies the fallback.
    pub cwd: Option<String>,
}

/// The closed set of push-reporter agents. A variant's [`Agent::source`] string
/// is the single vocabulary shared across the CLI argument, the wire `source`
/// field, and `Kind::from_source` — pinned by `source_round_trips_through_kind`.
/// Adding an agent is a compiler-guided enum variant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    /// Every push-reporter agent, in declaration order. Lets the coherence
    /// guards iterate the variants without re-typing the list (mirrors
    /// `Kind::ALL`). Test-only today — its sole consumers are the guard tests.
    #[cfg(test)]
    pub const ALL: &'static [Agent] = &[Agent::Claude, Agent::Codex];

    /// Parse the `notify <agent>` CLI argument. Inverse of [`Agent::source`].
    pub fn from_cli(s: &str) -> Option<Agent> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            _ => None,
        }
    }

    /// The wire `source` string. MUST round-trip through `Kind::from_source` to
    /// this agent's `Kind` (never `Kind::Other`) — see the guard test.
    pub fn source(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    /// Derive this agent's status update from the intake, or `None` for no-op.
    pub fn derive(self, intake: &Intake) -> Option<AgentUpdate> {
        match self {
            Agent::Claude => claude::derive(intake),
            Agent::Codex => codex::derive(intake),
        }
    }
}

/// Short present-tense activity string for a tool invocation, or None if the
/// tool has no useful activity phrasing. Shared by every agent adapter.
pub fn tool_activity(tool_name: &str, tool_input: &Value) -> Option<String> {
    match tool_name {
        "Edit" | "Write" | "MultiEdit" => {
            let path = tool_input.get("file_path")?.as_str()?;
            basename(path).map(|base| format!("editing {base}"))
        }
        "NotebookEdit" => {
            let path = tool_input.get("notebook_path")?.as_str()?;
            basename(path).map(|base| format!("editing {base}"))
        }
        "Read" => {
            let path = tool_input.get("file_path")?.as_str()?;
            basename(path).map(|base| format!("reading {base}"))
        }
        "Grep" | "Glob" => Some("searching".to_string()),
        "WebFetch" | "WebSearch" => Some("searching web".to_string()),
        "Task" => Some("delegating".to_string()),
        "TodoWrite" => Some("planning".to_string()),
        "apply_patch" => Some("editing files".to_string()),
        "Bash" => bash_activity(tool_input),
        name if name.starts_with("mcp__") => {
            let tool = name.rsplit("__").next().filter(|s| !s.is_empty())?;
            Some(format!("using {tool}"))
        }
        _ => None,
    }
}

fn basename(path: &str) -> Option<&str> {
    if path.is_empty() {
        return None;
    }
    path.rsplit('/').next().filter(|base| !base.is_empty())
}

/// Whole-word containment: is `word` present in `haystack` bounded by non
/// `[a-z0-9]` characters (or string edges)? Used instead of a bare substring so
/// `latest`/`uninstall`/`fastest`/`rebuild` don't trip the test/install/build
/// verbs. `haystack` is assumed already lowercased; `word` is a lowercase
/// literal (a multi-word phrase like `git push` works — its inner space is a
/// boundary char, not a `[a-z0-9]`). Mirrors `contains_word` in notify.sh.
fn contains_word(haystack: &str, word: &str) -> bool {
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_ascii_alphanumeric());
    haystack.match_indices(word).any(|(i, _)| {
        boundary(haystack[..i].chars().next_back())
            && boundary(haystack[i + word.len()..].chars().next())
    })
}

fn bash_activity(tool_input: &Value) -> Option<String> {
    let cmd = tool_input.get("command")?.as_str()?;
    let cmd_lower = cmd.to_lowercase();
    if cmd.trim().is_empty() {
        return None;
    }
    let has = |w: &str| contains_word(&cmd_lower, w);
    if has("git push") {
        Some("pushing".to_string())
    } else if has("git commit") {
        Some("committing".to_string())
    } else if has("git pull") || has("git fetch") {
        Some("syncing".to_string())
    } else if has("test") {
        Some("running tests".to_string())
    } else if has("build") || has("compile") {
        Some("building".to_string())
    } else if has("install") {
        Some("installing".to_string())
    } else {
        let first_token = cmd.split_whitespace().next()?;
        let base = first_token.rsplit('/').next().unwrap_or(first_token);
        if base.is_empty() {
            return None;
        }
        Some(format!("running {base}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::Kind;

    // ── Coherence guards: the agent vocabulary is a subset of `Kind` ──────────

    /// The wire `source` an agent broadcasts MUST map back to its own `Kind`
    /// (never `Kind::Other`) — otherwise the agent renders with the generic mark.
    /// This pins the host-side `Agent` enum to the wasm-side `Kind` enum across a
    /// boundary they can't reference in code. A new `Agent` variant forces a new
    /// match arm here.
    #[test]
    fn source_round_trips_through_kind() {
        for &agent in Agent::ALL {
            let expected = match agent {
                Agent::Claude => Kind::Claude,
                Agent::Codex => Kind::Codex,
            };
            assert_eq!(
                Kind::from_source(agent.source()),
                expected,
                "{agent:?} source {:?} must map to its Kind, not Other",
                agent.source()
            );
        }
    }

    #[test]
    fn from_cli_is_inverse_of_source() {
        for &agent in Agent::ALL {
            assert_eq!(Agent::from_cli(agent.source()), Some(agent));
        }
        assert_eq!(Agent::from_cli("gemini"), None);
        assert_eq!(Agent::from_cli("Claude"), None); // case-sensitive
        assert_eq!(Agent::from_cli(""), None);
    }

    /// The command observer suppresses an exe from command-tracking iff that exe
    /// is a push-reporter agent (`command.rs::AGENT_NAMES`). If the two sets
    /// drift, an exe in `AGENT_NAMES` with no adapter goes dark (suppressed AND
    /// never pushed) — the original Gemini bug. This pins them across a feature
    /// boundary they can't reference in code: command.rs has no `cli` feature, so
    /// it can't see `Agent`, and `cli` can't see into the wasm plugin's runtime.
    #[test]
    fn agent_names_match_push_adapter_sources() {
        use std::collections::BTreeSet;
        let suppressed: BTreeSet<&str> = crate::command::AGENT_NAMES.iter().copied().collect();
        let adapters: BTreeSet<&str> = Agent::ALL.iter().map(|a| a.source()).collect();
        assert_eq!(
            suppressed, adapters,
            "command.rs::AGENT_NAMES must equal the push-adapter source set — \
             an agent in one but not the other either goes dark or flickers"
        );
    }

    // ── tool_activity ─────────────────────────────────────────────────────────

    fn json(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn tool_edit_write_multiedit_reduce_to_basename() {
        for tool in &["Edit", "Write", "MultiEdit"] {
            let input = json(r#"{"file_path": "/path/to/auth.rs"}"#);
            assert_eq!(
                tool_activity(tool, &input).unwrap(),
                "editing auth.rs",
                "tool={tool}"
            );
        }
    }

    #[test]
    fn tool_read_reduces_to_basename() {
        let input = json(r#"{"file_path": "/some/deep/path/mod.rs"}"#);
        assert_eq!(tool_activity("Read", &input).unwrap(), "reading mod.rs");
    }

    #[test]
    fn tool_notebook_edit_uses_notebook_path() {
        let input = json(r#"{"notebook_path": "/notebooks/analysis.ipynb"}"#);
        assert_eq!(
            tool_activity("NotebookEdit", &input).unwrap(),
            "editing analysis.ipynb"
        );
    }

    #[test]
    fn tool_grep_and_glob_return_searching() {
        assert_eq!(
            tool_activity("Grep", &json(r#"{"pattern": "foo"}"#)).unwrap(),
            "searching"
        );
        assert_eq!(
            tool_activity("Glob", &json(r#"{"pattern": "*.rs"}"#)).unwrap(),
            "searching"
        );
    }

    #[test]
    fn tool_webfetch_and_websearch_return_searching_web() {
        assert_eq!(
            tool_activity("WebFetch", &json(r#"{"url": "https://example.com"}"#)).unwrap(),
            "searching web"
        );
        assert_eq!(
            tool_activity("WebSearch", &json(r#"{"query": "rust async"}"#)).unwrap(),
            "searching web"
        );
    }

    #[test]
    fn tool_task_returns_delegating() {
        assert_eq!(
            tool_activity("Task", &json(r#"{"description": "do X"}"#)).unwrap(),
            "delegating"
        );
    }

    #[test]
    fn tool_todowrite_returns_planning() {
        assert_eq!(
            tool_activity("TodoWrite", &json(r#"{"todos": []}"#)).unwrap(),
            "planning"
        );
    }

    #[test]
    fn tool_bash_git_push() {
        let input = json(r#"{"command": "git push origin main"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "pushing");
    }

    #[test]
    fn tool_bash_git_commit() {
        let input = json(r#"{"command": "git commit -m x"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "committing");
    }

    #[test]
    fn tool_bash_git_pull() {
        let input = json(r#"{"command": "git pull"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "syncing");
    }

    #[test]
    fn tool_bash_cargo_test() {
        let input = json(r#"{"command": "cargo test --features cli"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "running tests");
    }

    #[test]
    fn tool_bash_npm_run_build() {
        let input = json(r#"{"command": "npm run build"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "building");
    }

    #[test]
    fn tool_bash_pip_install() {
        let input = json(r#"{"command": "pip install foo"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "installing");
    }

    #[test]
    fn tool_bash_classification_is_word_bounded_not_substring() {
        // The verb match must be whole-word, not a substring, or innocent
        // commands misclassify. Each of these embeds a keyword inside another
        // word and must fall through to the generic "running <exe>".
        for (cmd, expected) in [
            ("git checkout latest", "running git"), // "latest" ⊅ test
            ("npm uninstall left-pad", "running npm"), // "uninstall" ⊅ install
            ("cat fastest.txt", "running cat"),     // "fastest" ⊅ test
            ("./rebuilder.sh", "running rebuilder.sh"), // "rebuilder" ⊅ build
        ] {
            let input = json(&format!(r#"{{"command": {cmd:?}}}"#));
            assert_eq!(
                tool_activity("Bash", &input).as_deref(),
                Some(expected),
                "cmd={cmd}"
            );
        }
    }

    #[test]
    fn tool_bash_path_stripped_to_basename() {
        let input = json(r#"{"command": "/usr/bin/ls -la"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "running ls");
    }

    #[test]
    fn tool_bash_empty_command_returns_none() {
        let input = json(r#"{"command": "   "}"#);
        assert!(tool_activity("Bash", &input).is_none());
    }

    #[test]
    fn tool_apply_patch_returns_editing_files() {
        let input = json(r#"{"command": "apply_patch <<'PATCH'\n*** Begin Patch\nPATCH"}"#);
        assert_eq!(
            tool_activity("apply_patch", &input).unwrap(),
            "editing files"
        );
    }

    #[test]
    fn tool_mcp_uses_tool_basename() {
        assert_eq!(
            tool_activity("mcp__filesystem__read_file", &json("{}")).unwrap(),
            "using read_file"
        );
    }

    #[test]
    fn tool_unknown_returns_none() {
        assert!(tool_activity("SomeFutureTool", &json("{}")).is_none());
    }

    #[test]
    fn tool_edit_missing_file_path_returns_none() {
        assert!(tool_activity("Edit", &json("{}")).is_none());
    }
}
