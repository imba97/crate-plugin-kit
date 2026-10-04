//! What a cdylib can be called on a given platform, and finding it by name.
//!
//! Nothing here opens a library: the extension and the candidate list are what both
//! naming an asset and looking one up are built from, so they have to agree even when
//! the target triple is not the one running.

use std::path::{Path, PathBuf};

use crate::error::{KitError, KitResult};

/// The cdylib extension implied by a target triple.
///
/// Derived from the triple rather than from the running host: the same function names
/// the assets a release publishes (`install::prebuilt` looks them up) and the artifact a
/// local build produced (`crate::pack`), and those two must agree even when the build
/// targets a triple other than the host's.
pub fn lib_ext_for_target(target: &str) -> &'static str {
    if target.contains("windows") {
        "dll"
    } else if target.contains("apple") || target.contains("darwin") {
        "dylib"
    } else {
        "so"
    }
}

/// The file name a cdylib has on the platform of `target`.
fn lib_file_name_for_target(stem: &str, target: &str) -> String {
    match lib_ext_for_target(target) {
        "dll" => format!("{stem}.dll"),
        ext => format!("lib{stem}.{ext}"),
    }
}

/// The cdylib file name candidates for `target`, in priority order.
///
/// Other platforms' spellings are listed as well, so that when a wrong-platform
/// artifact sits in the install dir the error message can show what was tried
/// instead of only saying "not found".
pub fn library_candidates_for_target(stem: &str, target: &str) -> Vec<String> {
    let mut out = vec![lib_file_name_for_target(stem, target)];

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

/// The cdylib file name candidates for this build's target, in priority order.
pub fn library_candidates(stem: &str) -> Vec<String> {
    library_candidates_for_target(stem, crate::TARGET_TRIPLE)
}

/// Finds a cdylib in a directory by walking the candidate list, returning the first
/// one that exists.
pub fn find_library(dir: &Path, stem: &str) -> KitResult<PathBuf> {
    find_library_for_target(dir, stem, crate::TARGET_TRIPLE)
}

/// [`find_library`], for a specific target triple.
pub fn find_library_for_target(dir: &Path, stem: &str, target: &str) -> KitResult<PathBuf> {
    let candidates = library_candidates_for_target(stem, target);

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

    /// The extension is a property of the target, not of whatever machine happens to be
    /// running: `pack` names assets for the triple it built, and `prebuilt` looks them up
    /// by the triple it was compiled for.
    #[test]
    fn the_extension_follows_the_target() {
        assert_eq!(lib_ext_for_target("x86_64-unknown-linux-gnu"), "so");
        assert_eq!(lib_ext_for_target("aarch64-unknown-linux-musl"), "so");
        assert_eq!(lib_ext_for_target("x86_64-pc-windows-msvc"), "dll");
        assert_eq!(lib_ext_for_target("aarch64-pc-windows-msvc"), "dll");
        assert_eq!(lib_ext_for_target("x86_64-apple-darwin"), "dylib");
        assert_eq!(lib_ext_for_target("aarch64-apple-darwin"), "dylib");
    }

    #[test]
    fn candidates_are_ordered_for_the_target_not_the_host() {
        // A Windows target on any host: the `.dll` spelling has to come first, otherwise
        // an artifact from a previous build of another platform could win.
        let c = library_candidates_for_target("stem", "x86_64-pc-windows-msvc");
        assert_eq!(c.first().map(String::as_str), Some("stem.dll"));

        let c = library_candidates_for_target("stem", "x86_64-apple-darwin");
        assert_eq!(c.first().map(String::as_str), Some("libstem.dylib"));

        let c = library_candidates_for_target("stem", "x86_64-unknown-linux-gnu");
        assert_eq!(c.first().map(String::as_str), Some("libstem.so"));
    }

    /// Every platform's spelling is still reachable, so a wrong-platform artifact in the
    /// directory produces a useful error rather than "not found".
    #[test]
    fn target_candidates_keep_every_fallback() {
        let c = library_candidates_for_target("stem", "x86_64-pc-windows-msvc");
        for name in ["stem.dll", "libstem.dylib", "libstem.so", "stem.so"] {
            assert!(c.iter().any(|x| x == name), "missing {name}: {c:?}");
        }
    }

    #[test]
    fn find_library_for_target_finds_another_platforms_artifact() {
        let dir = tempfile::tempdir().unwrap();
        // A Linux artifact, looked up for a Linux target: the `lib` prefix is what the
        // build actually produces there.
        std::fs::write(dir.path().join("libstem.so"), b"x").unwrap();

        let found = find_library_for_target(dir.path(), "stem", "x86_64-unknown-linux-gnu")
            .expect("should be found");
        assert_eq!(found.file_name().unwrap().to_string_lossy(), "libstem.so");
    }
}
