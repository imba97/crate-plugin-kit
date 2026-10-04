//! Generic `dlopen` loading.
//!
//! # No type erasure here
//!
//! `T` is the host's own `#[repr(C)]` entry struct (for bmux, that is
//! `BmuxPluginV1`). `load()` returns a `*const T` — a thin pointer, not a `dyn Trait`
//! — so there is no "convert a fat pointer between two different vtables" UB.
//!
//! This crate neither knows nor needs to know which fields `T` has; it only reads
//! the symbol address and casts it to `*const T`. Reading those fields safely is the
//! host contract crate's job.
//!
//! # What the caller has to guarantee
//!
//! 1. `T` and the struct the plugin actually exports have the same layout (backed by
//!    the host's `abi_version` field);
//! 2. every read of a field of `T` is wrapped in [`crate::panic::guard`].

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

/// The cdylib file name candidates for this platform, in priority order.
///
/// Other platforms' spellings are listed as well, so that when a wrong-platform
/// artifact sits in the install dir the error message can show what was tried
/// instead of only saying "not found".
pub fn library_candidates(stem: &str) -> Vec<String> {
    let mut out = Vec::new();

    match std::env::consts::OS {
        "windows" => out.push(format!("{stem}.dll")),
        "macos" => out.push(format!("lib{stem}.dylib")),
        _ => out.push(format!("lib{stem}.so")),
    }

    // Fallbacks: some build systems (or hand-made archives) omit the `lib` prefix
    // or use a different extension.
    for ext in ["so", "dylib", "dll"] {
        for name in [format!("lib{stem}.{ext}"), format!("{stem}.{ext}")] {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }

    out
}

/// Finds a cdylib in a directory by walking the candidate list, returning the first
/// one that exists.
pub fn find_library(dir: &Path, stem: &str) -> KitResult<PathBuf> {
    let candidates = library_candidates(stem);

    for name in &candidates {
        let p = dir.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }

    Err(KitError::LibraryNotFound {
        name: stem.to_string(),
        tried: candidates.join(", "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This platform's file name must come first in the candidate list, otherwise a
    /// wrong-platform artifact would win.
    #[test]
    fn the_current_platform_name_comes_first() {
        let c = library_candidates("myapp_plugin_foo");
        let first = c.first().expect("candidate list must not be empty");

        let expected = match std::env::consts::OS {
            "windows" => "myapp_plugin_foo.dll",
            "macos" => "libmyapp_plugin_foo.dylib",
            _ => "libmyapp_plugin_foo.so",
        };
        assert_eq!(first, expected);
    }

    /// The fallback entries must cover every platform spelling — that is what makes
    /// the "tried" list in the error meaningful when a wrong-platform artifact is
    /// sitting in the directory.
    #[test]
    fn candidates_cover_every_platform_spelling() {
        let c = library_candidates("stem");
        for name in [
            "stem.dll",
            "libstem.dylib",
            "libstem.so",
            "stem.so",
            "stem.dylib",
            "libstem.dll",
        ] {
            assert!(
                c.iter().any(|x| x == name),
                "candidate missing {name}: {c:?}"
            );
        }
    }

    #[test]
    fn candidates_have_no_duplicates() {
        let c = library_candidates("stem");
        let mut sorted = c.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), c.len(), "duplicate candidates: {c:?}");
    }

    #[test]
    fn find_library_picks_the_matching_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("unrelated.txt"), b"x").unwrap();

        let want = library_candidates("myapp_plugin_foo")[0].clone();
        std::fs::write(dir.path().join(&want), b"not really a library").unwrap();

        let found = find_library(dir.path(), "myapp_plugin_foo").expect("should be found");
        assert_eq!(found.file_name().unwrap().to_string_lossy(), want);
    }

    #[test]
    fn find_library_reports_what_it_tried() {
        let dir = tempfile::tempdir().unwrap();

        let err = find_library(dir.path(), "myapp_plugin_foo");
        match err {
            Err(KitError::LibraryNotFound { name, tried }) => {
                assert_eq!(name, "myapp_plugin_foo");
                assert!(tried.contains("myapp_plugin_foo"), "tried = {tried}");
            }
            other => panic!("expected LibraryNotFound, got {other:?}"),
        }
    }

    /// A directory with the right name does not count as a hit — only files do.
    #[test]
    fn find_library_ignores_a_directory_with_the_right_name() {
        let dir = tempfile::tempdir().unwrap();
        let want = &library_candidates("myapp_plugin_foo")[0];
        std::fs::create_dir(dir.path().join(want)).unwrap();

        assert!(find_library(dir.path(), "myapp_plugin_foo").is_err());
    }

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
