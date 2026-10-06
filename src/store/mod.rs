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
//!
//! # Layout
//!
//! The struct, its construction and its accessors are here. The methods that make up
//! the API surface are split by concern into the private `install` submodule (install /
//! update / uninstall), `list` (queries) and `load` (`dlopen`), which build the same
//! `impl` block.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::marker::PhantomData;
use std::path::PathBuf;

use crate::cache::{now_unix, Index, IndexEntry, InstallSource};
use crate::config::{KitConfig, KitPaths};
use crate::error::{KitError, KitResult};
use crate::install::Installed;
use crate::lock::FileLock;
use crate::manifest::PluginManifest;
use crate::registry::Registry;

pub(super) mod install;
pub(super) mod list;
pub(super) mod load;

/// An overview of one installed plugin.
///
/// This is what the kit read from the manifest while listing the plugin directory. It
/// carries the fields the kit itself understands, plus [`PluginInfo::extra`] — every
/// other section, exactly as written — so a host can read its own sections from the
/// same read instead of opening the manifest a second time.
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

    /// Every top-level section the kit does not interpret, verbatim.
    ///
    /// A host's own sections live here: `[detect]`, `[context]`, and anything else it
    /// defines. The kit neither reads nor validates them — it only carries them through,
    /// which is what lets a host keep its manifest vocabulary while the kit stays
    /// host-agnostic.
    pub extra: toml::Table,
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

    // ---- internals -------------------------------------------------------

    /// Takes the install lock. Only `install`, `update` and `uninstall` need it, which
    /// is why it lives next to the record rather than on the public surface.
    pub(super) fn lock(&self) -> KitResult<FileLock> {
        FileLock::acquire(&self.paths.lock_file, self.cfg.lock_timeout)
    }

    /// Looks up the latest version when none was given.
    ///
    /// Shared with the `install` submodule, which is why it is not private.
    pub(super) fn resolve_version(
        &self,
        crate_name: &str,
        version: Option<&str>,
    ) -> KitResult<String> {
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

    /// Writes one install into the record cache.
    pub(super) fn record(&self, installed: &Installed) -> KitResult<()> {
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
    use std::path::Path;
    use std::time::Duration;

    /// A host ABI struct for tests.
    ///
    /// Its contents are arbitrary — this crate never reads a field of `T`, it only
    /// casts the symbol to `*const T`.
    #[repr(C)]
    pub(super) struct FakeEntry {
        abi_version: u32,
    }

    pub(super) const MANIFEST: &str = r#"
[plugin]
name    = "foo"
version = "0.1.0"
abi     = 1
family  = "node"

[detect]
strong = ["fake.lock"]
"#;

    pub(super) fn kit(root: &Path) -> CratePluginKit<FakeEntry> {
        let cfg = KitConfig::new("myapp")
            .with_data_dir(root)
            .with_lock_timeout(Duration::from_millis(500));
        CratePluginKit::new(cfg).expect("should build")
    }

    /// Lays out an "installed" plugin directory by hand, skipping the compile step.
    pub(super) fn place_plugin(root: &Path, crate_name: &str) -> PathBuf {
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
    fn config_is_exposed() {
        let tmp = tempfile::tempdir().unwrap();
        let k = kit(&tmp.path().join("store"));
        assert_eq!(k.config().id, "myapp");
        assert_eq!(k.config().crate_prefix, "myapp-plugin-");
    }
}
