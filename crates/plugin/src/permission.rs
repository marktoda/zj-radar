//! Deep module: the first-run permission handshake as one state machine.
//!
//! Replaces four loose booleans on `PluginRuntime` with a single
//! [`PermissionState`]. The module is host-agnostic — it imports neither the
//! runtime's `Effect` vocabulary nor `Config`. Transitions take a
//! [`PermissionProbe`] (what `session_files` observed on disk) plus a
//! [`PermissionPolicy`] (the caller's role/defer choice, pre-collapsed) and
//! return a [`Transition`] the runtime maps to effects. The runtime keeps
//! ownership of timer-arming, `SetSelectable`, and `CloseSelf`.

/// The persisted answer a peer left in the session's permission marker file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionMarker {
    Granted,
    Denied,
}

/// What `session_files` observed: a landed marker (if any) and whether this
/// instance holds the first-run lock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PermissionProbe {
    pub marker: Option<PermissionMarker>,
    pub lock_acquired: bool,
}

/// The caller's permission stance, collapsed from `(role, defer_permission)` so
/// the three mutually-exclusive policies are explicit and this module never
/// imports `Config`. The runtime computes it once via `PermissionPolicy::of`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionPolicy {
    /// The onboarding floating pane: always request, regardless of the lock —
    /// it is the dedicated legible host for Zellij's grant prompt.
    OnboardingPane,
    /// A deferring rail: act ONLY on a landed marker; never self-own via the
    /// lock (that would steal the prompt binding from the onboarding pane).
    Deferring,
    /// The default: lock-coordinated. A held/reclaimed lock self-elects.
    LockCoordinated,
}

/// What a probe dictates under a policy. Private impl detail — [`Transition`]
/// is the module's public output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PermissionDecision {
    /// Become the prompt-shower: request permission from Zellij.
    Request,
    /// Permission was denied (a denied marker is already on disk).
    Deny,
}

/// The first-run permission state. `Unprompted` is the pre-load default: not
/// granted, not selectable, and the deferred-timer check is inert (only
/// `WaitingForPeer` re-probes). Illegal combinations the four booleans allowed
/// are now unrepresentable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PermissionState {
    #[default]
    Unprompted,
    /// No decision yet — re-probe on every timer tick.
    WaitingForPeer,
    /// Our request is in-flight; the pane is selectable and a paint heartbeat
    /// keeps the needs-permission screen alive until the user answers.
    Requesting,
    /// Terminal: the user (or a marker) answered.
    Resolved { granted: bool },
}

/// The observable result of a transition, in host-agnostic terms. The runtime
/// maps this to effects (`Requested` → `RequestPermission`, etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transition {
    /// We just decided to request permission (entered `Requesting`).
    Requested,
    /// We reached a terminal answer without a user prompt (a marker decided it).
    Resolved { granted: bool },
    /// No decision yet; we are (still) waiting on a peer.
    StillWaiting,
    /// Nothing changed (a timer tick with no decision, or from a settled state).
    NoChange,
}

impl PermissionState {
    /// The load-time transition. Decides per `policy`/`probe` and moves into the
    /// resulting state. (Production calls this once from `Unprompted`.)
    pub(crate) fn on_load(&mut self, probe: &PermissionProbe, policy: PermissionPolicy) -> Transition {
        match decide(probe, policy) {
            Some(decision) => self.enter(decision),
            // No decision yet — this is a fresh entry into the waiting state.
            None => {
                *self = PermissionState::WaitingForPeer;
                Transition::StillWaiting
            }
        }
    }

    /// A deferred timer tick. Acts only while `WaitingForPeer`; any other state
    /// is settled and yields `NoChange`.
    pub(crate) fn on_timer(&mut self, probe: &PermissionProbe, policy: PermissionPolicy) -> Transition {
        if !self.is_waiting() {
            return Transition::NoChange;
        }
        match decide(probe, policy) {
            Some(decision) => self.enter(decision),
            // Already waiting and still no decision: nothing changed.
            None => Transition::NoChange,
        }
    }

