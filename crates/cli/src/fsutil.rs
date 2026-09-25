//! Crash-safe filesystem writes shared across the `cli` module.
//!
//! `setup` (managing the user's `config.toml`/`config.kdl`) and `run` (writing
//! its owned config dir) both need the same guarantee: a reader sees either the
//! old file or the fully-written new one, never a half-written file. Centralizing
//! the temp-file + rename here keeps that one implementation, not two.

use std::io;
use std::path::{Path, PathBuf};

/// Write `contents` to `path` atomically: ensure the parent directory exists,
/// write a sibling temp file, then rename it over `path`. The rename is atomic
/// on the same filesystem, so an interrupted or failed write never leaves a
/// partially-written file in place — the next attempt simply rewrites.
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = tmp_sibling(path);
    write_new(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Write `contents` to a file that must not exist yet (`O_CREAT|O_EXCL`), so
/// a symlink planted at the temp name is never followed into someone else's
/// file. A leftover at that name (a crashed writer that had our pid) is
/// unlinked — the link itself, not its target — and the create retried once.
fn write_new(tmp: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let open = || std::fs::OpenOptions::new().write(true).create_new(true).open(tmp);
    let mut file = match open() {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(tmp)?;
            open()?
        }
        other => other?,
    };
    file.write_all(contents)
}

/// `<path>.zj-radar.<pid>.tmp` — a sibling so the final `rename` stays on one
/// filesystem. Appending to the full path (rather than replacing the extension)
/// avoids collisions between files that differ only by extension; the pid keeps
/// two concurrent writers of the SAME path from sharing a temp file — with a
/// fixed name, one process could rename the other's half-written temp into place.
fn tmp_sibling(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(format!(".zj-radar.{}.tmp", std::process::id()));
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn creates_parents_and_writes() {
        let d = tempdir().unwrap();
        let target = d.path().join("a/b/c.txt");
        atomic_write(&target, b"hello").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        // No temp file left behind — the target is its directory's only entry.
        let entries: Vec<_> =
            std::fs::read_dir(target.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("c.txt")]);
    }

    #[test]
    fn overwrites_existing() {
        let d = tempdir().unwrap();
        let target = d.path().join("c.txt");
        atomic_write(&target, b"first").unwrap();
        atomic_write(&target, b"second").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"second");
    }

    #[test]
    fn a_symlink_at_the_temp_name_is_replaced_not_followed() {
        let d = tempdir().unwrap();
        let victim = d.path().join("victim");
        std::fs::write(&victim, b"untouched").unwrap();
        let target = d.path().join("c.txt");
        std::os::unix::fs::symlink(&victim, tmp_sibling(&target)).unwrap();
        atomic_write(&target, b"hello").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        assert_eq!(std::fs::read(&victim).unwrap(), b"untouched", "the link's target was never written");
        assert!(!std::fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
    }
}
