//! End to end: install a plugin from a checkout, then list it, load it, update it, remove it.
//!
//! The counterpart of `pack.rs`: that one proves a checkout can be *packed* for release,
//! this one proves the same checkout can be installed into the store on this machine —
//! through the same generated wrapper, landing the same two files, so that everything
//! downstream (`list`, `load`, `uninstall`) cannot tell it apart from a published plugin.

use std::path::{Path, PathBuf};

use crate_plugin_kit::cache::InstallSource;
use crate_plugin_kit::{CratePluginKit, KitConfig};

/// The entry struct's shape. The kit never reads it; it only names the type of the pointer
/// `load` hands back, so any type will do.
struct Entry;

/// The `KitConfig` a host such as pmpx would use, with only the wrapper body changed: the
/// fixture's contract crate has no `export!` macro, so the body is written out here. It
/// still calls into the plugin crate, which is the point — it proves the wrapper resolves
/// the plugin through the path dependency it wrote.
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

/// The fixture checkout, copied out of the repository — the same reason `pack.rs` copies
/// it: a test must not write build output into the checkout.
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

/// A store rooted in a temporary directory, so nothing here can touch a real one.
fn kit(tmp: &Path) -> (CratePluginKit<Entry>, PathBuf) {
    let root = tmp.join("store");
    let kit = CratePluginKit::<Entry>::new(cfg().with_data_dir(&root)).expect("should build");
    (kit, root)
}

/// The whole loop, in one test: install from a directory, then use it as if it had come
/// from crates.io.
#[test]
fn a_checkout_installs_like_a_published_plugin() {
    let (tmp, checkout) = fixture();
    let (kit, _root) = kit(tmp.path());

    let installed = kit
        .install_from_path(&checkout)
        .expect("installing the fixture should succeed");

    assert_eq!(installed.crate_name, "toyapp-plugin-toy");
    assert_eq!(installed.version, "0.1.0");
    assert_eq!(
        installed.source,
        InstallSource::Local {
            path: checkout.canonicalize().unwrap()
        },
        "the directory is remembered, because it is what `update` rebuilds from"
    );

    // The two files a published install lands, landed: the cdylib under this platform's
    // naming, and the plugin's own manifest beside it.
    assert!(installed.library.is_file(), "{:?}", installed.library);
    assert!(installed.dir.join("toyapp-plugin.toml").is_file());

    // And the store sees it exactly like any other plugin -- host sections included.
    let listed = kit.list().expect("the store should list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].crate_name, "toyapp-plugin-toy");
    assert_eq!(listed[0].abi, Some(1));
    assert_eq!(listed[0].family.as_deref(), Some("test"));
    assert_eq!(
        listed[0].extra["detect"]["strong"][0].as_str(),
        Some("toy.lock"),
        "the host's own section survived the install"
    );

    // The loader finds it where the store put it: same naming rules, no special case.
    let loaded = kit
        .load("toyapp-plugin-toy")
        .expect("the installed plugin should load");
    assert_eq!(loaded.path(), installed.library);
}

/// Updating a plugin that came from a directory rebuilds that directory. The version
/// argument has nothing to resolve, so it is not consulted.
#[test]
fn update_rebuilds_from_the_recorded_directory() {
    let (tmp, checkout) = fixture();
    let (kit, root) = kit(tmp.path());

    kit.install_from_path(&checkout).expect("should install");
    assert!(root.join("plugins").join("toyapp-plugin-toy").is_dir());

    let updated = kit
        .update("toy", Some("9.9.9"))
        .expect("a local plugin updates by rebuilding");

    assert_eq!(updated.version, "0.1.0", "the checkout's own version");
    assert!(matches!(updated.source, InstallSource::Local { .. }));
    assert!(updated.library.is_file());
}

/// Removing one is the same call as for any other plugin -- nothing about a local install
/// needs a different way out.
#[test]
fn uninstall_removes_it() {
    let (tmp, checkout) = fixture();
    let (kit, _root) = kit(tmp.path());

    let installed = kit.install_from_path(&checkout).expect("should install");
    kit.uninstall("toy").expect("should uninstall");

    assert!(!installed.dir.exists(), "the directory should be gone");
    assert!(kit.list().expect("the store should list").is_empty());
}
