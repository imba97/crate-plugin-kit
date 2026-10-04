//! Opening a cdylib and resolving its entry symbol.
//!
//! The whole `dlopen` surface: a `Library` handle that is kept alive by the returned
//! [`LoadedPlugin`], the entry function looked up by name, and the platform-specific
//! "there is no such symbol" error turned into its own variant.
//!
//! All of it is unsafe by nature — the file being opened, and the layout of `T`, are
//! both assumptions this crate cannot check.

use std::path::{Path, PathBuf};

use libloading::Library;

use crate::error::{KitError, KitResult};

/// A loaded plugin.
///
/// Holds the `Library` handle — dropping this struct unloads the plugin. So keep it
/// alive for as long as anything exported by it is still in use.
pub struct LoadedPlugin<T> {
    /// Must be held. The field exists to keep the library alive until the struct is
    /// dropped.
    _lib: Library,
    entry: *const T,
    path: PathBuf,
}

impl<T> LoadedPlugin<T> {
    /// Pointer to the entry struct.
    ///
    /// # Safety
    ///
    /// The caller guarantees that the layout of `T` matches the struct exported by
    /// the plugin. This crate cannot verify that — it is the job of the host's
    /// `abi_version` field.
    pub fn entry(&self) -> *const T {
        self.entry
    }

    /// Dynamic library file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// `Send` / `Sync` are deliberately not implemented: `T` holds raw function pointers,
// and this crate has no way to know whether the code they point to is thread-safe.
// A host that needs to pass one across threads should say so explicitly with
// `unsafe impl`.

/// Opens a cdylib and calls its entry function.
///
/// # Safety
///
/// - `path` must point at a plugin installed by this kit (or a layout-equivalent
///   dynamic library);
/// - `T` must have the same layout as the struct that plugin exports.
///
/// # Errors
///
/// - [`KitError::LibraryLoad`]: `dlopen` failed.
/// - [`KitError::SymbolMissing`]: there is no exported symbol named `symbol`.
/// - [`KitError::NullEntry`]: the entry function returned a null pointer.
pub unsafe fn open<T>(path: &Path, symbol: &[u8]) -> KitResult<LoadedPlugin<T>> {
    let lib = Library::new(path).map_err(|source| KitError::LibraryLoad {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;

    let entry = {
        let f: libloading::Symbol<'_, unsafe extern "C" fn() -> *const T> =
            lib.get(symbol).map_err(|source| {
                if is_symbol_not_found(&source) {
                    // "The library does not have this symbol" is a common case (the
                    // wrong thing was installed, or a different plugin was), and
                    // reporting it separately reads much better than a vague "load
                    // failed".
                    KitError::SymbolMissing {
                        symbol: String::from_utf8_lossy(symbol).into_owned(),
                    }
                } else {
                    KitError::LibraryLoad {
                        path: path.to_path_buf(),
                        source: Box::new(source),
                    }
                }
            })?;
        f()
    };

    if entry.is_null() {
        return Err(KitError::NullEntry);
    }

    Ok(LoadedPlugin {
        _lib: lib,
        entry,
        path: path.to_path_buf(),
    })
}

/// The `libloading` errors that mean "the library does not have this symbol"; the
/// variant names differ per platform:
///
/// | Platform | Variants |
/// | ---- | ---- |
/// | Unix | `DlSym` / `DlSymUnknown` |
/// | Windows | `GetProcAddress` / `GetProcAddressUnknown` |
///
/// The variants only exist on their own platform, so the match is split by `cfg`.
fn is_symbol_not_found(e: &libloading::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(
            e,
            libloading::Error::DlSym { .. } | libloading::Error::DlSymUnknown
        )
    }

    #[cfg(windows)]
    {
        matches!(
            e,
            libloading::Error::GetProcAddress { .. } | libloading::Error::GetProcAddressUnknown
        )
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opening a file that is not a dynamic library at all must report
    /// `LibraryLoad`, not panic.
    #[test]
    fn opening_a_non_library_reports_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.bin");
        std::fs::write(&path, b"this is not a shared object").unwrap();

        // SAFETY: the point here is to make it fail; no layout assumption is involved.
        let got = unsafe { open::<u32>(&path, b"whatever") };
        assert!(
            matches!(got, Err(KitError::LibraryLoad { .. })),
            "expected LibraryLoad, got {:?}",
            got.err()
        );
    }
}
