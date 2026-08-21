//! Wall-clock source for the plugin runtime.
//!
//! A single free function rather than a trait/object: `RadarState` and the
//! stores beneath it take `now_epoch_s: u64` as a plain argument, so their
//! tests pass literal epochs with no clock to mock — and `PluginRuntime::timer`
//! follows the same discipline (its virtual-time harness, `runtime/tests.rs`'s
//! `FireSim`, passes its own advancing epoch). Only `PluginRuntime`
//! (`runtime.rs`) and the wasm glue (`lib.rs`) call this — at every entry
//! point that owns an epoch, plus render (age formatting).

/// Wall-clock seconds since the Unix epoch. Proven to work in wasm32-wasip1
/// (session_files.rs uses SystemTime). Free function so RadarState/store tests
/// can pass literal epochs instead.
pub(crate) fn now_epoch_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
