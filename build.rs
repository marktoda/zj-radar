use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Declare the cfg unconditionally so a `cli` build never trips the
    // unexpected-cfg lint, whether or not we end up embedding the wasm.
    println!("cargo:rustc-check-cfg=cfg(embedded_wasm)");

    // The wasm build itself must not recurse into this logic.
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return;
    }
    // Only the cli build embeds the wasm.
    if std::env::var("CARGO_FEATURE_CLI").is_err() {
        return;
    }
    println!("cargo:rerun-if-env-changed=ZJ_RADAR_WASM_PATH");

    // Embed the wasm when we can find or build one. When we can't (a
    // from-crates.io `cargo install` has no plugin crate to build and no
    // prebuilt artifact), the CLI ships WITHOUT an embedded wasm and `run`
    // downloads the matching release on first use — so this is best-effort, not
    // a hard failure.
    if let Some(path) = locate_wasm() {
        println!("cargo:rerun-if-changed={}", path.display());
        println!("cargo:rustc-env=ZJ_RADAR_WASM_PATH={}", path.display());
        println!("cargo:rustc-cfg=embedded_wasm");
    }
}

fn locate_wasm() -> Option<PathBuf> {
    // 1. Explicit override (release/nix supply a prebuilt wasm).
    if let Ok(p) = std::env::var("ZJ_RADAR_WASM_PATH") {
        let path = PathBuf::from(&p);
        if path.is_file() {
            return Some(path);
        }
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let prebuilt = manifest.join("target/wasm32-wasip1/release/zj_radar.wasm");
    // 2. Prebuilt artifact (fast path for `just test` / dev).
    if prebuilt.is_file() {
        return Some(prebuilt);
    }
    // 3. Build it from the sibling plugin crate — only in the workspace. A
    //    from-crates.io install has no plugin crate, so skip silently and let
    //    `run` download the wasm at first use.
    if !manifest.join("crates/plugin/Cargo.toml").is_file() {
        return None;
    }
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
        .status();
    match status {
        Ok(s) if s.success() && prebuilt.is_file() => Some(prebuilt),
        _ => {
            // In the workspace this usually means the wasm32-wasip1 target is
            // missing. Don't fail the build — embed nothing and let `run`
            // download — but surface the reason.
            println!(
                "cargo:warning=zj-radar: could not build the wasm to embed; \
                 `zj-radar run` will download it on first use \
                 (add the wasm32-wasip1 target to embed it instead)"
            );
            None
        }
    }
}
