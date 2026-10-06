//! Queries: what is installed, and what crates.io has.
//!
//! These never take the lock and never write: they read the plugin directories under
//! `<data-dir>/plugins`, and the last two forward to the registry.

use std::path::{Path, PathBuf};

use crate::cache::Index;
use crate::error::KitResult;
use crate::manifest::PluginManifest;
use crate::registry::{CrateInfo, CrateSummary};

use super::{CratePluginKit, PluginInfo};

impl<T> CratePluginKit<T> {
    /// Lists the installed plugins.
    ///
    /// The manifests on disk are authoritative; `.plugins.json` only supplies
    /// information the manifest does not carry, such as the source and install time.
    /// A lost cache therefore never hides a plugin.
    pub fn list(&self) -> KitResult<Vec<PluginInfo>> {
        let idx = Index::load(&self.paths.index_file);
        let mut out = Vec::new();

        let entries = match std::fs::read_dir(&self.paths.plugins) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };

        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let crate_name = entry.file_name().to_string_lossy().into_owned();

            // The directory is there but the manifest is missing or broken: skip it,
            // so one bad plugin cannot make the whole list fail
            let manifest_path = entry.path().join(&self.cfg.manifest_name);
            let Ok(manifest) = PluginManifest::read(&manifest_path) else {
                continue;
            };

            let source = idx.get(&crate_name).map(|e| e.source).unwrap_or_default();

            // Broken apart rather than cloned: the summary and the host's own sections
            // are all this read is for, so nothing is copied on the way out.
            let PluginManifest { plugin, extra, .. } = manifest;

            out.push(PluginInfo {
                name: plugin.name,
                crate_name,
                version: plugin.version,
                family: plugin.family,
                abi: plugin.abi,
                dir: entry.path(),
                source,
                extra,
            });
        }

        out.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));
        Ok(out)
    }

    /// Reads the manifest of one plugin.
    pub fn manifest_of(&self, name: &str) -> KitResult<PluginManifest> {
        let crate_name = self.cfg.normalize_crate_name(name);
        let path = self
            .paths
            .manifest_path(&crate_name, &self.cfg.manifest_name);
        PluginManifest::read(&path)
    }

    /// Resolves a path relative to `name` into an absolute path.
    pub fn resolve(&self, name: &str, relative: impl AsRef<Path>) -> PathBuf {
        let crate_name = self.cfg.normalize_crate_name(name);
        self.paths.plugin_dir(&crate_name).join(relative)
    }

    /// Searches for crates.
    pub fn search(&self, keyword: &str, limit: usize) -> KitResult<Vec<CrateSummary>> {
        self.registry.search(keyword, limit)
    }

    /// Looks up a crate by exact name.
    pub fn view(&self, name: &str) -> KitResult<Option<CrateInfo>> {
        let crate_name = self.cfg.normalize_crate_name(name);
        self.registry.view(&crate_name)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{kit, place_plugin};
    use crate::cache::InstallSource;
    use crate::error::KitError;

    /// A host's own sections come back with the summary.
    ///
    /// This is what saves a host a second read of the manifest: it asked the kit what is
    /// installed, so the kit hands over the sections it read on the way -- `[detect]` here,
    /// which the kit itself has no opinion about.
    #[test]
    fn the_hosts_own_sections_come_back_with_the_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        place_plugin(&root, "myapp-plugin-foo");

        let k = kit(&root);
        let infos = k.list().unwrap();

        assert_eq!(infos.len(), 1);
        let strong = infos[0]
            .extra
            .get("detect")
            .and_then(|detect| detect.get("strong"))
            .and_then(|strong| strong.as_array())
            .expect("the [detect] section has to survive the listing");
        assert_eq!(strong.len(), 1);
        assert_eq!(strong[0].as_str(), Some("fake.lock"));

        assert_eq!(infos[0].name, "foo");
        assert_eq!(infos[0].family.as_deref(), Some("node"));
        assert_eq!(infos[0].abi, Some(1));
    }

    #[test]
    fn a_fresh_store_lists_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let k = kit(&tmp.path().join("store"));
        assert!(k.list().unwrap().is_empty());
    }

    /// When the directory does not exist at all, `list` still returns an empty list
    /// instead of an error.
    #[test]
    fn list_tolerates_a_missing_plugins_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        std::fs::remove_dir_all(&root).unwrap();
        assert!(k.list().unwrap().is_empty());
    }

    /// Key behavior: the manifest on disk is the source of truth, not
    /// `.plugins.json`. A plugin placed by hand (with no cache entry) must still be
    /// listed.
    #[test]
    fn list_reads_manifests_from_disk_not_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        place_plugin(&root, "myapp-plugin-foo");

        let all = k.list().unwrap();
        assert_eq!(all.len(), 1, "the cache is empty but the disk has a plugin");
        let info = &all[0];
        assert_eq!(info.name, "foo");
        assert_eq!(info.crate_name, "myapp-plugin-foo");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.abi, Some(1));
        assert_eq!(info.family.as_deref(), Some("node"));
        // No cache entry → falls back to the default source
        assert_eq!(info.source, InstallSource::BuildHost);
    }

    /// A broken manifest must not fail the whole `list()` — skipping it is enough.
    #[test]
    fn list_skips_a_directory_with_a_broken_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        place_plugin(&root, "myapp-plugin-good");
        let bad = root.join("plugins").join("myapp-plugin-bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("myapp-plugin.toml"), "this is not toml").unwrap();

        // A directory that does not even have a manifest
        let empty = root.join("plugins").join("myapp-plugin-empty");
        std::fs::create_dir_all(&empty).unwrap();

        let all = k.list().unwrap();
        assert_eq!(all.len(), 1, "only the good one should be listed: {all:?}");
        assert_eq!(all[0].crate_name, "myapp-plugin-good");
    }

    #[test]
    fn list_is_sorted_by_crate_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        for name in ["myapp-plugin-c", "myapp-plugin-a", "myapp-plugin-b"] {
            place_plugin(&root, name);
        }

        let names: Vec<_> = k
            .list()
            .unwrap()
            .into_iter()
            .map(|i| i.crate_name)
            .collect();
        assert_eq!(
            names,
            vec!["myapp-plugin-a", "myapp-plugin-b", "myapp-plugin-c"]
        );
    }

    #[test]
    fn manifest_of_reads_the_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);
        place_plugin(&root, "myapp-plugin-foo");

        let m = k
            .manifest_of("foo")
            .expect("the short name should work too");
        assert_eq!(m.plugin.name, "foo");
        assert!(m.extra.contains_key("detect"), "the host's section is kept");
    }

    #[test]
    fn manifest_of_reports_a_missing_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let k = kit(&tmp.path().join("store"));
        assert!(matches!(
            k.manifest_of("nope"),
            Err(KitError::ManifestRead { .. })
        ));
    }

    #[test]
    fn resolve_points_into_the_plugin_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        assert_eq!(
            k.resolve("foo", "extra.txt"),
            root.join("plugins")
                .join("myapp-plugin-foo")
                .join("extra.txt")
        );
        // The short and the full name must land in the same place
        assert_eq!(k.resolve("foo", "x"), k.resolve("myapp-plugin-foo", "x"));
    }
}
