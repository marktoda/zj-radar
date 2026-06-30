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
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// `<path>.zj-radar.tmp` — a sibling so the final `rename` stays on one
/// filesystem. Appending to the full path (rather than replacing the extension)
/// avoids collisions between files that differ only by extension.
fn tmp_sibling(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".zj-radar.tmp");
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
        // No temp file left behind.
        assert!(!target.with_file_name("c.txt.zj-radar.tmp").exists());
    }

    #[test]
    fn overwrites_existing() {
        let d = tempdir().unwrap();
        let target = d.path().join("c.txt");
        atomic_write(&target, b"first").unwrap();
        atomic_write(&target, b"second").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"second");
    }
}
