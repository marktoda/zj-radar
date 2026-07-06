//! Shared fixtures for the plugin's unit-test modules (the `lib.rs` tests,
//! `runtime/tests.rs`, and `radar_state/tests.rs`).
//!
//! These pin the canonical test vocabulary — position-derived tab ids, bare
//! panes, and the status-payload strings (`"repo"` / `"main"` / `"working"` /
//! `"claude"`) — in one place so the suites can't drift apart. Fixtures that
//! encode a *suite-specific* concern (State wiring in lib.rs, focus flags and
//! explicit tab ids in radar_state) stay as thin local wrappers over these,
//! so the difference reads as intentional at the definition site.

use crate::payload::StatusPayload;
use crate::radar_state::{RadarTab, TabId};
use crate::rollup::TerminalPane;
use crate::status::Status;

/// A tab whose `TabId` is decoupled from its position — for suites that
/// exercise id-vs-position semantics. Everyone else wants [`tab`].
pub(crate) fn tab_with_id(id: usize, position: usize, name: &str, active: bool) -> RadarTab {
    RadarTab {
        id: TabId::new(id),
        position,
        name: name.into(),
        active,
        has_bell: false,
    }
}

/// The common-case tab: `TabId` derived from its position (`position + 1`),
/// no bell.
pub(crate) fn tab(position: usize, name: &str, active: bool) -> RadarTab {
    tab_with_id(position + 1, position, name, active)
}

/// A bare terminal pane: only the id set — unfocused, no exit, no title.
pub(crate) fn pane(id: u32) -> TerminalPane {
    TerminalPane {
        id,
        ..TerminalPane::default()
    }
}

/// A status payload scoped to an explicit repo — for suites where the repo
/// drives the behavior under test (tab naming, cross-repo isolation).
pub(crate) fn payload_in_repo(pane_id: u32, status: Status, repo: &str) -> StatusPayload {
    StatusPayload {
        pane_id,
        status,
        repo: repo.into(),
        branch: "main".into(),
        msg: "working".into(),
        task: String::new(),
        source: "claude".into(),
    }
}

/// The canonical status payload: repo `"repo"`, branch `"main"`,
/// msg `"working"`, source `"claude"`.
pub(crate) fn payload_for(pane_id: u32, status: Status) -> StatusPayload {
    payload_in_repo(pane_id, status, "repo")
}
