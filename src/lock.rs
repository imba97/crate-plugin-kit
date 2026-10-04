//! Exclusive file lock held during install / uninstall.
//!
//! Two terminals running `plugin add` at once would corrupt `.plugins.json`, or let
//! two `cargo build` runs write into the same wrapper directory. A cross-process
//! lock keeps them out of each other's way.
//!
//! # Implementation
//!
//! The lock file is created with `create_new(true)` — atomic on mainstream file
//! systems, so whoever creates it first holds the lock. The pid and a timestamp are
//! written into it to make diagnosis easier.
//!
//! No OS-level file lock (`flock` / `LockFileEx`): that would need extra
//! per-platform code, and this crate already has a simpler criterion — does the lock
//! file exist, and is it stale? The price is that a `kill -9` leaves a stale lock
//! file behind, hence the stale takeover.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::error::{KitError, KitResult};

/// How old a lock file has to be before it counts as stale. A lock older than this
/// is taken over by a later arrival.
///
/// The value has to be longer than any reasonable install takes (`cargo build` can
/// take minutes), and shorter than it takes the user to forget about it.
pub const STALE_AFTER: Duration = Duration::from_secs(30 * 60);

/// Poll interval.
const POLL_INTERVAL: Duration = Duration::from_millis(120);

/// Holds an exclusive lock. Released on `Drop`.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    /// Acquires the lock, waiting at most `timeout`.
    ///
    /// # Errors
    ///
    /// - [`KitError::LockTimeout`]: another holder still has it when the timeout expires.
    /// - [`KitError::LockIo`]: an IO error, or the lock file's parent directory could
    ///   not be created.
    pub fn acquire(path: &Path, timeout: Duration) -> KitResult<Self> {
        Self::acquire_with_stale_after(path, timeout, STALE_AFTER)
    }

    /// Like [`Self::acquire`], but with a caller-supplied staleness threshold.
    ///
    /// The hook exists for testability: setting the threshold to 0 exercises the
    /// takeover path without touching file timestamps (which needs platform APIs).
    pub(crate) fn acquire_with_stale_after(
        path: &Path,
        timeout: Duration,
        stale_after: Duration,
    ) -> KitResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| KitError::LockIo {
                path: path.to_path_buf(),
                source,
            })?;
        }

        let deadline = Instant::now() + timeout;

        loop {
            match Self::try_create(path) {
                Ok(lock) => return Ok(lock),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Someone else holds it. First check whether it is stale.
                    if Self::take_over_if_stale(path, stale_after)? {
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(KitError::LockTimeout {
                            path: path.to_path_buf(),
                            secs: timeout.as_secs(),
                        });
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(source) => {
                    return Err(KitError::LockIo {
                        path: path.to_path_buf(),
                        source,
                    })
                }
            }
        }
    }

    fn try_create(path: &Path) -> std::io::Result<Self> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;

        // Some diagnostic content. Failure does not matter — the lock is already held.
        let _ = writeln!(f, "pid={} at={:?}", std::process::id(), SystemTime::now());

        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Deletes the lock file if it is too old, returning `true` to mean "it is gone,
    /// retry now".
    fn take_over_if_stale(path: &Path, stale_after: Duration) -> KitResult<bool> {
        let Ok(meta) = std::fs::metadata(path) else {
            // Already gone (someone just released it): let the caller retry.
            return Ok(true);
        };
        let Ok(modified) = meta.modified() else {
            return Ok(false);
        };
        let Ok(age) = SystemTime::now().duration_since(modified) else {
            // The timestamp is in the future — the clock is broken, so do not take over.
            return Ok(false);
        };

        if age < stale_after {
            return Ok(false);
        }

        match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            // Someone else removed it at the same moment — even better; let the
            // caller retry.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(source) => Err(KitError::LockIo {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(".lock")
    }

    #[test]
    fn acquires_and_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        // The parent does not exist yet — acquire must create it
        let path = dir.path().join("nested").join("deeper").join(".lock");

        let lock = FileLock::acquire(&path, Duration::from_millis(50)).expect("should acquire");
        assert!(path.is_file(), "the lock file should have been created");
        drop(lock);
    }

    #[test]
    fn releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        {
            let _lock = FileLock::acquire(&path, Duration::from_millis(50)).unwrap();
            assert!(path.is_file());
        }

        assert!(!path.exists(), "the lock file should be deleted on drop");
    }

    /// The second holder has to wait, and reports `LockTimeout` when it runs out of
    /// time — it must not silently proceed.
    #[test]
    fn a_second_holder_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        let _first = FileLock::acquire(&path, Duration::from_millis(50)).unwrap();

        let err = FileLock::acquire(&path, Duration::from_millis(120));
        assert!(
            matches!(err, Err(KitError::LockTimeout { .. })),
            "expected LockTimeout, got {err:?}"
        );
    }

    /// Once the lock is released, a later arrival should get it immediately.
    #[test]
    fn a_later_holder_succeeds_after_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        let first = FileLock::acquire(&path, Duration::from_millis(50)).unwrap();
        drop(first);

        let _second = FileLock::acquire(&path, Duration::from_millis(200)).expect("should acquire");
    }

    /// A stale lock (left behind by `kill -9`) has to be taken over, otherwise the
    /// user stays blocked forever.
    ///
    /// With the staleness threshold set to 0, any existing lock counts as stale —
    /// which keeps the test portable, since changing file timestamps needs platform
    /// APIs.
    #[test]
    fn takes_over_a_stale_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        std::fs::write(&path, "pid=1 at=<stale>").unwrap();

        let _lock =
            FileLock::acquire_with_stale_after(&path, Duration::from_millis(500), Duration::ZERO)
                .expect("a stale lock should be taken over");
    }

    /// A fresh lock must not be stolen.
    #[test]
    fn does_not_steal_a_fresh_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        std::fs::write(&path, "pid=1 at=<fresh>").unwrap();

        // Threshold left at the default: a lock just written is nowhere near stale
        let err =
            FileLock::acquire_with_stale_after(&path, Duration::from_millis(120), STALE_AFTER);
        assert!(
            matches!(err, Err(KitError::LockTimeout { .. })),
            "a fresh lock must not be stolen"
        );
    }

    /// The lock file carries the pid, which makes it easy to see who holds it.
    #[test]
    fn writes_diagnostic_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        let _lock = FileLock::acquire(&path, Duration::from_millis(50)).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("pid="), "lock file content: {text:?}");
        assert!(text.contains(&std::process::id().to_string()));
    }
}
