//! Pre-seed the sidebar's permission grant into Zellij's `permissions.kdl`.
//!
//! Zellij reads `permissions.kdl` fresh on every plugin load (each
//! `request_permission` call checks the on-disk cache first), so an entry
//! written here auto-resolves the sidebar's first-run prompt — the user never
//! sees Zellij's native y/n overlay, which is illegible at rail width
//! (zellij#4749). Grants are keyed by the plugin's absolute path with no
//! `file:` prefix, and only apply when the cached entry covers **every**
//! permission the plugin requests — so the merge always writes the full
//! [`crate::run::REQUIRED_PLUGIN_PERMISSIONS`] set.
//!
//! Zellij owns this file (it read-merge-rewrites it on every y/n answer), so
//! the merge is deliberately conservative: other plugins' entries are
//! preserved byte-for-byte, and unparseable input is refused outright — a
//! malformed file would be silently reset to a single entry by Zellij's next
//! grant, so writing into one risks amplifying damage we didn't cause.
//! One known erasure vector remains Zellij's: answering `n` to any later
//! prompt for this plugin overwrites the entry with an empty grant.

use crate::run::REQUIRED_PLUGIN_PERMISSIONS;

/// Outcome of merging our grant into `permissions.kdl` text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Preseed {
    /// Some existing block already covers the full permission set — no write.
    AlreadyGranted,
    /// The new file contents to write.
    Merged(String),
}

/// Merge a full grant for `wasm_abs_path` into `existing` `permissions.kdl`
/// text (`None` = file absent). Fails closed: input that does not parse as
/// KDL, or a merge whose output would not, is refused rather than written.
pub(crate) fn merge_grant(existing: Option<&str>, wasm_abs_path: &str) -> Result<Preseed, String> {
    if wasm_abs_path.contains(['"', '\\', '\n']) {
        // Quotable-verbatim paths only: the emitted key must byte-match what
        // Zellij's own lookup (and our `wasm_is_granted`) compare against.
        return Err(format!("refusing to pre-seed an unquotable wasm path: {wasm_abs_path}"));
    }
    let text = existing.unwrap_or("");
    if !text.trim().is_empty() {
        text.parse::<kdl::KdlDocument>().map_err(|e| {
            format!("existing permissions.kdl failed to parse — refusing to edit it ({e})")
        })?;
    }
    if crate::run::wasm_is_granted(text, wasm_abs_path) {
        return Ok(Preseed::AlreadyGranted);
    }
    let merged = extend_last_block(text, wasm_abs_path)
        .unwrap_or_else(|| append_block(text, wasm_abs_path));
    merged.parse::<kdl::KdlDocument>().map_err(|e| {
        format!("merged permissions.kdl would not parse — refusing to write it ({e})")
    })?;
    Ok(Preseed::Merged(merged))
}

/// Widen the LAST existing block for this path to the full permission set,
/// or `None` when no block exists. Last, not first: Zellij loads the file
/// into a map keyed by path, so with duplicate blocks the later one wins —
/// widening an earlier one would change nothing.
fn extend_last_block(text: &str, wasm_abs_path: &str) -> Option<String> {
    let needle = format!("\"{wasm_abs_path}\"");
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let open = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with(&needle) && l.contains('{'))?;
    let close = (open + 1..lines.len()).find(|&j| lines[j].trim_start().starts_with('}'))?;
    let granted: Vec<&str> = lines[open + 1..close].iter().map(|l| l.trim()).collect();

    let mut out = String::with_capacity(text.len() + 128);
    for (j, line) in lines.iter().enumerate() {
        if j == close {
            for perm in REQUIRED_PLUGIN_PERMISSIONS.iter().filter(|p| !granted.contains(p)) {
                out.push_str("    ");
                out.push_str(perm);
                out.push('\n');
            }
        }
        out.push_str(line);
    }
    Some(out)
}

