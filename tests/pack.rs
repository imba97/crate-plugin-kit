//! End to end: really pack a plugin checkout, and really load what came out.
//!
//! This is the release path's counterpart of `load.rs`. It covers the parts no unit test
//! can: that the generated wrapper compiles against a plugin that is only a **path**
//! dependency, that the cdylib really exports the entry symbol, and that both files come
//! out under the names `install::prebuilt` looks for.
//!
//! `pack` verifies the finished artifact by `dlopen`ing it, so a successful call here
//! already proves the symbol exists; the assertions below add the naming, the manifest
//! copy, and the guard rails.

use std::path::{Path, PathBuf};

use crate_plugin_kit::{pack_plugin, KitConfig, KitError, PackOptions};

/// The `KitConfig` a host such as pmpx would use, with only the wrapper body changed:
/// the fixture's contract crate has no `export!` macro, so the body is written out here.
/// It still calls into the plugin crate, which is the point — it proves the wrapper
/// resolves the plugin through the path dependency `pack` wrote.
fn cfg() -> KitConfig {
    let mut cfg = KitConfig::new("toyapp");
    cfg.wrapper_body = concat!(
        "pub fn entry_value() -> u32 { {crate_ident}::create() }\n",
        "\n",
        "#[unsafe(no_mangle)]\n",
        "pub extern \"C\" fn toyapp_plugin_entry_v1() -> u32 { entry_value() }\n",
    )
    .to_string();
    cfg
}

/// The fixture checkout, copied out of the repository.
///
/// Copied for the same reason `load.rs` copies its fixture, plus one specific to packing:
/// `pack` puts cargo's target directory next to the plugin by default, and a test must not
/// write build output into the checkout.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("should create a temp dir");
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let dst = tmp.path().join("fixtures");
    copy_tree(&src.join("toy-rlib-plugin"), &dst.join("toy-rlib-plugin"));
    copy_tree(&src.join("toy-contract"), &dst.join("toy-contract"));

    (tmp, dst.join("toy-rlib-plugin"))
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("should create the fixture directory");
    for entry in std::fs::read_dir(src).expect("should read the fixture directory") {
        let entry = entry.expect("should read a fixture entry");
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("should stat").is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).expect("should copy a fixture file");
        }
    }
}

/// Both assets land under the names the download path builds, for this machine's target.
#[test]
fn it_writes_the_two_assets_under_the_expected_names() {
    let (_tmp, plugin) = fixture();
    let out = plugin.parent().unwrap().join("dist");

    let assets = pack_plugin(&cfg(), &plugin, &out, &PackOptions::default())
        .expect("packing the fixture should succeed");

    let target = crate_plugin_kit::TARGET_TRIPLE;

    let base = format!("toyapp-plugin-toy-0.1.0-{target}");
    assert_eq!(assets.base_name(), base);
    assert_eq!(assets.crate_name, "toyapp-plugin-toy");
    assert_eq!(assets.version, "0.1.0");
    assert_eq!(assets.target, target);

    assert!(assets.library.is_file(), "{:?}", assets.library);
    assert!(assets.manifest.is_file(), "{:?}", assets.manifest);

    // The extension has to be the one this platform uses, since that is what prebuilt
    // appends to the URL it fetches.
    let ext = assets
        .library
        .extension()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let expected = match std::env::consts::OS {
        "windows" => "dll",
        "macos" => "dylib",
        _ => "so",
    };
    assert_eq!(ext, expected, "{:?}", assets.library);

    assert!(assets.library.metadata().unwrap().len() > 0);
}

/// The manifest that travels with the cdylib is the plugin's own file, byte for byte —
/// host-specific sections included.
#[test]
fn the_manifest_is_copied_verbatim() {
    let (_tmp, plugin) = fixture();
    let out = plugin.parent().unwrap().join("dist");

    let assets = pack_plugin(&cfg(), &plugin, &out, &PackOptions::default()).unwrap();

    let written = std::fs::read_to_string(&assets.manifest).unwrap();
    let original = std::fs::read_to_string(plugin.join("toyapp-plugin.toml")).unwrap();
    assert_eq!(written, original);
    assert!(written.contains("[detect]"), "{written}");
}

