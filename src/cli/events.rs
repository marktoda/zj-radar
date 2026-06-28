//! Shared status decisions for agent hook adapters.

use crate::status::Status;

/// The decision a pure derivation produces. `None` from a derivation means no-op.
#[derive(Debug, PartialEq, Eq)]
pub struct Update {
    pub status: Status,
    pub msg: String,
}

/// Short present-tense activity string for a tool invocation, or None if the
/// tool has no useful activity phrasing.
pub fn tool_activity(tool_name: &str, tool_input: &serde_json::Value) -> Option<String> {
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

fn bash_activity(tool_input: &serde_json::Value) -> Option<String> {
    let cmd = tool_input.get("command")?.as_str()?;
    let cmd_lower = cmd.to_lowercase();
    if cmd.trim().is_empty() {
        return None;
    }
    if cmd_lower.contains("git push") {
        Some("pushing".to_string())
    } else if cmd_lower.contains("git commit") {
        Some("committing".to_string())
    } else if cmd_lower.contains("git pull") || cmd_lower.contains("git fetch") {
        Some("syncing".to_string())
    } else if cmd_lower.contains("test") {
        Some("running tests".to_string())
    } else if cmd_lower.contains("build") || cmd_lower.contains("compile") {
        Some("building".to_string())
    } else if cmd_lower.contains("install") {
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

    fn json(s: &str) -> serde_json::Value {
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