/// Append a fresh full-grant block, preserving the existing text byte-for-byte.
fn append_block(text: &str, wasm_abs_path: &str) -> String {
    let mut out = String::with_capacity(text.len() + 160);
    out.push_str(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\"{wasm_abs_path}\" {{\n"));
    for perm in REQUIRED_PLUGIN_PERMISSIONS {
        out.push_str("    ");
        out.push_str(perm);
        out.push('\n');
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const WASM: &str = "/home/u/.config/zellij/plugins/zj_radar.wasm";

    fn merged(existing: Option<&str>) -> String {
        match merge_grant(existing, WASM) {
            Ok(Preseed::Merged(text)) => text,
            other => panic!("expected Merged, got {other:?}"),
        }
    }

    fn block_grants_all(text: &str, path: &str) -> bool {
        crate::run::wasm_is_granted(text, path)
    }

    #[test]
    fn absent_file_produces_a_full_grant_block() {
        let out = merged(None);
        assert!(block_grants_all(&out, WASM), "output must grant the full set:\n{out}");
        assert!(out.parse::<kdl::KdlDocument>().is_ok(), "output must be valid KDL:\n{out}");
        assert!(!out.contains("file:"), "grant keys carry no file: prefix");
    }

    #[test]
    fn foreign_entries_survive_byte_for_byte() {
        let existing = "\"/nix/store/abc-room.wasm\" {\n    ReadApplicationState\n    ChangeApplicationState\n}\n";
        let out = merged(Some(existing));
        assert!(out.starts_with(existing), "foreign entry must be preserved untouched:\n{out}");
        assert!(block_grants_all(&out, WASM), "our grant must be appended:\n{out}");
        assert!(out.parse::<kdl::KdlDocument>().is_ok());
    }

    #[test]
    fn partial_block_is_extended_to_the_full_set() {
        // An older plugin version's narrower grant: Zellij would re-prompt
        // (illegibly) because the cached entry must be a superset of the
        // request. The merge widens the existing block instead of appending a
        // duplicate.
        let existing = format!("\"{WASM}\" {{\n    ReadApplicationState\n    ReadCliPipes\n}}\n");
        let out = merged(Some(&existing));
        assert!(block_grants_all(&out, WASM), "block must now cover the full set:\n{out}");
        assert_eq!(
            out.matches(&format!("\"{WASM}\"")).count(),
            1,
            "extend the block, don't append a duplicate:\n{out}"
        );
        assert!(out.parse::<kdl::KdlDocument>().is_ok());
    }

    #[test]
    fn full_block_is_already_granted() {
        let existing = format!(
            "\"{WASM}\" {{\n    RunCommands\n    ChangeApplicationState\n    ReadCliPipes\n    ReadApplicationState\n}}\n"
        );
        assert_eq!(merge_grant(Some(&existing), WASM), Ok(Preseed::AlreadyGranted));
    }

    #[test]
    fn later_full_duplicate_counts_as_granted() {
        // Mirror `wasm_is_granted`: after a Zellij re-prompt a stale partial
        // block can precede a later full one — the pair is already granted.
        let existing = format!(
            "\"{WASM}\" {{\n    ReadApplicationState\n}}\n\
             \"{WASM}\" {{\n    ReadApplicationState\n    ReadCliPipes\n    ChangeApplicationState\n    RunCommands\n}}\n"
        );
        assert_eq!(merge_grant(Some(&existing), WASM), Ok(Preseed::AlreadyGranted));
    }

    #[test]
    fn merge_is_idempotent() {
        let once = merged(None);
        assert_eq!(merge_grant(Some(&once), WASM), Ok(Preseed::AlreadyGranted));
    }

    #[test]
    fn unparseable_input_is_refused() {
        // Zellij treats a malformed permissions.kdl as empty and rewrites it
        // wholesale on the next grant; we must not write into one.
        let err = merge_grant(Some("\"/a.wasm\" {\n    ReadApplicationState\n"), WASM).unwrap_err();
        assert!(err.contains("parse"), "refusal should name the parse failure: {err}");
    }

    #[test]
    fn paths_with_spaces_round_trip() {
        let spaced = "/Users/m/Library/Application Support/zj-radar/zellij/plugins/zj_radar.wasm";
        let out = match merge_grant(None, spaced) {
            Ok(Preseed::Merged(text)) => text,
            other => panic!("expected Merged, got {other:?}"),
        };
        assert!(block_grants_all(&out, spaced), "spaced path must be quoted intact:\n{out}");
        assert!(out.parse::<kdl::KdlDocument>().is_ok());
    }

    #[test]
    fn empty_file_behaves_like_absent() {
        let out = merged(Some(""));
        assert!(block_grants_all(&out, WASM));
        assert!(out.parse::<kdl::KdlDocument>().is_ok());
    }
}
