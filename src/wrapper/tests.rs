//! Tests for the wrapper that [`super::write`] generates.
//!
//! They are a sibling file rather than the bottom of `mod.rs` only because that file is
//! over the 300-line limit this repository keeps; they test the same thing they always did.

use super::*;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

pub(super) fn cfg() -> KitConfig {
    let mut c = KitConfig::new("myapp");
    c.contract_version = "0.1".to_string();
    c.wrapper_body = "myapp_plugin::export!({crate_ident}::create);\n".to_string();
    c.lock_timeout = Duration::from_millis(500);
    c
}

pub(super) fn write_into(
    c: &KitConfig,
    plugin: PluginSource<'_>,
    contract: Option<&ContractDep>,
) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("wrapper");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let stem = c.lib_stem("myapp-plugin-foo");
    write(c, &dir, "myapp-plugin-foo", &stem, plugin, contract).unwrap();
    tmp
}

pub(super) fn toml_of(tmp: &tempfile::TempDir) -> String {
    std::fs::read_to_string(tmp.path().join("wrapper").join("Cargo.toml")).unwrap()
}

/// The wrapper as `install::build_host` generates it: the plugin comes from the
/// registry, pinned to one version.
fn generate(c: &KitConfig) -> (tempfile::TempDir, PathBuf) {
    generate_with_contract(c, None)
}

fn generate_with_contract(
    c: &KitConfig,
    contract: Option<&ContractDep>,
) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("build").join("myapp-plugin-foo");
    std::fs::create_dir_all(dir.join("src")).unwrap();

    let stem = c.lib_stem("myapp-plugin-foo");
    write(
        c,
        &dir,
        "myapp-plugin-foo",
        &stem,
        PluginSource::Registry { version: "0.1.0" },
        contract,
    )
    .unwrap();
    (tmp, dir)
}

