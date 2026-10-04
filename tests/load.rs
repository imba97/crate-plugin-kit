//! End to end: really compile a cdylib, really `dlopen` it, really call across the
//! boundary.
//!
//! This is the only test in the crate that proves the loading chain actually works —
//! every other unit test only verifies file IO and string derivation.
//!
//! Paths covered:
//!
//! ```text
//! cargo build --release   (the fixture is a zero-dependency cdylib)
//!   → copy into the plugin dir + write the manifest
//!   → CratePluginKit::list()      reads files only, no dlopen
//!   → CratePluginKit::load()      dlopen + resolve the symbol
//!   → read struct fields / call a function pointer    really crosses the boundary
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate_plugin_kit::{find_library, CratePluginKit, KitConfig};

/// Field-for-field counterpart of `ToyEntry` in
/// `tests/fixtures/toy-plugin/src/lib.rs`.
///
/// This is a miniature of the struct in a host contract crate; in a real project it is
/// defined by the contract crate and `abi_version` is compared strictly.
#[repr(C)]
struct ToyEntry {
    abi_version: u32,
    name_ptr: *const u8,
    name_len: usize,
    add: extern "C" fn(u32, u32) -> u32,
}

/// The `[lib]` section is deliberately omitted — so that the derivation path in
/// `KitConfig::lib_stem()` gets exercised too.
const MANIFEST: &str = r#"
[plugin]
name    = "toy"
version = "0.1.0"
abi     = 1
family  = "test"

# A host section: the kit does not know it and must not touch it
[detect]
strong = ["toy.lock"]
"#;

/// Copies the fixture into a temporary directory and compiles it, returning the
/// temporary directory (to keep it alive) and the path of the cdylib that was built.
///
/// It has to be copied out before compiling: the fixture lives under the repository's
/// `tests/fixtures/`, and compiling it in place would let cargo walk all the way up to
/// the repository root's `rust-toolchain.toml`, which in CI can trigger a needless
/// toolchain download. Below the temporary directory there is no such file.
fn build_fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("should create a temp dir");

    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("toy-plugin");
    let dst = tmp.path().join("toy-plugin");
    copy_tree(&src, &dst);

    let target_dir = tmp.path().join("target");

    // Call `cargo` directly: it is certainly on PATH (we are running under cargo test
    // right now), and it is a real .exe rather than the .cmd shim Windows uses.
    let status = Command::new("cargo")
        .args(["build", "--release", "--manifest-path"])
        .arg(dst.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .status()
        .expect("should start cargo");
    assert!(status.success(), "the fixture should compile");

    let lib = find_library(&target_dir.join("release"), "toyapp_plugin_toy")
        .expect("should find the fixture's cdylib among the artifacts");

    (tmp, lib)
}

/// Recursively copies a directory (only plain files and directories, which is enough
/// for the fixture).
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("should create the target dir");
    for entry in std::fs::read_dir(from).expect("should read the source dir") {
        let entry = entry.expect("directory entries should be readable");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("should get the type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("should copy the file");
        }
    }
}

/// A plugin store with the fixture installed, returning (temp dir to keep alive, kit).
fn store_with_fixture(lib: &Path) -> (tempfile::TempDir, CratePluginKit<ToyEntry>) {
    let tmp = tempfile::tempdir().expect("should create a temp dir");
    let root = tmp.path().join("store");

    let plugin_dir = root.join("plugins").join("toyapp-plugin-toy");
    std::fs::create_dir_all(&plugin_dir).expect("should create the plugin dir");

    // Manifest under the default name: `{id}-plugin.toml`
    std::fs::write(plugin_dir.join("toyapp-plugin.toml"), MANIFEST)
        .expect("should write the manifest");

    // The cdylib lands under its original file name; the loader builds its own
    // platform-specific candidates
    std::fs::copy(lib, plugin_dir.join(lib.file_name().unwrap())).expect("should copy the cdylib");

    let cfg = KitConfig::new("toyapp")
        .with_data_dir(&root)
        .with_lock_timeout(Duration::from_millis(500));

    let kit = CratePluginKit::<ToyEntry>::new(cfg).expect("should build the kit");
    (tmp, kit)
}

