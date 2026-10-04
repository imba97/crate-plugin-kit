//! Installation: turning a plugin crate on crates.io into a loadable cdylib plus
//! its manifest on disk.
//!
//! Two routes, chosen by [`crate::CratePluginKit::install`] based on
//! [`crate::KitConfig::prefer_prebuilt`]:
//!
//! ```text
//! prefer_prebuilt = true
//!   └─ prebuilt::try_install ──(no prebuilt published / cannot download)──> build_host::install
//! prefer_prebuilt = false
//!   └─ build_host::install
//! ```
//!
//! build-host is the default and the fallback: it goes through `cargo build` and
//! therefore uses whatever registry or mirror the user has configured. prebuilt
//! downloads from GitHub bypass those settings, so it is only an accelerator.

pub mod build_host;
pub mod prebuilt;

use std::path::PathBuf;

use crate::cache::InstallSource;

/// The artifact of one successful install.
#[derive(Debug, Clone)]
pub struct Installed {
    /// Full crate name, such as `bmux-plugin-cargo`.
    pub crate_name: String,
    /// The version actually installed.
    pub version: String,
    /// Install dir.
    pub dir: PathBuf,
    /// Path of the cdylib that landed on disk.
    pub library: PathBuf,
    /// Where it came from.
    pub source: InstallSource,
}

/// Removes a directory (treating a missing one as success).
pub(crate) fn remove_dir_if_exists(dir: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
