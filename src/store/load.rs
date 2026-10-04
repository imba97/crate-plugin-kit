//! Loading an installed plugin with `dlopen`.
//!
//! `load` is the one call that makes [`CratePluginKit::uninstall`] refuse to delete a
//! plugin: once the library is open in this process, the crate name is recorded so the
//! two cannot disagree.

use crate::error::KitResult;
use crate::loader::{self, LoadedPlugin};

use super::CratePluginKit;

impl<T> CratePluginKit<T> {
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
}
