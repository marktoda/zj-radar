//! Resolved per-pane observation vocabulary shared by status and command sources.

use crate::status::Status;

/// Which source produced an observation: the status pipe (agents) or a tracked
/// shell command. Carries its own snapshot wire vocabulary, like `Status` —
/// the persisted snapshot is the only place these tokens cross a boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObservationOrigin {
    StatusPipe,
    Command,
}

impl ObservationOrigin {
    /// The snapshot wire token for this origin.
    pub fn as_wire(self) -> &'static str {
        match self {
            ObservationOrigin::StatusPipe => "status_pipe",
            ObservationOrigin::Command => "command",
        }
    }

    /// Parse a snapshot wire token; an unknown token yields `None` so the
    /// caller drops the entry rather than guessing an origin.
    pub fn from_wire(raw: &str) -> Option<ObservationOrigin> {
        match raw {
            "status_pipe" => Some(ObservationOrigin::StatusPipe),
            "command" => Some(ObservationOrigin::Command),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedObservation {
    pub origin: ObservationOrigin,
    pub status: Status,
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub source: String,
    pub last_change_tick: u64,
    pub seq: Option<u64>,
    pub on_focus: Option<Status>,
    pub ever_active: bool,
    /// Exit code of a finished command pane, when known. Set by
    /// `CommandStore::on_exit` from a `zellij run`-style pane exit; `None` for
    /// agents (status pipe) and for commands that finish by returning to the
    /// shell prompt (no exit code is reported). Drives the `(exit N)` outcome
    /// tag on error rows.
    pub exit_code: Option<i32>,
}

impl TrackedObservation {
    /// Apply a pending `on_focus` transition (clear-on-focus): adopt the queued
    /// status and clear it. `last_change_tick` advances only when the status
    /// actually changes. Shared by `StatusStore` and `CommandStore`; the
    /// transition belongs to the observation, not the store.
    pub fn apply_on_focus(&mut self, tick: u64) {
        if let Some(next) = self.on_focus.take() {
            if self.status != next {
                self.last_change_tick = tick;
            }
            self.status = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_wire_round_trips_and_rejects_unknown() {
        for origin in [ObservationOrigin::StatusPipe, ObservationOrigin::Command] {
            assert_eq!(ObservationOrigin::from_wire(origin.as_wire()), Some(origin));
        }
        assert_eq!(ObservationOrigin::from_wire("nonsense"), None);
        assert_eq!(ObservationOrigin::from_wire(""), None);
    }
}
