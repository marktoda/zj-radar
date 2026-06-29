//! Resolved per-pane observation vocabulary shared by status and command sources.

use crate::status::Status;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObservationOrigin {
    StatusPipe,
    Command,
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