#[test]
fn loads_a_real_cdylib_and_calls_across_the_boundary() {
    let (_build_tmp, lib) = build_fixture();
    let (_store_tmp, kit) = store_with_fixture(&lib);

    // ---- list: files only, no dlopen -------------------------------------
    let all = kit.list().expect("list should succeed");
    assert_eq!(all.len(), 1, "exactly one plugin should be listed: {all:?}");
    assert_eq!(all[0].name, "toy", "the name comes from the manifest");
    assert_eq!(all[0].crate_name, "toyapp-plugin-toy");
    assert_eq!(all[0].version, "0.1.0");
    assert_eq!(all[0].family.as_deref(), Some("test"));

    // ---- manifest_of: the host's section must be preserved ---------------
    let manifest = kit
        .manifest_of("toy")
        .expect("the manifest should be readable");
    assert!(
        manifest.extra.contains_key("detect"),
        "the host's section must not be lost"
    );

    // ---- load: the real dlopen -------------------------------------------
    let loaded = kit.load("toy").expect("should load");
    let entry = loaded.entry();
    assert!(!entry.is_null(), "the entry pointer must not be null");
    assert_eq!(loaded.path().file_name(), lib.file_name());

    // SAFETY: `ToyEntry` is a field-for-field counterpart of the `#[repr(C)]`
    // definition in the fixture. In a real project an `abi_version` comparison would
    // come before this step (the contract crate's job).
    unsafe {
        assert_eq!((*entry).abi_version, 1, "the ABI version should be 1");

        let name = std::slice::from_raw_parts((*entry).name_ptr, (*entry).name_len);
        assert_eq!(name, b"toy", "the name read across the boundary");

        // A call through a cross-boundary function pointer — the hard evidence that
        // the dlopen chain works
        assert_eq!(((*entry).add)(2, 3), 5);
        assert_eq!(((*entry).add)(u32::MAX, 1), 0, "wrapping_add semantics");
    }
}

/// Once loaded, the plugin may not be deleted — on Windows a `.dll` that is currently
/// `dlopen`ed simply cannot be deleted, and on Linux deleting it only leaks the space.
/// Rather than gamble on it, refuse.
#[test]
fn uninstall_is_refused_after_the_plugin_was_loaded() {
    let (_build_tmp, lib) = build_fixture();
    let (_store_tmp, kit) = store_with_fixture(&lib);

    // It can be uninstalled normally before loading
    // (only "refused after loading" is under test here, so load it first)
    let _loaded = kit.load("toy").expect("should load");

    match kit.uninstall("toy") {
        Err(crate_plugin_kit::KitError::PluginInUse { name }) => {
            assert_eq!(name, "toyapp-plugin-toy");
        }
        other => panic!("expected PluginInUse, got {other:?}"),
    }

    // The directory must be untouched — a refusal really means nothing happened
    assert!(kit.paths().plugin_dir("toyapp-plugin-toy").is_dir());
}

/// When what got installed is a junk file, report "no exported symbol" rather than
/// panicking or passing silently.
#[test]
fn loading_a_non_plugin_reports_a_missing_symbol() {
    let tmp = tempfile::tempdir().expect("should create a temp dir");
    let root = tmp.path().join("store");

    let plugin_dir = root.join("plugins").join("toyapp-plugin-toy");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("toyapp-plugin.toml"), MANIFEST).unwrap();

    // Use a real dynamic library to impersonate a plugin: it loads, but it does not
    // have the symbol we want. This test's own binary is a dynamically linked
    // executable, which makes it the easiest material to use.
    let impostor = std::env::current_exe().expect("should get this test's path");
    let stem = "toyapp_plugin_toy";
    let name = match std::env::consts::OS {
        "windows" => format!("{stem}.dll"),
        "macos" => format!("lib{stem}.dylib"),
        _ => format!("lib{stem}.so"),
    };
    std::fs::copy(&impostor, plugin_dir.join(&name)).expect("should copy");

    let cfg = KitConfig::new("toyapp")
        .with_data_dir(&root)
        .with_lock_timeout(Duration::from_millis(500));
    let kit = CratePluginKit::<ToyEntry>::new(cfg).unwrap();

    // The outcome depends on the platform: either the file cannot be dlopen'ed at all
    // (an executable is not a shared library), or it loads but the symbol is not
    // found. Both are "correctly reported a failure", and neither is a panic.
    let err = match kit.load("toy") {
        Err(e) => e,
        // It cannot succeed: the symbol really does not exist. If it did succeed, the
        // validation in load would have a hole.
        Ok(_) => panic!("this file must not load successfully as a plugin"),
    };
    assert!(
        matches!(
            err,
            crate_plugin_kit::KitError::SymbolMissing { .. }
                | crate_plugin_kit::KitError::LibraryLoad { .. }
        ),
        "expected SymbolMissing or LibraryLoad, got {err:?}"
    );
}
