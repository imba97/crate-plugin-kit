//! Where a pack run does its work.
//!
//! The generated wrapper is scaffolding, not an artifact: it has to exist somewhere
//! cargo can build it, and it must not collide with any other run doing the same thing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::KitConfig;

/// Where this run's generated wrapper lives: `<temp>/<id>-plugin-asset-<pid>-<n>/<crate>`.
///
/// Unique per call, not per crate: two runs at once (two threads packing, or two `plugin
/// asset` processes) must not delete and rewrite each other's files. The crate name stays in
/// the path so a leftover directory is still identifiable, and the process id separates
/// concurrent processes.
///
/// A failed build leaves the directory behind, which is the point — that is where someone
/// looks. A successful one removes the per-run directory (its parent) again.
pub(super) fn wrapper_dir(cfg: &KitConfig, crate_name: &str) -> PathBuf {
    static RUN: AtomicU64 = AtomicU64::new(0);

    let run = RUN.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "{}-plugin-asset-{}-{run}",
            cfg.id,
            std::process::id()
        ))
        .join(crate_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this exists for: one directory per crate, shared by concurrent runs,
    /// meant one run could be deleting files another was writing or compiling — which on
    /// Windows fails outright with "access denied".
    #[test]
    fn every_run_gets_its_own_wrapper_dir() {
        let cfg = KitConfig::new("myapp");

        let first = wrapper_dir(&cfg, "myapp-plugin-foo");
        let second = wrapper_dir(&cfg, "myapp-plugin-foo");
        assert_ne!(first, second, "{first:?} must not be reused");

        // Still identifiable: the crate name is the last component and the app id leads.
        assert!(first.ends_with("myapp-plugin-foo"), "{}", first.display());
        assert!(
            first
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().starts_with("myapp-plugin-asset"))
                .unwrap_or(false),
            "{}",
            first.display()
        );
    }

    #[test]
    fn two_crates_do_not_share_a_directory() {
        let cfg = KitConfig::new("myapp");

        assert_ne!(
            wrapper_dir(&cfg, "myapp-plugin-foo"),
            wrapper_dir(&cfg, "myapp-plugin-bar")
        );
    }
}
