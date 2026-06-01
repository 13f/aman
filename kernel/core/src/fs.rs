// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Filesystem utilities: atomic writes and temp-file cleanup.
//!
//! Every caller that writes a complete file through a temp+sync+rename dance
//! should use [`atomic_write`] instead. Callers that produce temp files during
//! normal operation should call [`cleanup_temp_files`] on their output directory
//! to clear away stale leftovers from previous crashes.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Write `content` to `path` atomically.
///
/// Writes to a temporary file in the same directory, calls `fsync`, then
/// renames to the final path. If the process crashes mid-write, the temp
/// file is left behind and can be cleaned up later by [`cleanup_temp_files`].
///
/// The temp file is named `.tmp.{uuid}.tmp` in `path`'s parent directory.
pub fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp = parent.join(format!(".tmp.{}.tmp", uuid::Uuid::now_v7()));
    let mut f = fs::File::create(&temp)?;
    f.write_all(content)?;
    f.sync_all()?;
    fs::rename(&temp, path)?;
    Ok(())
}

/// Remove leftover `.tmp.*` files from `dir` that are older than
/// `max_age_secs`. These accumulate when a process crashes during an
/// [`atomic_write`].
///
/// Pass `max_age_secs: 0` to remove all temp files regardless of age.
pub fn cleanup_temp_files(dir: &Path, max_age_secs: u64) -> std::io::Result<u64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut removed = 0u64;
    if !dir.exists() {
        return Ok(0);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if !name_str.starts_with(".tmp.") {
            continue;
        }
        if max_age_secs > 0
            && let Ok(meta) = entry.metadata()
                && let Ok(mtime) = meta.modified()
                    && let Ok(age) = mtime.duration_since(UNIX_EPOCH)
                        && now.saturating_sub(age.as_secs()) < max_age_secs
        {
            continue; // too recent — might still be in-flight
        }
        let _ = fs::remove_file(entry.path());
        removed += 1;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("aman_fs_test_{}", uuid::Uuid::now_v7()))
    }

    #[test]
    fn atomic_write_creates_file() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("target.txt");
        atomic_write(&path, b"hello world").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn atomic_write_overwrites() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("target.txt");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cleanup_temp_files_removes_stale() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        // Create a stale temp file
        fs::write(dir.join(".tmp.stale.tmp"), b"stale").unwrap();
        // Create a real file that should NOT be removed
        fs::write(dir.join("real.txt"), b"real").unwrap();
        let count = cleanup_temp_files(&dir, 0).unwrap();
        assert_eq!(count, 1);
        assert!(dir.join("real.txt").exists());
        assert!(!dir.join(".tmp.stale.tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cleanup_temp_files_respects_age() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        // Create a temp file that was just written
        fs::write(dir.join(".tmp.fresh.tmp"), b"fresh").unwrap();
        // With a very large max_age_secs, it should be kept
        let count = cleanup_temp_files(&dir, 3600).unwrap();
        assert_eq!(count, 0);
        assert!(dir.join(".tmp.fresh.tmp").exists());
        // With max_age_secs=0, it should be removed
        let count = cleanup_temp_files(&dir, 0).unwrap();
        assert_eq!(count, 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
