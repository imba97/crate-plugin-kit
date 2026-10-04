//! [`CratePluginKit`]: the facade of this crate.
//!
//! # The generic parameter `T`
//!
//! `T` is the host's own `#[repr(C)]` entry struct. This crate does not use any of
//! its fields — it only appears in the return type of [`CratePluginKit::load`], to
//! turn the symbol `dlopen` produced into a thin pointer `*const T`.
//!
//! That removes the "type erasure → restore" layer: there is no conversion of a fat
//! pointer between two vtables, and therefore no UB of that kind.
//!
//! # Thread safety
//!
//! [`CratePluginKit`] holds a `RefCell` recording which plugins this process has
//! loaded (used by [`CratePluginKit::uninstall`] to refuse deleting a library that
//! is in use). It is therefore not `Sync` — build one per process and use it
//! serially (which is what a CLI does). To go concurrent, give each thread its own
//! kit in its own scope.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use crate::cache::{now_unix, Index, IndexEntry, InstallSource};
use crate::config::{KitConfig, KitPaths};
use crate::error::{KitError, KitResult};
use crate::install::{self, build_host, prebuilt, Installed};
use crate::loader::{self, LoadedPlugin};
use crate::lock::FileLock;
use crate::manifest::PluginManifest;
use crate::registry::{CrateInfo, CrateSummary, Registry};

/// An overview of one installed plugin.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    /// Plugin's self-declared name (the manifest's `plugin.name`).
    pub name: String,
    /// Full crate name, such as `bmux-plugin-cargo`.
    pub crate_name: String,
    /// Version.
    pub version: String,
    /// Ecosystem family (a host field; may be empty).
    pub family: Option<String>,
    /// ABI version (a host field; may be empty).
    pub abi: Option<u32>,
    /// Install dir.
    pub dir: PathBuf,
    /// How it got installed.
    pub source: InstallSource,
}

/// Generic plugin management library.
///
/// Build one and use it serially.
pub struct CratePluginKit<T> {
    cfg: KitConfig,
    paths: KitPaths,
    registry: Registry,
    /// Crate names this process has `dlopen`ed. `uninstall` uses it to refuse
    /// deleting a library that is in use.
    loaded: RefCell<BTreeSet<String>>,
    /// `fn() -> T` rather than `T`: does not pretend to own a `T`, and does not
    /// require `T: Send/Sync`.
    _host: PhantomData<fn() -> T>,
}

impl<T> CratePluginKit<T> {
    /// Builds a kit instance from the config. Creates the data directories.
    pub fn new(cfg: KitConfig) -> KitResult<Self> {
        let paths = cfg.paths()?;
        std::fs::create_dir_all(&paths.plugins)?;
        std::fs::create_dir_all(&paths.build)?;

        let registry = Registry::new(&cfg);

        Ok(Self {
            cfg,
            paths,
            registry,
            loaded: RefCell::new(BTreeSet::new()),
            _host: PhantomData,
        })
    }

    /// Config.
    pub fn config(&self) -> &KitConfig {
        &self.cfg
    }

    /// The various paths.
    pub fn paths(&self) -> &KitPaths {
        &self.paths
    }

    /// Registry client.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    // ---- install / uninstall ----------------------------------------------

    /// Installs a plugin.
    ///
    /// `name` may be a short name (`"cargo"`, to which [`KitConfig::crate_prefix`]
    /// is prepended) or a full crate name. With `version` set to `None`, the latest
    /// version is looked up on crates.io.
    ///
    /// # Errors
    ///
    /// - [`KitError::AlreadyInstalled`]: it is already installed. Use [`Self::update`]
    ///   to replace it.
    pub fn install(&self, name: &str, version: Option<&str>) -> KitResult<Installed> {
        let crate_name = self.cfg.normalize_crate_name(name);

        let _lock = self.lock()?;

        let dir = self.paths.plugin_dir(&crate_name);
        if dir.is_dir() {
            return Err(KitError::AlreadyInstalled {
                name: crate_name,
                path: dir,
            });
        }

        let version = self.resolve_version(&crate_name, version)?;
        let installed = self.install_locked(&crate_name, &version)?;
        self.record(&installed)?;
        Ok(installed)
    }

