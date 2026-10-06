//! Installing, updating and uninstalling a plugin.
//!
//! These are the only calls that take the install lock and touch the install directory:
//! `install` refuses a crate that is already present, `update` replaces it either way,
//! and `uninstall` deletes it — but not while this process has its library loaded, since
//! a `dlopen`ed library cannot always be removed.

use std::path::Path;

use crate::cache::{Index, InstallSource};
use crate::error::{KitError, KitResult};
use crate::install::{self, build_host, prebuilt, Installed};
use crate::manifest::PluginManifest;

use super::CratePluginKit;

impl<T> CratePluginKit<T> {
    /// Installs a plugin.
    ///
    /// `name` may be a short name (`"cargo"`, to which [`crate::KitConfig::crate_prefix`]
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

    /// Installs a plugin from a checkout on this machine.
    ///
    /// The checkout is an ordinary plugin crate: a plain rlib with its manifest beside its
    /// `Cargo.toml`. It is built exactly the way a published one is — through the same
    /// generated wrapper, landing the same two files — so nothing downstream (loading,
    /// listing, uninstalling) can tell the two apart. What differs is only where the source
    /// comes from.
    ///
    /// This is the development loop: build and install what you are working on without
    /// publishing it first. [`Self::update`] on such a plugin rebuilds from the same
    /// directory, and the prebuilt download is skipped (there is no release to download).
    ///
    /// # Errors
    ///
    /// - [`KitError::NoManifest`]: the directory has no `Cargo.toml`.
    /// - [`KitError::ManifestRead`]: it has no readable `<id>-plugin.toml` beside it.
    /// - [`KitError::AlreadyInstalled`]: that plugin is already installed. Use
    ///   [`Self::update`] to replace it.
    pub fn install_from_path(&self, plugin_dir: impl AsRef<Path>) -> KitResult<Installed> {
        let dir = plugin_dir.as_ref().canonicalize()?;

        let _lock = self.lock()?;

        // Checked before the build rather than after it: a checkout that cannot be installed
        // should say so in a second, not after a `cargo build`.
        if !dir.join("Cargo.toml").is_file() {
            return Err(KitError::NoManifest { dir });
        }
        let manifest = PluginManifest::read(&dir.join(&self.cfg.manifest_name))?;

        let crate_name = self.cfg.normalize_crate_name(&manifest.plugin.name);
        let dir_installed = self.paths.plugin_dir(&crate_name);
        if dir_installed.is_dir() {
            return Err(KitError::AlreadyInstalled {
                name: crate_name,
                path: dir_installed,
            });
        }

        let installed = build_host::install_from_path(
            self.config(),
            self.paths(),
            &crate_name,
            &manifest.plugin.version,
            &dir,
        )?;
        self.record(&installed)?;
        Ok(installed)
    }

    /// Updates (or reinstalls) a plugin. Replaces it if present, installs it if not.
    ///
    /// A plugin that was installed from a checkout is rebuilt from that checkout: the
    /// registry has nothing to say about a version that was never published, and rebuilding
    /// the directory is what updating it means.
    pub fn update(&self, name: &str, version: Option<&str>) -> KitResult<Installed> {
        let crate_name = self.cfg.normalize_crate_name(name);

        let _lock = self.lock()?;

        // The recorded source decides, not the argument: a `version` has no meaning for a
        // checkout.
        let recorded = Index::load(&self.paths.index_file)
            .get(&crate_name)
            .map(|entry| entry.source.clone());
        if let Some(InstallSource::Local { path }) = recorded {
            let manifest = PluginManifest::read(&path.join(&self.cfg.manifest_name))?;
            let installed = build_host::install_from_path(
                self.config(),
                self.paths(),
                &crate_name,
                &manifest.plugin.version,
                &path,
            )?;
            self.record(&installed)?;
            return Ok(installed);
        }

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

    /// The actual install. The caller must already hold the lock.
    fn install_locked(&self, crate_name: &str, version: &str) -> KitResult<Installed> {
        // Clear the old directory first (the update path)
        install::remove_dir_if_exists(&self.paths.plugin_dir(crate_name))?;

        if self.cfg.prefer_prebuilt {
            match prebuilt::try_install(
                self.config(),
                self.paths(),
                self.registry(),
                crate_name,
                version,
            ) {
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

        build_host::install(self.config(), self.paths(), crate_name, version)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{kit, place_plugin};
    use super::*;

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

    /// A directory that is not a plugin crate is refused before anything is built.
    #[test]
    fn install_from_path_needs_a_cargo_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        let checkout = tmp.path().join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();

        match k.install_from_path(&checkout) {
            Err(KitError::NoManifest { dir }) => {
                assert!(dir.ends_with("checkout"), "{dir:?}")
            }
            other => panic!("expected NoManifest, got {other:?}"),
        }
    }

    /// A crate without the manifest beside it is refused too — and again before the
    /// compile, which is the point of checking up front.
    #[test]
    fn install_from_path_needs_the_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        let checkout = tmp.path().join("checkout");
        std::fs::create_dir_all(checkout.join("src")).unwrap();
        std::fs::write(
            checkout.join("Cargo.toml"),
            "[package]\nname = \"myapp-plugin-foo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        assert!(
            k.install_from_path(&checkout).is_err(),
            "a crate with no manifest is not a plugin checkout"
        );
    }

    /// The same refusal an install from crates.io gives: one plugin name, one directory.
    #[test]
    fn install_from_path_refuses_when_already_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);
        place_plugin(&root, "myapp-plugin-foo");

        let checkout = tmp.path().join("checkout");
        std::fs::create_dir_all(checkout.join("src")).unwrap();
        std::fs::write(
            checkout.join("Cargo.toml"),
            "[package]\nname = \"myapp-plugin-foo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            checkout.join("myapp-plugin.toml"),
            super::super::tests::MANIFEST,
        )
        .unwrap();

        match k.install_from_path(&checkout) {
            Err(KitError::AlreadyInstalled { name, .. }) => {
                assert_eq!(name, "myapp-plugin-foo")
            }
            other => panic!("expected AlreadyInstalled, got {other:?}"),
        }
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
}