    /// The user answered Zellij's prompt. Terminal.
    pub(crate) fn on_result(&mut self, granted: bool) {
        *self = PermissionState::Resolved { granted };
    }

    /// True once permission is granted (gates clicks, pipes, and the live rail).
    pub(crate) fn granted(&self) -> bool {
        matches!(self, PermissionState::Resolved { granted: true })
    }

    /// True only while a request is in-flight: the pane must be selectable so
    /// the user can reach Zellij's y/n prompt.
    pub(crate) fn selectable(&self) -> bool {
        matches!(self, PermissionState::Requesting)
    }

    /// True only while waiting on a peer's marker (drives the timer heartbeat).
    pub(crate) fn is_waiting(&self) -> bool {
        matches!(self, PermissionState::WaitingForPeer)
    }

    /// Move into the state a (definite) decision dictates, reporting the
    /// transition. The `None`-decision case differs by entry point — a fresh
    /// `StillWaiting` at load vs `NoChange` on a tick — so it stays with the
    /// callers; only the decisive arms are shared here.
    fn enter(&mut self, decision: PermissionDecision) -> Transition {
        match decision {
            PermissionDecision::Request => {
                *self = PermissionState::Requesting;
                Transition::Requested
            }
            PermissionDecision::Deny => {
                *self = PermissionState::Resolved { granted: false };
                Transition::Resolved { granted: false }
            }
        }
    }
}