    /// Updates (or reinstalls) a plugin. Replaces it if present, installs it if not.
    pub fn update(&self, name: &str, version: Option<&str>) -> KitResult<Installed> {
        let crate_name = self.cfg.normalize_crate_name(name);

        let _lock = self.lock()?;

        let version = self.resolve_version(&crate_name, version)?;
        let installed = self.install_locked(&crate_name, &version)?;
        self.record(&installed)?;
        Ok(installed)
    }

    /// Uninstalls a plugin.
    ///
    /// # Errors
    ///
    /// - [`KitError::NotInstalled`]: it was not installed in the first place.
    /// - [`KitError::PluginInUse`]: this process has loaded its library. A `.dll`
    ///   that is currently `dlopen`ed cannot be deleted on Windows; on Linux the
    ///   removal succeeds but the disk space is not reclaimed. Rather than gamble on
    ///   it, ask the user to retry from a clean process.
    pub fn uninstall(&self, name: &str) -> KitResult<()> {
        let crate_name = self.cfg.normalize_crate_name(name);

        let _lock = self.lock()?;

        let dir = self.paths.plugin_dir(&crate_name);
        if !dir.is_dir() {
            return Err(KitError::NotInstalled { name: crate_name });
        }

        if self.loaded.borrow().contains(&crate_name) {
            return Err(KitError::PluginInUse {
                name: crate_name.clone(),
            });
        }

        std::fs::remove_dir_all(&dir)?;

        let mut idx = Index::load(&self.paths.index_file);
        idx.remove(&crate_name);
        idx.save(&self.paths.index_file)?;

        Ok(())
    }

    // ---- queries ----------------------------------------------------------

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

