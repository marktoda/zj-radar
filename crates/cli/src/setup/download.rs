use super::*;

use std::path::{Path, PathBuf};

/// The release URL for the wasm artifact built from a given crate version.
/// `setup zellij --download` fetches the wasm matching the CLI's own version so
/// the two halves shipped from one tag can't drift across Zellij's unstable
/// plugin ABI (a CLI and a hand-downloaded wasm of different versions otherwise
/// can). Pure so the version→asset mapping is unit-tested; the fetch itself is
/// thin IO below.
fn wasm_release_url(version: &str) -> String {
    format!("https://github.com/{}/releases/download/v{version}/zj_radar.wasm", repo_slug())
}

/// The `owner/repo` slug release assets are fetched from. Defaults to the
/// repository baked in at build time (`CARGO_PKG_REPOSITORY`), so a fork only has
/// to set `repository` in Cargo.toml — no source edit and no drift between the
/// download URL and the error message. `ZJ_RADAR_REPO` overrides at runtime,
/// mirroring the curl|sh installer's same-named knob.
fn repo_slug() -> String {
    match std::env::var("ZJ_RADAR_REPO") {
        Ok(slug) if !slug.is_empty() => slug,
        _ => env!("CARGO_PKG_REPOSITORY")
            .trim_end_matches('/')
            .rsplit("github.com/")
            .next()
            .unwrap_or("marktoda/zj-radar")
            .to_string(),
    }
}

/// Fetch the wasm matching `version` to `dest` (creating its parent dir). Shells
/// out to curl (or wget) rather than linking a Rust TLS stack — keeping the host
/// build free of openssl/rustls, and curl is already assumed by the install flow.
/// Shared by `setup zellij --download` and `run`'s first-use fallback (when the
/// CLI shipped without an embedded wasm).
pub(crate) fn download_wasm_to(version: &str, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir failed — {e}"))?;
    }
    let url = wasm_release_url(version);
    println!("zj-radar: downloading wasm {version} from {url}");
    run_download(&url, dest)?;
    if !dest.is_file() {
        return Err(format!("download reported success but {} is missing", dest.display()));
    }
    Ok(())
}

/// Fetch the wasm matching `version` to a temp file and return its path.
pub(crate) fn download_wasm(version: &str) -> Result<PathBuf, String> {
    let dest = std::env::temp_dir().join(format!("zj_radar-{version}.wasm"));
    download_wasm_to(version, &dest)?;
    Ok(dest)
}

/// HTTPS-only download via curl, falling back to wget only when curl is absent
/// (so a curl HTTP error surfaces as itself rather than a confusing wget retry).
fn run_download(url: &str, dest: &Path) -> Result<(), String> {
    use std::process::Command;
    if which("curl") {
        let status = Command::new("curl")
            .args(["--proto", "=https", "--tlsv1.2", "-fL", url, "-o"])
            .arg(dest)
            .status()
            .map_err(|e| format!("failed to run curl — {e}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!(
                "curl failed for {url} — is v{} released? See https://github.com/{}/releases",
                env!("CARGO_PKG_VERSION"),
                repo_slug()
            ))
        };
    }
    if which("wget") {
        let status = Command::new("wget")
            .args(["--https-only", "-O"])
            .arg(dest)
            .arg(url)
            .status()
            .map_err(|e| format!("failed to run wget — {e}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!("wget failed for {url}"))
        };
    }
    Err("need curl or wget on PATH to --download".to_string())
}

/// The wasm release tag to fetch: `ZJ_RADAR_VERSION` (a leading `v` is optional)
/// overrides, else this CLI's own version — the version-skew-safe default.
pub(crate) fn wasm_download_version() -> String {
    std::env::var("ZJ_RADAR_VERSION")
        .ok()
        .map(|v| v.trim_start_matches('v').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_release_url_points_at_versioned_asset() {
        assert_eq!(
            wasm_release_url("0.1.0"),
            "https://github.com/marktoda/zj-radar/releases/download/v0.1.0/zj_radar.wasm"
        );
    }
}