/// The artifact is a real cdylib exporting the symbol a host resolves.
#[test]
fn the_artifact_can_be_loaded_and_called() {
    let (_tmp, plugin) = fixture();
    let out = plugin.parent().unwrap().join("dist");

    let assets = pack_plugin(&cfg(), &plugin, &out, &PackOptions::default()).unwrap();

    // `pack` skips this check for a cross-compiled target; the test always builds for the
    // host, so the check really ran — and loading it again here shows why it matters.
    // SAFETY: this is a cdylib built from the fixture, with the signature below.
    let value = unsafe {
        let lib = libloading::Library::new(&assets.library).expect("should dlopen");
        let entry: libloading::Symbol<'_, unsafe extern "C" fn() -> u32> = lib
            .get(b"toyapp_plugin_entry_v1")
            .expect("the entry symbol should be exported");
        entry()
    };

    // The value comes from the plugin crate through the wrapper, so it proves the path
    // dependency was compiled in rather than an empty wrapper being produced.
    assert_eq!(value, 7);
}

/// A manifest that lags behind `Cargo.toml` would publish assets that cannot be consumed;
/// that is caught before anything is built.
#[test]
fn a_manifest_version_that_disagrees_is_refused() {
    let (_tmp, plugin) = fixture();
    std::fs::write(
        plugin.join("toyapp-plugin.toml"),
        "[plugin]\nname = \"toy\"\nversion = \"9.9.9\"\n",
    )
    .unwrap();
    let out = plugin.parent().unwrap().join("dist");

    let err = pack_plugin(&cfg(), &plugin, &out, &PackOptions::default()).unwrap_err();

    match err {
        KitError::VersionMismatch {
            declared,
            crate_version,
            ..
        } => {
            assert_eq!(declared, "9.9.9");
            assert_eq!(crate_version, "0.1.0");
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

/// A directory without a `Cargo.toml` is a usage mistake, and says so.
#[test]
fn a_missing_manifest_is_refused() {
    let tmp = tempfile::tempdir().unwrap();

    let err = pack_plugin(
        &cfg(),
        tmp.path(),
        tmp.path().join("dist"),
        &PackOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(err, KitError::NoManifest { .. }), "{err:?}");
}

/// The `--target-dir` override is the one CI depends on: build output goes to the
/// directory the cache action keeps, not into the checkout.
#[test]
fn the_target_dir_can_be_moved() {
    let (tmp, plugin) = fixture();
    let target_dir = tmp.path().join("build-cache");
    let out = tmp.path().join("dist");

    let assets = pack_plugin(
        &cfg(),
        &plugin,
        &out,
        &PackOptions {
            target_dir: Some(target_dir.clone()),
            ..PackOptions::default()
        },
    )
    .unwrap();

    assert!(assets.library.is_file());
    assert!(
        target_dir.join(crate_plugin_kit::TARGET_TRIPLE).is_dir(),
        "the build should have gone to the given directory"
    );
    assert!(
        !plugin.join("target").exists(),
        "nothing should have been written into the checkout"
    );
}

/// Two runs at once must not interfere.
///
/// This is a regression test, not a hypothetical: every run used to build in one directory
/// keyed by the crate name, so two packs of the same plugin deleted and rewrote each other's
/// files. On Windows that is not a flake but an outright failure — removing a directory
/// another process still has open is refused with "access denied" (os error 5).
#[test]
fn packing_the_same_plugin_twice_at_once_works() {
    let (tmp, plugin) = fixture();
    let first_out = tmp.path().join("dist-first");
    let second_out = tmp.path().join("dist-second");
    let first_cache = tmp.path().join("cache-first");
    let second_cache = tmp.path().join("cache-second");

    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            pack_plugin(
                &cfg(),
                &plugin,
                &first_out,
                &PackOptions {
                    target_dir: Some(first_cache.clone()),
                    ..PackOptions::default()
                },
            )
        });
        let second = scope.spawn(|| {
            pack_plugin(
                &cfg(),
                &plugin,
                &second_out,
                &PackOptions {
                    target_dir: Some(second_cache.clone()),
                    ..PackOptions::default()
                },
            )
        });

        (
            first
                .join()
                .expect("the first pack thread should not panic"),
            second
                .join()
                .expect("the second pack thread should not panic"),
        )
    });

    let first = first.expect("the first pack should succeed");
    let second = second.expect("the second pack should succeed");

    assert!(first.library.is_file(), "{:?}", first.library);
    assert!(second.library.is_file(), "{:?}", second.library);
    // Same content, different destinations: neither run disturbed the other.
    assert_ne!(first.library, second.library);
    assert_eq!(first.base_name(), second.base_name());
}
