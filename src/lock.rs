//! 安装 / 卸载期间的排他文件锁。
//!
//! 两个终端同时 `plugin add` 会写坏 `.plugins.json`、或让两个 `cargo build`
//! 往同一个 wrapper 目录里写。用一把**跨进程**的锁挡住。
//!
//! # 实现
//!
//! 用 `create_new(true)` 创建锁文件 —— 这个操作在主流文件系统上是原子的，
//! 谁创建成功谁持锁。内容写入 pid 与时间戳，方便排查。
//!
//! **不做 OS 级文件锁**（`flock` / `LockFileEx`）：那需要额外的平台代码，
//! 而本 crate 已经有一个更简单的判据 —— 锁文件是否存在 + 是否过期。
//! 代价是"进程被 kill -9 会留下陈旧的锁文件"，所以加了过期接管。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::error::{KitError, KitResult};

/// 锁文件多久算过期。超过这个年龄的锁会被后来者接管。
///
/// 取值要比任何合理的安装耗时长（`cargo build` 可能好几分钟），
/// 又要比"用户已经忘了这件事"的时长短。
pub const STALE_AFTER: Duration = Duration::from_secs(30 * 60);

/// 轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(120);

/// 持有一把排他锁。`Drop` 时释放。
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    /// 取锁，最多等 `timeout`。
    ///
    /// # Errors
    ///
    /// - [`KitError::LockTimeout`]：超时仍是别人持有。
    /// - [`KitError::LockIo`]：IO 出错，或锁文件所在目录建不出来。
    pub fn acquire(path: &Path, timeout: Duration) -> KitResult<Self> {
        Self::acquire_with_stale_after(path, timeout, STALE_AFTER)
    }

    /// 同 [`Self::acquire`]，但可以指定"多久算过期"。
    ///
    /// 单独开这个口子是为了可测：把阈值设成 0，就能在不碰文件时间戳（那需要平台 API）
    /// 的前提下验证接管逻辑。
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
                    // 别人持着。先看看是不是过期了。
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

        // 写点诊断信息。失败无所谓 —— 锁本身已经拿到了。
        let _ = writeln!(f, "pid={} at={:?}", std::process::id(), SystemTime::now());

        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// 锁文件太老就把它删掉，返回 `true` 表示"删掉了，可以重试"。
    fn take_over_if_stale(path: &Path, stale_after: Duration) -> KitResult<bool> {
        let Ok(meta) = std::fs::metadata(path) else {
            // 已经没了（别人刚释放），直接让调用方重试。
            return Ok(true);
        };
        let Ok(modified) = meta.modified() else {
            return Ok(false);
        };
        let Ok(age) = SystemTime::now().duration_since(modified) else {
            // 时间戳在未来 —— 时钟有问题，保守起见不接管。
            return Ok(false);
        };

        if age < stale_after {
            return Ok(false);
        }

        match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            // 别人刚好也删了 —— 那更好，让调用方重试。
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
        // 父目录还不存在 —— acquire 应当自己建出来
        let path = dir.path().join("nested").join("deeper").join(".lock");

        let lock = FileLock::acquire(&path, Duration::from_millis(50)).expect("应当能取到锁");
        assert!(path.is_file(), "锁文件应当已创建");
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

        assert!(!path.exists(), "drop 之后锁文件应当被删掉");
    }

    /// 第二个持有者要等，等到超时就报 `LockTimeout` —— 而不是静默放行。
    #[test]
    fn a_second_holder_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        let _first = FileLock::acquire(&path, Duration::from_millis(50)).unwrap();

        let err = FileLock::acquire(&path, Duration::from_millis(120));
        assert!(
            matches!(err, Err(KitError::LockTimeout { .. })),
            "期望 LockTimeout，得到 {err:?}"
        );
    }

    /// 拿到锁之后再释放，后来者应当能立刻接上。
    #[test]
    fn a_later_holder_succeeds_after_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        let first = FileLock::acquire(&path, Duration::from_millis(50)).unwrap();
        drop(first);

        let _second = FileLock::acquire(&path, Duration::from_millis(200)).expect("应当能接上");
    }

    /// 陈旧的锁（`kill -9` 的遗留物）要被接管，否则用户会被永久挡住。
    ///
    /// 把 stale 阈值设成 0，任何已存在的锁都算过期 —— 这样不用去改文件时间戳
    /// （那要平台 API），测试是可移植的。
    #[test]
    fn takes_over_a_stale_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        std::fs::write(&path, "pid=1 at=<stale>").unwrap();

        let _lock =
            FileLock::acquire_with_stale_after(&path, Duration::from_millis(500), Duration::ZERO)
                .expect("陈旧的锁应当被接管");
    }

    /// 新鲜的锁不能被抢。
    #[test]
    fn does_not_steal_a_fresh_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        std::fs::write(&path, "pid=1 at=<fresh>").unwrap();

        // 阈值给足，刚写下的锁远没到过期
        let err =
            FileLock::acquire_with_stale_after(&path, Duration::from_millis(120), STALE_AFTER);
        assert!(
            matches!(err, Err(KitError::LockTimeout { .. })),
            "新鲜的锁不该被抢走"
        );
    }

    /// 锁文件写着 pid，方便排查是谁占着。
    #[test]
    fn writes_diagnostic_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        let _lock = FileLock::acquire(&path, Duration::from_millis(50)).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("pid="), "锁文件内容：{text:?}");
        assert!(text.contains(&std::process::id().to_string()));
    }
}
