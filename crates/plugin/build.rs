//! Linker flags for the wasm plugin. A build script rather than
//! `.cargo/config.toml`: cargo replaces `target.<triple>.rustflags` wholesale
//! whenever `RUSTFLAGS` is set in the environment, so a config-file flag
//! silently vanishes on any machine or CI job that exports one — and this
//! flag decides the plugin's memory profile.
//!
//! `-zstack-size`: the WASI shadow stack. wasm-ld's default is 1 MiB, which
//! alone put the plugin's initial linear memory at 19 pages (1.2 MiB) before
//! any heap, paid once per tab per client. The plugin's deepest call chains
//! are serde_json's decode (recursion capped at 128 by the library) and the
//! renderer, neither of which comes near 256 KiB; an overflow traps rather
//! than corrupting memory (the stack sits first in memory). `tools/wasm-fuel`
//! prints the resulting initial pages so the number cannot drift unnoticed.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo:rustc-link-arg-bins=-zstack-size=262144");
    }
}
