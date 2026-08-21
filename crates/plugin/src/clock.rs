//! Wall-clock source for the plugin runtime.
//!
//! A single free function rather than a trait/object: `RadarState` and the
//! stores beneath it take `now_epoch_s: u64` as a plain argument, so their
//! tests pass literal epochs with no clock to mock. Only `PluginRuntime`
//! (`runtime.rs`) calls this — at every entry point that owns an epoch, plus
//! render (age formatting).

#[cfg(test)]
thread_local! {
    /// Test override for [`now_epoch_s`] — `None` (the default) = real wall
    /// clock. Per-thread, and each `#[test]` runs on its own thread, so an
    /// override never leaks across tests.
    static TEST_NOW_EPOCH_S: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// Test-only: pin [`now_epoch_s`] to a fixed epoch on this thread. Lets a
/// virtual-time harness (`runtime/tests.rs`'s `FireSim`) advance the clock
/// the wall-clock-keyed gates read — most notably the presence heartbeat's
/// level trigger (`PRESENCE_HEARTBEAT_S`) — in lockstep with its fires.
#[cfg(test)]
pub(crate) fn set_now_for_test(epoch_s: u64) {
    TEST_NOW_EPOCH_S.with(|c| c.set(Some(epoch_s)));
}

/// Wall-clock seconds since the Unix epoch. Proven to work in wasm32-wasip1
/// (session_files.rs uses SystemTime). Free function so RadarState/store tests
/// can pass literal epochs instead.
pub(crate) fn now_epoch_s() -> u64 {
    #[cfg(test)]
    if let Some(pinned) = TEST_NOW_EPOCH_S.with(|c| c.get()) {
        return pinned;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
