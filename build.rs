use std::path::PathBuf;
use std::process::Command;

fn main() {
    // The wasm build itself must not recurse into this logic.
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return;
    }
    // Only the cli build embeds the wasm.
    if std::env::var("CARGO_FEATURE_CLI").is_err() {
        return;
    }
    println!("cargo:rerun-if-env-changed=ZJ_RADAR_WASM_PATH");

    // 1. Explicit override (nix/just provide a prebuilt wasm).
    if let Ok(p) = std::env::var("ZJ_RADAR_WASM_PATH") {
        if PathBuf::from(&p).is_file() {
            println!("cargo:rerun-if-changed={p}");
            println!("cargo:rustc-env=ZJ_RADAR_WASM_PATH={p}");
            return;
        }
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let prebuilt = manifest.join("target/wasm32-wasip1/release/zj_radar.wasm");
    // 2. Prebuilt artifact (fast path for `just test` / dev).
    if prebuilt.is_file() {
        println!("cargo:rerun-if-changed={}", prebuilt.display());
        println!("cargo:rustc-env=ZJ_RADAR_WASM_PATH={}", prebuilt.display());
        return;
    }
    // 3. Build it from the sibling plugin crate (self-contained workspace
    //    builds). Requires the wasm target AND the workspace (so this path is
    //    NOT taken on a from-crates.io install — release builds instead supply
    //    ZJ_RADAR_WASM_PATH via step 1, the prebuilt fast path).
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip1",
            "-p",
            "zj-radar-plugin",
        ])
        .current_dir(&manifest)
        .status()
        .expect("failed to spawn cargo for wasm build");
    assert!(status.success(), "wasm build failed; install the wasm32-wasip1 target");
    println!("cargo:rerun-if-changed={}", prebuilt.display());
    println!("cargo:rustc-env=ZJ_RADAR_WASM_PATH={}", prebuilt.display());
}
