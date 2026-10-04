//! The directories a kit instance works in, and the files inside them.
//!
//! [`KitPaths`] is resolved once, when the kit is built, from [`KitConfig::data_dir`] or
//! the default `~/.{id}`. Every path the crate touches afterwards hangs off it: the
//! install dirs under `plugins`, the build-host scaffolding under `build`, the install
//! lock and the record cache.

use std::path::PathBuf;

use crate::error::{KitError, KitResult};

use super::KitConfig;

impl KitPaths {
    /// Resolves the various paths. Creates directories as needed.
    pub(super) fn resolve(cfg: &KitConfig) -> KitResult<Self> {
        let root = match &cfg.data_dir {
            Some(d) => d.clone(),
            None => default_data_dir(&cfg.id)?,
        };
        Ok(Self {
            plugins: root.join("plugins"),
            build: root.join("build"),
            lock_file: root.join(".lock"),
            index_file: root.join("plugins").join(".plugins.json"),
            root,
        })
    }
}

/// Default data dir `~/.{id}`, as determined by `directories`.
fn default_data_dir(id: &str) -> KitResult<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or(KitError::NoDataDir)?;
    Ok(home.join(format!(".{id}")))
}

/// All paths the kit uses.
#[derive(Debug, Clone)]
pub struct KitPaths {
    /// `<data-dir>` itself.
    pub root: PathBuf,
    /// Install dir of installed plugins, `<root>/plugins`.
    pub plugins: PathBuf,
    /// build-host scaffolding directory, `<root>/build`.
    pub build: PathBuf,
    /// Install exclusive lock file, `<root>/.lock`.
    pub lock_file: PathBuf,
    /// Install record cache, `<root>/plugins/.plugins.json`.
    pub index_file: PathBuf,
}

impl KitPaths {
    /// Install dir of one plugin, `<plugins>/<crate_name>`.
    pub fn plugin_dir(&self, crate_name: &str) -> PathBuf {
        self.plugins.join(crate_name)
    }

    /// Manifest path of one plugin.
    pub fn manifest_path(&self, crate_name: &str, manifest_name: &str) -> PathBuf {
        self.plugin_dir(crate_name).join(manifest_name)
    }

    /// build-host scaffolding directory of one plugin.
    pub fn build_dir(&self, crate_name: &str) -> PathBuf {
        self.build.join(crate_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> KitConfig {
        KitConfig::new("myapp")
    }

    #[test]
    fn paths_honour_the_data_dir_override() {
        let root = PathBuf::from("some").join("dir");
        let c = cfg().with_data_dir(&root);
        let p = c.paths().expect("must not touch HOME when overridden");

        assert_eq!(p.root, root);
        assert_eq!(p.plugins, root.join("plugins"));
        assert_eq!(p.build, root.join("build"));
        assert_eq!(p.lock_file, root.join(".lock"));
        assert_eq!(p.index_file, root.join("plugins").join(".plugins.json"));
    }

    #[test]
    fn plugin_paths_sit_under_the_plugins_dir() {
        let root = PathBuf::from("some").join("dir");
        let c = cfg().with_data_dir(&root);
        let p = c.paths().unwrap();

        assert_eq!(
            p.plugin_dir("myapp-plugin-foo"),
            root.join("plugins").join("myapp-plugin-foo")
        );
        assert_eq!(
            p.build_dir("myapp-plugin-foo"),
            root.join("build").join("myapp-plugin-foo")
        );
        assert_eq!(
            p.manifest_path("myapp-plugin-foo", "myapp-plugin.toml"),
            root.join("plugins")
                .join("myapp-plugin-foo")
                .join("myapp-plugin.toml")
        );
    }
}
