//! Install record cache `<plugins>/.plugins.json`.
//!
//! This file is for speed and display only, never a source of truth — the source
//! of truth is always the manifest in each plugin dir. If the cache is lost or
//! corrupt, `list()` rebuilds from disk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::KitResult;

/// Current cache schema version.
pub const CACHE_VERSION: u32 = 1;

/// Contents of `.plugins.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    /// Schema version.
    #[serde(rename = "cacheVersion", default = "default_cache_version")]
    pub cache_version: u32,

    /// Crate name → install record.
    #[serde(default)]
    pub plugins: BTreeMap<String, IndexEntry>,
}

fn default_cache_version() -> u32 {
    CACHE_VERSION
}

impl Default for Index {
    fn default() -> Self {
        Self {
            cache_version: CACHE_VERSION,
            plugins: BTreeMap::new(),
        }
    }
}

/// One install record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Installed version.
    pub version: String,

    /// ABI version declared by the manifest (for the host's own use).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi: Option<u32>,

    /// Absolute path of the install dir.
    pub root: PathBuf,

    /// How it was installed.
    #[serde(default)]
    pub source: InstallSource,

    /// Install time (Unix seconds). An integer instead of an RFC3339 string, which
    /// saves a date library dependency.
    #[serde(rename = "installedAt", default)]
    pub installed_at: u64,
}

/// How a plugin got installed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallSource {
    /// Prebuilt artifact downloaded from GitHub Releases.
    Prebuilt,
    /// Compiled locally by `cargo build` (the default path).
    #[default]
    BuildHost,
    /// Compiled from a checkout on this machine.
    ///
    /// The path is kept because it is what `update` rebuilds from: a plugin that was
    /// installed from a directory has no registry to look up, and rebuilding the same
    /// directory is what updating it means.
    Local {
        /// The directory that was installed from.
        path: PathBuf,
    },
}

/// Current Unix seconds. Returns 0 when the system time is unavailable — a wrong
/// timestamp beats a failed install.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Index {
    /// Reads the cache. A missing or unparsable file yields an empty index — the
    /// cache is not a source of truth.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut idx) => {
                idx.cache_version = CACHE_VERSION;
                idx
            }
            Err(_) => Self::default(),
        }
    }

    /// Writes the cache (atomic replace: write a temp file, then rename).
    pub fn save(&self, path: &Path) -> KitResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| {
            crate::error::KitError::Registry(format!("failed to serialize index: {e}"))
        })?;

        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Records one entry.
    pub fn insert(&mut self, crate_name: &str, entry: IndexEntry) {
        self.plugins.insert(crate_name.to_string(), entry);
    }

    /// Removes one entry.
    pub fn remove(&mut self, crate_name: &str) -> Option<IndexEntry> {
        self.plugins.remove(crate_name)
    }

    /// Looks up one entry.
    pub fn get(&self, crate_name: &str) -> Option<&IndexEntry> {
        self.plugins.get(crate_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(version: &str) -> IndexEntry {
        IndexEntry {
            version: version.to_string(),
            abi: Some(1),
            root: PathBuf::from("some").join("plugin"),
            source: InstallSource::BuildHost,
            installed_at: 1_767_225_600,
        }
    }

    #[test]
    fn insert_get_remove_round_trip() {
        let mut idx = Index::default();
        assert!(idx.get("a").is_none());

        idx.insert("a", entry("0.1.0"));
        assert_eq!(idx.get("a").unwrap().version, "0.1.0");

        let removed = idx.remove("a");
        assert_eq!(removed.unwrap().version, "0.1.0");
        assert!(idx.get("a").is_none());
        assert!(idx.remove("a").is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(".plugins.json");

        let mut idx = Index::default();
        idx.insert("myapp-plugin-foo", entry("0.2.0"));
        idx.save(&path).expect("should be able to write to disk");

        let back = Index::load(&path);
        assert_eq!(back.cache_version, CACHE_VERSION);
        let got = back.get("myapp-plugin-foo").unwrap();
        assert_eq!(got.version, "0.2.0");
        assert_eq!(got.abi, Some(1));
        assert_eq!(got.source, InstallSource::BuildHost);
        assert_eq!(got.installed_at, 1_767_225_600);
    }

    /// The cache is not a source of truth: a missing file yields an empty index
    /// rather than an error.
    #[test]
    fn a_missing_file_yields_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let idx = Index::load(&dir.path().join("nope.json"));
        assert!(idx.plugins.is_empty());
        assert_eq!(idx.cache_version, CACHE_VERSION);
    }

    /// Same as above: a corrupt file is also treated as empty, so it cannot drag
    /// down the whole `list()`.
    #[test]
    fn a_corrupt_file_yields_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".plugins.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let idx = Index::load(&path);
        assert!(idx.plugins.is_empty());
    }

    /// Older caches may lack the `source` field — it must read back and fall back
    /// to the default.
    #[test]
    fn missing_optional_fields_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".plugins.json");
        std::fs::write(&path, r#"{"plugins":{"a":{"version":"0.1.0","root":"r"}}}"#).unwrap();

        let idx = Index::load(&path);
        let got = idx.get("a").unwrap();
        assert_eq!(got.source, InstallSource::BuildHost);
        assert_eq!(got.abi, None);
        assert_eq!(got.installed_at, 0);
    }

    /// Saving writes a temp file and then renames, so no `.tmp` leftovers should
    /// remain.
    #[test]
    fn save_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".plugins.json");

        let mut idx = Index::default();
        idx.insert("a", entry("0.1.0"));
        idx.save(&path).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "left a temp file behind: {leftovers:?}"
        );
    }

    #[test]
    fn now_unix_is_after_the_epoch() {
        assert!(now_unix() > 1_600_000_000);
    }
}
