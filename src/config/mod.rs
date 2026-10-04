//! [`KitConfig`]: parametrizes every host-application-specific name used by a kit
//! instance.
//!
//! No host application name is hardcoded anywhere in this crate. Everything like
//! `bmux` / `bmux-plugin.toml` / `bmux_plugin_entry_v1` is supplied here.
//!
//! # Layout
//!
//! Naming lives here, because every name is derived from the same few fields and they
//! have to stay consistent. The paths those names are resolved against — the data dir
//! and the files inside it — are in `paths`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::KitResult;

mod paths;

pub use paths::KitPaths;

/// Default lock wait time.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// Every configurable item of a kit instance.
#[derive(Debug, Clone)]
pub struct KitConfig {
    /// Application id. Determines the data dir `~/.{id}`, and is part of the default
    /// HTTP User-Agent.
    pub id: String,

    /// File name of the plugin manifest, such as `"bmux-plugin.toml"`.
    ///
    /// The `[plugin]` and `[lib]` sections of the manifest are defined by this crate
    /// (see [`crate::manifest`]). A host may append its own sections to the same file
    /// (for example bmux's `[detect]`); this crate ignores them.
    pub manifest_name: String,

    /// Plugin crate name prefix, such as `"bmux-plugin-"`.
    ///
    /// `install("cargo")` first expands to `"bmux-plugin-cargo"`; a name that already
    /// carries the prefix is used as-is.
    pub crate_prefix: String,

    /// Entry symbol exported by the cdylib, such as `b"bmux_plugin_entry_v1"`.
    pub entry_symbol: Vec<u8>,

    /// Prefix of the cdylib file name stem, such as `"bmux_plugin_"`.
    ///
    /// Concatenated with the crate name minus its prefix, then given the platform
    /// extension, this yields the actual file name.
    /// Example: `bmux-plugin-cargo` → `bmux_plugin_` + `cargo` → `libbmux_plugin_cargo.so`.
    pub lib_stem_prefix: String,

    /// Name of the host's contract crate, such as `"bmux-plugin"`. The generated
    /// wrapper project depends on it.
    pub contract_crate: String,

    /// Fallback version requirement for the contract crate, such as `"0.1"`.
    ///
    /// Normally unused. The wrapper repeats whatever the plugin crate's own manifest requires
    /// of the contract crate, which is the only way to guarantee both resolve to one version:
    /// cargo treats differently-versioned copies as distinct crates, each with its own traits,
    /// and will not merge them even when a single version satisfies both requirements. This
    /// value is only consulted when the plugin's manifest declares no such dependency.
    pub contract_version: String,

    /// Edition used by the generated wrapper project.
    pub wrapper_edition: String,

    /// Template for the wrapper project's `src/lib.rs`.
    ///
    /// `{crate_ident}` is replaced with the plugin crate's ident (`-` becomes `_`).
    /// Example (bmux): `"bmux_plugin::export!({crate_ident}::create);\n"`
    pub wrapper_body: String,

    /// Data dir override. `None` = use `~/.{id}`.
    pub data_dir: Option<PathBuf>,

    /// Timeout for waiting on the install lock.
    pub lock_timeout: Duration,

    /// Whether to try prebuilt artifacts first when installing.
    pub prefer_prebuilt: bool,

    /// Override for the target triple. `None` = use the triple injected at compile
    /// time (see `build.rs`).
    pub target_triple: Option<String>,

    /// crates.io index address. May point at a mirror.
    pub registry: String,

    /// Local path overrides: crate name → local directory.
    ///
    /// Written into the generated wrapper project as its `[patch.crates-io]`. For
    /// development use: the wrapper lives under `~/.{id}/build/` and cannot see the
    /// host project's `.cargo/config.toml`, so local overrides must be passed
    /// explicitly here. Leave empty in production.
    pub local_overrides: BTreeMap<String, PathBuf>,
}

