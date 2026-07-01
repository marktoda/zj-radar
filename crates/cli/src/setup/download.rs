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

/// The `.sha256` sidecar published next to the wasm asset (same release, same
/// version). Generated and uploaded by `.github/workflows/release.yml`.
fn wasm_checksum_url(version: &str) -> String {
    format!("{}.sha256", wasm_release_url(version))
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
    // Download to a `.part` sibling and rename into place only after the
    // transfer AND checksum both succeed. curl/wget write `dest` incrementally,
    // so an interrupted transfer straight to `dest` would leave a partial file
    // that the `exists()`/up-to-date gates treat as a valid wasm forever after —
    // and Zellij would then load it with permissions.
    let part = match dest.file_name().and_then(|n| n.to_str()) {
        Some(name) => dest.with_file_name(format!("{name}.part")),
        None => return Err(format!("invalid download destination {}", dest.display())),
    };
    let fetched = run_download(&url, &part)
        .and_then(|()| {
            if !part.is_file() {
                return Err(format!(
                    "download reported success but {} is missing",
                    part.display()
                ));
            }
            verify_checksum(version, &part)
        })
        .and_then(|()| {
            std::fs::rename(&part, dest)
                .map_err(|e| format!("moving the download into place failed — {e}"))
        });
    if fetched.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    fetched
}

/// Verify the freshly-downloaded wasm against its published `.sha256` sidecar.
///
/// Strict when the sidecar is fetchable: a mismatch is a hard error and the bad
/// wasm is removed — Zellij runs this wasm with permissions, so a payload that
/// doesn't match its published digest must never be installed. When the sidecar
/// is absent (a release predating checksums) or no local sha256 tool is on PATH,
/// this warns and continues rather than blocking install — TLS + GitHub release
/// storage remain the floor, and every new release publishes the sidecar (see
/// `.github/workflows/release.yml`), so absence is the exception, not the norm.
fn verify_checksum(version: &str, wasm: &Path) -> Result<(), String> {
    let sidecar = std::env::temp_dir().join(format!("zj_radar-{version}.wasm.sha256"));
    let _ = std::fs::remove_file(&sidecar); // clear any stale sidecar first
    if !try_download(&wasm_checksum_url(version), &sidecar) {
        eprintln!(
            "zj-radar: warning — no checksum published for v{version}; \
             installed wasm is TLS-verified only (integrity not checked)"
        );
        return Ok(());
    }
    let expected = std::fs::read_to_string(&sidecar).ok().and_then(|s| parse_sha256(&s));
    let _ = std::fs::remove_file(&sidecar);
    let Some(expected) = expected else {
        eprintln!("zj-radar: warning — checksum for v{version} was unreadable; skipping integrity check");
        return Ok(());
    };
    let Some(actual) = compute_sha256(wasm) else {
        eprintln!(
            "zj-radar: warning — no sha256 tool (sha256sum/shasum) on PATH; \
             skipping integrity check for v{version}"
        );
        return Ok(());
    };
    if actual.eq_ignore_ascii_case(&expected) {
        println!("zj-radar: wasm checksum verified");
        Ok(())
    } else {
        let _ = std::fs::remove_file(wasm); // don't leave a mismatched wasm staged
        Err(format!(
            "checksum mismatch for zj_radar.wasm v{version}\n  expected {expected}\n  got      {actual}\n\
             Refusing to install a wasm that does not match its published checksum."
        ))
    }
}

/// Extract a 64-char lowercase hex digest from a `sha256sum`/`shasum` line (`<hex>
/// <name>`) or a bare digest: the first whitespace-delimited token, validated as
/// exactly 64 hex chars. `None` for anything malformed.
fn parse_sha256(raw: &str) -> Option<String> {
    let token = raw.split_whitespace().next()?;
    let is_hex = token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit());
    is_hex.then(|| token.to_ascii_lowercase())
}

/// Compute a file's SHA-256 as lowercase hex via the system tool — `sha256sum`
/// (Linux/coreutils), else `shasum -a 256` (macOS). `None` when neither is on
/// PATH. Shelling out keeps the host build free of a crypto dependency, the same
/// reason the download itself uses curl/wget rather than a Rust TLS stack.
fn compute_sha256(path: &Path) -> Option<String> {
    use std::process::Command;
    let out = if which("sha256sum") {
        Command::new("sha256sum").arg(path).output().ok()?
    } else if which("shasum") {
        Command::new("shasum").args(["-a", "256"]).arg(path).output().ok()?
    } else {
        return None;
    };
    if !out.status.success() {
        return None;
    }
    parse_sha256(&String::from_utf8(out.stdout).ok()?)
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
            // `--proto-redir =https` too: `--proto` alone doesn't constrain
            // redirect targets, and `-L` follows them.
            .args(["--proto", "=https", "--proto-redir", "=https", "--tlsv1.2", "-fL", url, "-o"])
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

/// Best-effort HTTPS fetch for *optional* assets (the checksum sidecar): returns
/// whether it landed, swallowing the reason. A 404 for a release without the asset
/// is expected, not an error, so — unlike `run_download` — this reports absence as
/// a plain `false` rather than a user-facing message.
fn try_download(url: &str, dest: &Path) -> bool {
    use std::process::Command;
    if which("curl") {
        return Command::new("curl")
            .args(["--proto", "=https", "--proto-redir", "=https", "--tlsv1.2", "-fsSL", url, "-o"])
            .arg(dest)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }
    if which("wget") {
        return Command::new("wget")
            .args(["--https-only", "-qO"])
            .arg(dest)
            .arg(url)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }
    false
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

    #[test]
    fn wasm_checksum_url_is_the_wasm_url_plus_sha256() {
        assert_eq!(
            wasm_checksum_url("0.1.0"),
            "https://github.com/marktoda/zj-radar/releases/download/v0.1.0/zj_radar.wasm.sha256"
        );
    }

    #[test]
    fn parse_sha256_takes_the_digest_token_and_lowercases() {
        let digest = "a".repeat(64);
        // `sha256sum` line format: "<hex>  <name>".
        assert_eq!(parse_sha256(&format!("{digest}  zj_radar.wasm")), Some(digest.clone()));
        // A bare digest with surrounding whitespace/newline.
        assert_eq!(parse_sha256(&format!("  {digest}\n")), Some(digest.clone()));
        // Uppercase hex is normalized to lowercase for comparison.
        assert_eq!(parse_sha256(&"A".repeat(64)), Some("a".repeat(64)));
    }

    #[test]
    fn parse_sha256_rejects_malformed() {
        assert_eq!(parse_sha256(""), None);
        assert_eq!(parse_sha256("   "), None);
        assert_eq!(parse_sha256("not-a-hash  file"), None); // non-hex chars
        assert_eq!(parse_sha256(&"a".repeat(63)), None); // too short
        assert_eq!(parse_sha256(&"a".repeat(65)), None); // too long
        assert_eq!(parse_sha256(&format!("{}g", "a".repeat(63))), None); // 64 chars, one non-hex
    }
}
