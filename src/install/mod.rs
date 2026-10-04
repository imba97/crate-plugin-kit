//! 安装：把 crates.io 上的一个插件 crate 变成磁盘上可 `dlopen` 的 cdylib + manifest。
//!
//! 两条路，由 [`crate::CratePluginKit::install`] 按 [`crate::KitConfig::prefer_prebuilt`] 选：
//!
//! ```text
//! prefer_prebuilt = true
//!   └─ prebuilt::try_install ──(没发 prebuilt / 下不到)──> build_host::install
//! prefer_prebuilt = false
//!   └─ build_host::install
//! ```
//!
//! **build-host 是默认与兜底**：它走 `cargo build`，因此天然使用用户已经配好的
//! registry / 镜像。prebuilt 从 GitHub 下载会绕开这些配置，所以只当加速项。

pub mod build_host;
pub mod prebuilt;

use std::path::PathBuf;

use crate::cache::InstallSource;

/// 一次成功安装的产物。
#[derive(Debug, Clone)]
pub struct Installed {
    /// 完整 crate 名，如 `bmux-plugin-cargo`。
    pub crate_name: String,
    /// 实际安装的版本。
    pub version: String,
    /// 安装目录。
    pub dir: PathBuf,
    /// 落地的 cdylib 路径。
    pub library: PathBuf,
    /// 怎么来的。
    pub source: InstallSource,
}

/// 清理一个目录（不存在也算成功）。
pub(crate) fn remove_dir_if_exists(dir: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