impl KitConfig {
    /// Starts a config from the minimum amount of information, with conservative
    /// defaults for the rest.
    ///
    /// All defaults are derived from `id` and are mutually consistent — for example
    /// `id = "myapp"` yields:
    ///
    /// | Field | Value |
    /// | ---- | -- |
    /// | `manifest_name` | `myapp-plugin.toml` |
    /// | `crate_prefix` | `myapp-plugin-` |
    /// | `lib_stem_prefix` | `myapp_plugin_` |
    /// | `entry_symbol` | `myapp_plugin_entry_v1` |
    /// | `contract_crate` | `myapp-plugin` |
    ///
    /// `wrapper_body` still has to be filled in by hand when the contract crate's path differs
    /// from the derivation above. `contract_version` does not: it is a fallback, and the wrapper
    /// normally repeats whatever the plugin crate's own manifest requires.
    ///
    /// `entry_symbol` does not need a trailing NUL — `libloading` adds it.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        let snake = id.replace('-', "_");
        Self {
            manifest_name: format!("{id}-plugin.toml"),
            crate_prefix: format!("{id}-plugin-"),
            entry_symbol: format!("{snake}_plugin_entry_v1").into_bytes(),
            lib_stem_prefix: format!("{snake}_plugin_"),
            contract_crate: format!("{id}-plugin"),
            contract_version: "0.1".to_string(),
            wrapper_edition: "2021".to_string(),
            wrapper_body: format!("{snake}_plugin::export!({{crate_ident}}::create);\n"),
            data_dir: None,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            prefer_prebuilt: true,
            target_triple: None,
            registry: "https://crates.io".to_string(),
            local_overrides: BTreeMap::new(),
            id,
        }
    }

    /// Overrides the data dir.
    pub fn with_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(dir.into());
        self
    }

    /// Overrides the lock wait time.
    pub fn with_lock_timeout(mut self, t: Duration) -> Self {
        self.lock_timeout = t;
        self
    }

    /// Target triple of this build. The configured override wins.
    pub fn effective_target(&self) -> &str {
        self.target_triple
            .as_deref()
            .unwrap_or(crate::TARGET_TRIPLE)
    }

    /// Normalizes the name passed to `install()` into a full crate name.
    ///
    /// A name that already carries the prefix is returned as-is; otherwise the
    /// prefix is prepended.
    pub fn normalize_crate_name(&self, name: &str) -> String {
        if name.starts_with(&self.crate_prefix) {
            name.to_string()
        } else {
            format!("{}{}", self.crate_prefix, name)
        }
    }

    /// Derives the cdylib file name stem (without the `lib` prefix and extension)
    /// from a crate name.
    ///
    /// `bmux-plugin-cargo` → `bmux_plugin_cargo`
    pub fn lib_stem(&self, crate_name: &str) -> String {
        let tail = crate_name
            .strip_prefix(&self.crate_prefix)
            .unwrap_or(crate_name);
        format!("{}{}", self.lib_stem_prefix, tail.replace('-', "_"))
    }

    /// Resolves the various paths. Creates directories as needed.
    pub fn paths(&self) -> KitResult<KitPaths> {
        KitPaths::resolve(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> KitConfig {
        KitConfig::new("myapp")
    }

    #[test]
    fn new_derives_names_from_the_id() {
        let c = cfg();
        assert_eq!(c.id, "myapp");
        assert_eq!(c.manifest_name, "myapp-plugin.toml");
        assert_eq!(c.crate_prefix, "myapp-plugin-");
        assert_eq!(c.lib_stem_prefix, "myapp_plugin_");
        assert_eq!(c.contract_crate, "myapp-plugin");
    }

    #[test]
    fn normalizes_short_and_full_crate_names() {
        let c = cfg();
        assert_eq!(c.normalize_crate_name("foo"), "myapp-plugin-foo");
        // A name that already carries the prefix is returned as-is, otherwise it
        // would become myapp-plugin-myapp-plugin-foo
        assert_eq!(
            c.normalize_crate_name("myapp-plugin-foo"),
            "myapp-plugin-foo"
        );
    }

    #[test]
    fn derives_lib_stem_from_crate_name() {
        let c = cfg();
        assert_eq!(c.lib_stem("myapp-plugin-foo"), "myapp_plugin_foo");
        // Hyphens become underscores
        assert_eq!(c.lib_stem("myapp-plugin-a-b"), "myapp_plugin_a_b");
    }

    #[test]
    fn target_falls_back_to_compile_time_triple() {
        let c = cfg();
        assert_eq!(c.effective_target(), crate::TARGET_TRIPLE);

        let mut overridden = cfg();
        overridden.target_triple = Some("aarch64-apple-darwin".to_string());
        assert_eq!(overridden.effective_target(), "aarch64-apple-darwin");
    }

    #[test]
    fn with_lock_timeout_replaces_the_default() {
        let c = cfg().with_lock_timeout(Duration::from_millis(5));
        assert_eq!(c.lock_timeout, Duration::from_millis(5));
    }
}