/// The single probe→decision mapping, dispatched by policy. `None` means "no
/// decision yet — keep waiting." Both entry points ride this, so the load and
/// deferred-timer paths can never disagree.
fn decide(probe: &PermissionProbe, policy: PermissionPolicy) -> Option<PermissionDecision> {
    match policy {
        // The onboarding float always owns the prompt — it's the only legible
        // surface — regardless of the lock.
        PermissionPolicy::OnboardingPane => Some(PermissionDecision::Request),
        // A deferring rail acts ONLY on a landed marker; the lock never elects
        // it (that would steal Zellij's prompt binding from the float).
        PermissionPolicy::Deferring => match probe.marker {
            Some(PermissionMarker::Granted) => Some(PermissionDecision::Request),
            Some(PermissionMarker::Denied) => Some(PermissionDecision::Deny),
            None => None,
        },
        // Default: a marker decides; otherwise a held/reclaimed lock self-elects.
        PermissionPolicy::LockCoordinated => match probe.marker {
            Some(PermissionMarker::Granted) => Some(PermissionDecision::Request),
            Some(PermissionMarker::Denied) => Some(PermissionDecision::Deny),
            None if probe.lock_acquired => Some(PermissionDecision::Request),
            None => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::PermissionMarker::{Denied, Granted};
    use super::PermissionPolicy::{Deferring, LockCoordinated, OnboardingPane};
    use super::*;

    fn probe(marker: Option<PermissionMarker>, lock_acquired: bool) -> PermissionProbe {
        PermissionProbe { marker, lock_acquired }
    }

    /// Every (state-entry-point × policy × probe) maps to the documented next
    /// state and transition. This is the interface-as-test-surface for the
    /// machine; the runtime tests only assert on the effects it derives.
    #[test]
    fn on_load_truth_table() {
        // (policy, marker, lock) -> (next state, transition)
        let cases = [
            // OnboardingPane: always request, ignores the probe entirely.
            (OnboardingPane, None, false, PermissionState::Requesting, Transition::Requested),
            (OnboardingPane, None, true, PermissionState::Requesting, Transition::Requested),
            (OnboardingPane, Some(Denied), false, PermissionState::Requesting, Transition::Requested),
            // Deferring: marker-only; the lock never self-elects.
            (Deferring, None, true, PermissionState::WaitingForPeer, Transition::StillWaiting),
            (Deferring, None, false, PermissionState::WaitingForPeer, Transition::StillWaiting),
            (Deferring, Some(Granted), false, PermissionState::Requesting, Transition::Requested),
            (Deferring, Some(Denied), false, PermissionState::Resolved { granted: false }, Transition::Resolved { granted: false }),
            // LockCoordinated: marker, else a held lock self-elects.
            (LockCoordinated, Some(Granted), false, PermissionState::Requesting, Transition::Requested),
            (LockCoordinated, Some(Denied), true, PermissionState::Resolved { granted: false }, Transition::Resolved { granted: false }),
            (LockCoordinated, None, true, PermissionState::Requesting, Transition::Requested),
            (LockCoordinated, None, false, PermissionState::WaitingForPeer, Transition::StillWaiting),
        ];
        for (policy, marker, lock, want_state, want_tr) in cases {
            let mut st = PermissionState::default();
            let tr = st.on_load(&probe(marker, lock), policy);
            assert_eq!(tr, want_tr, "transition for {policy:?} marker={marker:?} lock={lock}");
            assert_eq!(st, want_state, "state for {policy:?} marker={marker:?} lock={lock}");
        }
    }

    #[test]
    fn on_timer_acts_only_while_waiting() {
        // Settled states never move on a tick, even a decisive probe.
        for settled in [
            PermissionState::Unprompted,
            PermissionState::Requesting,
            PermissionState::Resolved { granted: true },
            PermissionState::Resolved { granted: false },
        ] {
            let mut st = settled;
            let tr = st.on_timer(&probe(Some(Granted), true), LockCoordinated);
            assert_eq!(tr, Transition::NoChange, "settled {settled:?} must not move");
            assert_eq!(st, settled, "settled {settled:?} state unchanged");
        }
    }

    #[test]
    fn waiting_peer_promotes_on_granted_marker() {
        let mut st = PermissionState::WaitingForPeer;
        let tr = st.on_timer(&probe(Some(Granted), false), LockCoordinated);
        assert_eq!(tr, Transition::Requested);
        assert_eq!(st, PermissionState::Requesting);
    }

    #[test]
    fn waiting_peer_self_promotes_on_reclaimed_lock() {
        let mut st = PermissionState::WaitingForPeer;
        let tr = st.on_timer(&probe(None, true), LockCoordinated);
        assert_eq!(tr, Transition::Requested);
        assert_eq!(st, PermissionState::Requesting);
    }

    #[test]
    fn waiting_peer_stays_put_without_a_decision() {
        let mut st = PermissionState::WaitingForPeer;
        let tr = st.on_timer(&probe(None, false), LockCoordinated);
        assert_eq!(tr, Transition::NoChange);
        assert_eq!(st, PermissionState::WaitingForPeer);
    }

    #[test]
    fn deferring_waiter_ignores_the_lock_but_takes_a_marker() {
        let mut st = PermissionState::WaitingForPeer;
        // A reclaimed lock must NOT promote a deferring rail.
        assert_eq!(st.on_timer(&probe(None, true), Deferring), Transition::NoChange);
        assert_eq!(st, PermissionState::WaitingForPeer);
        // Only a landed marker unblocks it.
        assert_eq!(st.on_timer(&probe(Some(Granted), false), Deferring), Transition::Requested);
        assert_eq!(st, PermissionState::Requesting);
    }

    #[test]
    fn on_result_is_terminal_and_drives_queries() {
        let mut st = PermissionState::Requesting;
        st.on_result(true);
        assert_eq!(st, PermissionState::Resolved { granted: true });
        assert!(st.granted());
        assert!(!st.selectable());
        assert!(!st.is_waiting());

        let mut st = PermissionState::Requesting;
        st.on_result(false);
        assert_eq!(st, PermissionState::Resolved { granted: false });
        assert!(!st.granted());
    }

    #[test]
    fn queries_partition_the_states() {
        assert!(PermissionState::Requesting.selectable());
        assert!(!PermissionState::WaitingForPeer.selectable());
        assert!(PermissionState::WaitingForPeer.is_waiting());
        assert!(!PermissionState::Requesting.is_waiting());
        assert!(!PermissionState::Unprompted.granted());
        assert!(!PermissionState::Unprompted.selectable());
        assert!(!PermissionState::Unprompted.is_waiting());
    }
}