/// The packing path: the plugin is a checkout, and the wrapper still calls
/// `export!` — that is the one thing that makes the cdylib loadable.
#[test]
fn a_local_plugin_becomes_a_path_dependency_and_still_exports() {
    let c = cfg();
    let tmp = write_into(
        &c,
        PluginSource::Path {
            dir: Path::new("/local/plugin-foo"),
        },
        None,
    );

    let toml = toml_of(&tmp);
    assert!(
        toml.contains(r#"myapp-plugin-foo = { path = "/local/plugin-foo" }"#),
        "{toml}"
    );

    let body =
        std::fs::read_to_string(tmp.path().join("wrapper").join("src").join("lib.rs")).unwrap();
    assert_eq!(body, "myapp_plugin::export!(myapp_plugin_foo::create);\n");
}

#[test]
fn wrapper_manifest_names_the_right_library() {
    let c = cfg();
    let (_tmp, dir) = generate(&c);
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

    assert!(
        toml.contains(r#"name       = "myapp_plugin_foo""#),
        "{toml}"
    );
    assert!(toml.contains(r#"crate-type = ["cdylib"]"#), "{toml}");
}

#[test]
fn wrapper_is_not_publishable() {
    let c = cfg();
    let (_tmp, dir) = generate(&c);
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

    assert!(toml.contains("publish      = false"), "{toml}");
}

/// The wrapper is a single-element workspace — it lives under `<data-dir>/build/`
/// and must not be claimed by any Cargo.toml above it.
#[test]
fn wrapper_is_its_own_workspace() {
    let c = cfg();
    let (_tmp, dir) = generate(&c);
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

    assert!(toml.contains("[workspace]"), "{toml}");
}

/// The version has to be pinned exactly, otherwise the version recorded in
/// `.plugins.json` may not match the cdylib that was built.
#[test]
fn the_plugin_version_is_pinned_exactly() {
    let c = cfg();
    let (_tmp, dir) = generate(&c);
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

    assert!(toml.contains(r#"myapp-plugin-foo = "=0.1.0""#), "{toml}");
}

#[test]
fn contract_crate_is_a_dependency() {
    let c = cfg();
    let (_tmp, dir) = generate(&c);
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

    assert!(toml.contains("myapp-plugin"), "{toml}");
    assert!(toml.contains(r#""0.1""#), "{toml}");
}

/// When the plugin's own manifest declares a requirement, the wrapper has to repeat it
/// verbatim. Anything else and cargo may resolve two versions of the contract crate.
#[test]
fn the_plugins_own_requirement_wins() {
    let c = cfg();
    let contract = ContractDep {
        req: Some("=0.0.3".to_string()),
        path: None,
    };
    let (_tmp, dir) = generate_with_contract(&c, Some(&contract));
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

    assert!(toml.contains(r#"myapp-plugin   = "=0.0.3""#), "{toml}");
    assert!(
        !toml.contains(r#""0.1""#),
        "the fallback must not leak in: {toml}"
    );
}

#[test]
fn body_substitutes_the_crate_ident() {
    let c = cfg();
    let (_tmp, dir) = generate(&c);
    let body = std::fs::read_to_string(dir.join("src").join("lib.rs")).unwrap();

    // Hyphens have to become underscores, otherwise it is not a valid Rust path
    assert_eq!(body, "myapp_plugin::export!(myapp_plugin_foo::create);\n");
    assert!(
        !body.contains("{crate_ident}"),
        "placeholder not substituted: {body}"
    );
}

#[test]
fn body_substitution_handles_multi_hyphen_names() {
    let c = cfg();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("w");
    std::fs::create_dir_all(dir.join("src")).unwrap();

    let stem = c.lib_stem("myapp-plugin-a-b");
    write(
        &c,
        &dir,
        "myapp-plugin-a-b",
        &stem,
        PluginSource::Registry { version: "0.1.0" },
        None,
    )
    .unwrap();

    let body = std::fs::read_to_string(dir.join("src").join("lib.rs")).unwrap();
    assert_eq!(body, "myapp_plugin::export!(myapp_plugin_a_b::create);\n");
}

#[test]
fn writes_a_body_but_no_toolchain_pin() {
    let c = cfg();
    let (_tmp, dir) = generate(&c);

    assert!(dir.join("src").join("lib.rs").is_file());
    // No toolchain file is written for the wrapper — see the comment in `write`
    assert!(
        !dir.join("rust-toolchain.toml").exists(),
        "the wrapper must not pin a toolchain"
    );
}

#[test]
fn no_patch_section_without_local_overrides() {
    let c = cfg();
    let (_tmp, dir) = generate(&c);
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

    assert!(!toml.contains("[patch.crates-io]"), "{toml}");
}

/// Local development: paths the host gives explicitly go into the wrapper's
/// `[patch.crates-io]`. The wrapper cannot see the host project's
/// `.cargo/config.toml`, so this is the only way to pass them.
#[test]
fn local_overrides_become_a_patch_section() {
    let mut c = cfg();
    c.local_overrides = BTreeMap::from([
        (
            "myapp-plugin-foo".to_string(),
            PathBuf::from("/local/plugin-foo"),
        ),
        ("myapp-plugin".to_string(), PathBuf::from("/local/contract")),
    ]);

    let (_tmp, dir) = generate(&c);
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

    assert!(toml.contains("[patch.crates-io]"), "{toml}");
    assert!(
        toml.contains(r#"myapp-plugin-foo = { path = "/local/plugin-foo" }"#),
        "{toml}"
    );
    assert!(
        toml.contains(r#"myapp-plugin = { path = "/local/contract" }"#),
        "{toml}"
    );
}

/// Windows paths have to be written with forward slashes, otherwise the
/// backslashes in the TOML are read as escapes.
#[test]
fn local_override_paths_use_forward_slashes() {
    let mut c = cfg();
    c.local_overrides = BTreeMap::from([(
        "myapp-plugin-foo".to_string(),
        PathBuf::from(r"D:\local\plugin-foo"),
    )]);

    let (_tmp, dir) = generate(&c);
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

    assert!(toml.contains(r#"path = "D:/local/plugin-foo""#), "{toml}");
    // Not a single backslash should be left (TOML would read it as an escape)
    let patch_line = toml
        .lines()
        .find(|l| l.starts_with("myapp-plugin-foo"))
        .unwrap();
    assert!(!patch_line.contains('\\'), "{patch_line}");
}