            out.push(PluginInfo {
                name: manifest.plugin.name.clone(),
                crate_name,
                version: manifest.plugin.version.clone(),
                family: manifest.plugin.family.clone(),
                abi: manifest.plugin.abi,
                dir: entry.path(),
                source,
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

    // ---- loading ----------------------------------------------------------

    /// Loads a plugin and returns a `*const T`.
    ///
    /// Once loaded, [`Self::uninstall`] refuses to delete it until the process exits.
    pub fn load(&self, name: &str) -> KitResult<LoadedPlugin<T>> {
        let crate_name = self.cfg.normalize_crate_name(name);

        let manifest = self.manifest_of(&crate_name)?;
        let dir = self.paths.plugin_dir(&crate_name);
        let stem = manifest.effective_lib_stem(&self.cfg, &crate_name);
        let lib_path = loader::find_library(&dir, &stem)?;

        // SAFETY: `lib_path` was installed by this kit; whether the layout of `T`
        // matches the struct it exports is covered by the host's abi_version field —
        // that is the host contract crate's job, not this crate's.
        let plugin = unsafe { loader::open::<T>(&lib_path, &self.cfg.entry_symbol)? };

        self.loaded.borrow_mut().insert(crate_name);
        Ok(plugin)
    }

    // ---- registry --------------------------------------------------------

    /// Searches for crates.
    pub fn search(&self, keyword: &str, limit: usize) -> KitResult<Vec<CrateSummary>> {
        self.registry.search(keyword, limit)
    }

    /// Looks up a crate by exact name.
    pub fn view(&self, name: &str) -> KitResult<Option<CrateInfo>> {
        let crate_name = self.cfg.normalize_crate_name(name);
        self.registry.view(&crate_name)
    }

    // ---- internals -------------------------------------------------------

    fn lock(&self) -> KitResult<FileLock> {
        FileLock::acquire(&self.paths.lock_file, self.cfg.lock_timeout)
    }

    /// Looks up the latest version when none was given.
    fn resolve_version(&self, crate_name: &str, version: Option<&str>) -> KitResult<String> {
        if let Some(v) = version {
            return Ok(v.to_string());
        }
        match self.registry.view(crate_name)? {
            Some(info) if !info.version.is_empty() => Ok(info.version),
            Some(_) => Err(KitError::Registry(format!(
                "no usable version of {crate_name} in the registry"
            ))),
            None => Err(KitError::Registry(format!(
                "{crate_name} was not found in the registry"
            ))),
        }
    }

    /// The actual install. The caller must already hold the lock.
    fn install_locked(&self, crate_name: &str, version: &str) -> KitResult<Installed> {
        // Clear the old directory first (the update path)
        install::remove_dir_if_exists(&self.paths.plugin_dir(crate_name))?;

        if self.cfg.prefer_prebuilt {
            match prebuilt::try_install(&self.cfg, &self.paths, &self.registry, crate_name, version)
            {
                Ok(Some(installed)) => return Ok(installed),
                // No prebuilt asset — this is exactly the signal to fall back to
                // build-host
                Ok(None) => {}
                // Broken network or a corrupt asset: fall back as well. build-host
                // goes through cargo, which is a different route from GitHub and may
                // well work.
                Err(_) => {}
            }
        }

        build_host::install(&self.cfg, &self.paths, crate_name, version)
    }

    fn record(&self, installed: &Installed) -> KitResult<()> {
        let mut idx = Index::load(&self.paths.index_file);

        let abi = PluginManifest::read(
            &self
                .paths
                .manifest_path(&installed.crate_name, &self.cfg.manifest_name),
        )
        .ok()
        .and_then(|m| m.plugin.abi);

        idx.insert(
            &installed.crate_name,
            IndexEntry {
                version: installed.version.clone(),
                abi,
                root: installed.dir.clone(),
                source: installed.source,
                installed_at: now_unix(),
            },
        );
        idx.save(&self.paths.index_file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A host ABI struct for tests.
    ///
    /// Its contents are arbitrary — this crate never reads a field of `T`, it only
    /// casts the symbol to `*const T`.
    #[repr(C)]
    struct FakeEntry {
        abi_version: u32,
    }

    const MANIFEST: &str = r#"
[plugin]
name    = "foo"
version = "0.1.0"
abi     = 1
family  = "node"

[detect]
strong = ["fake.lock"]
"#;

    fn kit(root: &Path) -> CratePluginKit<FakeEntry> {
        let cfg = KitConfig::new("myapp")
            .with_data_dir(root)
            .with_lock_timeout(Duration::from_millis(500));
        CratePluginKit::new(cfg).expect("should build")
    }

    /// Lays out an "installed" plugin directory by hand, skipping the compile step.
    fn place_plugin(root: &Path, crate_name: &str) -> PathBuf {
        let dir = root.join("plugins").join(crate_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("myapp-plugin.toml"), MANIFEST).unwrap();
        dir
    }

    #[test]
    fn new_creates_the_data_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");

        let k = kit(&root);
        assert!(
            k.paths().plugins.is_dir(),
            "the plugins dir should have been created"
        );
        assert!(
            k.paths().build.is_dir(),
            "the build dir should have been created"
        );
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
    fn uninstall_reports_a_plugin_that_is_not_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let k = kit(&tmp.path().join("store"));

        assert!(matches!(
            k.uninstall("nope"),
            Err(KitError::NotInstalled { .. })
        ));
    }

    #[test]
    fn uninstall_removes_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);
        let dir = place_plugin(&root, "myapp-plugin-foo");

        assert!(dir.is_dir());
        k.uninstall("foo").expect("should uninstall");
        assert!(!dir.exists(), "the directory should have been deleted");
        assert!(k.list().unwrap().is_empty());
    }

    /// `install` does not overwrite an existing installation — use `update` to
    /// replace it.
    ///
    /// The check happens after the lock is taken and before any network or compile
    /// work, so the test needs no network access.
    #[test]
    fn install_refuses_when_already_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);
        place_plugin(&root, "myapp-plugin-foo");

        match k.install("foo", Some("0.1.0")) {
            Err(KitError::AlreadyInstalled { name, .. }) => {
                assert_eq!(name, "myapp-plugin-foo")
            }
            other => panic!("expected AlreadyInstalled, got {other:?}"),
        }
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

    #[test]
    fn config_is_exposed() {
        let tmp = tempfile::tempdir().unwrap();
        let k = kit(&tmp.path().join("store"));
        assert_eq!(k.config().id, "myapp");
        assert_eq!(k.config().crate_prefix, "myapp-plugin-");
    }
}
