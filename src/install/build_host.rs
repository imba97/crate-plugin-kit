//! build-host: generates a wrapper project, `cargo build`s a cdylib out of it, and
//! copies that into the install dir.
//!
//! # Why a wrapper is needed
//!
//! `cargo install` only knows about bin targets, so a `crate-type = ["cdylib"]` crate
//! cannot be installed that way. And `cargo build` on a plugin crate directly does
//! not make Cargo produce a cdylib for a dependency either.
//!
//! So this crate generates a wrapper project of a few dozen lines:
//!
//! ```text
//! <build>/<crate>/
//! ├── Cargo.toml        # [lib] crate-type = ["cdylib"], depends on the plugin and the host contract crate
//! └── src/lib.rs        # one line: <contract>::export!(<plugin_crate>::create);
//! ```
//!
//! The benefit is that the plugin's own crate stays a plain rlib: crates.io consumes
//! it normally, unit tests can `use` it directly, and the plugin author does not have
//! to write a single `#[no_mangle]`.
//!
//! # Where the manifest comes from
//!
//! The plugin crate's source root holds a copy of `<manifest_name>`. `cargo metadata`
//! is used here to locate the crate source dir, and the file is copied verbatim into
//! the install dir — see the module docs of [`crate::manifest`].

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cache::InstallSource;
use crate::config::{KitConfig, KitPaths};
use crate::error::{KitError, KitResult};
use crate::install::{remove_dir_if_exists, Installed};
use crate::loader;

/// Installs a plugin through build-host.
///
/// `version` must be a concrete version number (the caller looks up the latest one on
/// crates.io first).
pub fn install(
    cfg: &KitConfig,
    paths: &KitPaths,
    crate_name: &str,
    version: &str,
) -> KitResult<Installed> {
    let cargo = which::which("cargo").map_err(|_| KitError::CargoNotFound)?;

    let wrapper_dir = paths.build_dir(crate_name);
    remove_dir_if_exists(&wrapper_dir)?;
    std::fs::create_dir_all(wrapper_dir.join("src"))?;

    let stem = cfg.lib_stem(crate_name);

    // Ask the plugin crate what it requires of the contract crate, and have the wrapper ask for
    // exactly the same thing.
    //
    // Getting this wrong is not a cosmetic problem. If the two requirements resolve to
    // different versions, the graph holds two copies of the contract crate; each defines its
    // own `PackageManager`, and the `export!` in the wrapper stops typechecking with an E0308
    // that the plugin author has no way to see coming. Cargo will not unify them for us even
    // when one version satisfies both, because 0.0.x releases are incompatible across patches
    // by definition. Copying the plugin's own requirement makes the two agree by construction.
    let probe_dir = paths.build_dir(&format!("{crate_name}-probe"));
    let contract_req = probe_contract_requirement(cfg, &cargo, &probe_dir, crate_name, version)?;
    remove_dir_if_exists(&probe_dir)?;

    write_wrapper(
        cfg,
        &wrapper_dir,
        crate_name,
        version,
        &stem,
        contract_req.as_deref(),
    )?;

    let manifest_path = wrapper_dir.join("Cargo.toml");

    // 1. metadata: resolve and download dependencies, and pick up the target
    //    directory and the plugin source dir along the way
    let metadata = cargo_metadata(&cargo, &manifest_path)?;
    let target_dir = metadata.target_directory.clone();
    let plugin_src = metadata.plugin_source_dir(crate_name);

    // 2. the actual compile. stdio is inherited — this can take minutes and the user
    //    has to be able to watch it.
    let status = Command::new(&cargo)
        .arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .status()
        .map_err(KitError::Io)?;

    if !status.success() {
        return Err(KitError::BuildFailed {
            code: status.code(),
        });
    }

    // 3. find the artifact
    let release_dir = target_dir.join("release");
    let built =
        loader::find_library(&release_dir, &stem).map_err(|_| KitError::BuildArtifactMissing {
            stem: stem.clone(),
            dir: release_dir.display().to_string(),
        })?;

    // 4. land it: cdylib + manifest
    let plugin_dir = paths.plugin_dir(crate_name);
    remove_dir_if_exists(&plugin_dir)?;
    std::fs::create_dir_all(&plugin_dir)?;

    let dest_lib = plugin_dir.join(
        built
            .file_name()
            .expect("an artifact always has a file name"),
    );
    std::fs::copy(&built, &dest_lib)?;

    copy_manifest(cfg, &plugin_dir, plugin_src)?;

    Ok(Installed {
        crate_name: crate_name.to_string(),
        version: version.to_string(),
        dir: plugin_dir,
        library: dest_lib,
        source: InstallSource::BuildHost,
    })
}

// ---- wrapper generation ---------------------------------------------------

fn write_wrapper(
    cfg: &KitConfig,
    dir: &Path,
    crate_name: &str,
    version: &str,
    stem: &str,
    contract_req: Option<&str>,
) -> KitResult<()> {
    let crate_ident = crate_name.replace('-', "_");

    let mut toml = format!(
        r#"# Generated by crate-plugin-kit; do not edit by hand — every install rewrites it.
[package]
name         = "{crate_name}-wrapper"
version      = "0.0.0"
edition      = "{edition}"
publish      = false

# Single-element workspace: the wrapper lives under <data-dir>/build/ and must not be
# claimed by any Cargo.toml above it.
[workspace]

[lib]
name       = "{stem}"
crate-type = ["cdylib"]

[dependencies]
{crate_name} = "={version}"
{contract}   = "{contract_version}"
"#,
        crate_name = crate_name,
        edition = cfg.wrapper_edition,
        stem = stem,
        version = version,
        contract = cfg.contract_crate,
        contract_version = contract_req.unwrap_or(cfg.contract_version.as_str()),
    );

    // Local path overrides: during development, point the plugin and the contract
    // crate at a local checkout.
    if !cfg.local_overrides.is_empty() {
        toml.push_str(
            "\n# from KitConfig::local_overrides (development only)\n[patch.crates-io]\n",
        );
        for (name, path) in &cfg.local_overrides {
            let p = path.display().to_string().replace('\\', "/");
            toml.push_str(&format!("{name} = {{ path = \"{p}\" }}\n"));
        }
    }

    std::fs::write(dir.join("Cargo.toml"), toml)?;

    let body = cfg.wrapper_body.replace("{crate_ident}", &crate_ident);
    std::fs::write(dir.join("src").join("lib.rs"), body)?;

    // No `rust-toolchain.toml` is written for the wrapper.
    //
    // The wrapper lives under `<data-dir>/build/`, where no toolchain file exists
    // above it, so cargo uses the current default toolchain — which is what we want:
    // whatever toolchain you were already using. Pinning a channel would instead
    // trigger an unexpected rustup download during install.
    //
    // The host and the plugin do not need the same toolchain anyway; the only thing
    // crossing the boundary is `#[repr(C)]` data.

    Ok(())
}

/// Copies the manifest from the plugin source dir into the install dir.
fn copy_manifest(cfg: &KitConfig, plugin_dir: &Path, plugin_src: Option<&Path>) -> KitResult<()> {
    let Some(src_dir) = plugin_src else {
        return Err(KitError::ManifestMissingField {
            field: cfg.manifest_name.clone(),
            path: PathBuf::from("<plugin source dir not located>"),
        });
    };

    let src = src_dir.join(&cfg.manifest_name);
    if !src.is_file() {
        return Err(KitError::ManifestMissingField {
            field: cfg.manifest_name.clone(),
            path: src,
        });
    }

    std::fs::copy(&src, plugin_dir.join(&cfg.manifest_name))?;
    Ok(())
}

// ---- cargo metadata -------------------------------------------------------

/// The few values we want out of `cargo metadata`.
struct Metadata {
    target_directory: PathBuf,
    /// Crate name → source directory.
    packages: Vec<(String, PathBuf)>,
}

impl Metadata {
    fn plugin_source_dir(&self, crate_name: &str) -> Option<&Path> {
        self.packages
            .iter()
            .find(|(name, _)| name == crate_name)
            .map(|(_, dir)| dir.as_path())
    }
}

/// Run `cargo metadata` for a manifest and hand back the raw JSON.
fn cargo_metadata_json(cargo: &Path, manifest_path: &Path) -> KitResult<serde_json::Value> {
    let out = Command::new(cargo)
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--manifest-path")
        .arg(manifest_path)
        .output()
        .map_err(KitError::Io)?;

    if !out.status.success() {
        return Err(KitError::BuildFailed {
            code: out.status.code(),
        });
    }

    serde_json::from_slice(&out.stdout)
        .map_err(|e| KitError::Registry(format!("failed to parse cargo metadata output: {e}")))
}

/// The requirement `crate_name` itself declares on `contract`.
///
/// Taken from the plugin crate's own manifest, which is the only place that knows it.
fn declared_contract_requirement(
    v: &serde_json::Value,
    crate_name: &str,
    contract: &str,
) -> Option<String> {
    v.get("packages")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(|x| x.as_str()) == Some(crate_name))?
        .get("dependencies")?
        .as_array()?
        .iter()
        .find(|d| d.get("name").and_then(|x| x.as_str()) == Some(contract))?
        .get("req")?
        .as_str()
        .map(str::to_owned)
}

/// Ask cargo what the plugin crate requires of the contract crate.
///
/// Needs its own throwaway manifest, because the answer is needed *before* the wrapper's
/// manifest can be written -- and it cannot be read off the wrapper, since the wrapper is where
/// the question comes from in the first place.
fn probe_contract_requirement(
    cfg: &KitConfig,
    cargo: &Path,
    dir: &Path,
    crate_name: &str,
    version: &str,
) -> KitResult<Option<String>> {
    std::fs::create_dir_all(dir.join("src"))?;
    // Its own workspace, so the wrapper's Cargo.toml above it does not claim it.
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n\
             [workspace]\n\n[dependencies]\n{crate_name} = \"={version}\"\n"
        ),
    )?;
    std::fs::write(dir.join("src").join("lib.rs"), "")?;

    let v = cargo_metadata_json(cargo, &dir.join("Cargo.toml"))?;
    Ok(declared_contract_requirement(
        &v,
        crate_name,
        &cfg.contract_crate,
    ))
}

fn cargo_metadata(cargo: &Path, manifest_path: &Path) -> KitResult<Metadata> {
    let v = cargo_metadata_json(cargo, manifest_path)?;

    let target_directory = v
        .get("target_directory")
        .and_then(|x| x.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| KitError::Registry("cargo metadata has no target_directory".into()))?;

    let mut packages = Vec::new();
    if let Some(arr) = v.get("packages").and_then(|x| x.as_array()) {
        for p in arr {
            let (Some(name), Some(mp)) = (
                p.get("name").and_then(|x| x.as_str()),
                p.get("manifest_path").and_then(|x| x.as_str()),
            ) else {
                continue;
            };
            // manifest_path is .../<crate>/Cargo.toml, so the parent is the source dir
            if let Some(dir) = Path::new(mp).parent() {
                packages.push((name.to_string(), dir.to_path_buf()));
            }
        }
    }

    Ok(Metadata {
        target_directory,
        packages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn cfg() -> KitConfig {
        let mut c = KitConfig::new("myapp");
        c.contract_version = "0.1".to_string();
        c.wrapper_body = "myapp_plugin::export!({crate_ident}::create);\n".to_string();
        c.lock_timeout = Duration::from_millis(500);
        c
    }

    fn generate(c: &KitConfig) -> (tempfile::TempDir, PathBuf) {
        generate_with_contract(c, None)
    }

    fn generate_with_contract(
        c: &KitConfig,
        contract_req: Option<&str>,
    ) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("build").join("myapp-plugin-foo");
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let stem = c.lib_stem("myapp-plugin-foo");
        write_wrapper(c, &dir, "myapp-plugin-foo", "0.1.0", &stem, contract_req).unwrap();
        (tmp, dir)
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
        let (_tmp, dir) = generate_with_contract(&c, Some("=0.0.3"));
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

        assert!(toml.contains(r#"myapp-plugin   = "=0.0.3""#), "{toml}");
        assert!(
            !toml.contains(r#""0.1""#),
            "the fallback must not leak in: {toml}"
        );
    }

    fn metadata_with(dep_name: &str, req: &str) -> serde_json::Value {
        serde_json::json!({
            "packages": [
                { "name": "unrelated" },
                {
                    "name": "myapp-plugin-foo",
                    "dependencies": [
                        { "name": "serde", "req": "^1" },
                        { "name": dep_name, "req": req },
                    ],
                },
            ],
        })
    }

    #[test]
    fn reads_the_requirement_the_plugin_declares() {
        let v = metadata_with("myapp-plugin", "=0.0.3");
        assert_eq!(
            declared_contract_requirement(&v, "myapp-plugin-foo", "myapp-plugin").as_deref(),
            Some("=0.0.3")
        );
    }

    /// A plugin that does not depend on the contract crate at all leaves the caller on the
    /// configured fallback rather than inventing something.
    #[test]
    fn a_missing_requirement_is_none() {
        let v = metadata_with("something-else", "^2");
        assert_eq!(
            declared_contract_requirement(&v, "myapp-plugin-foo", "myapp-plugin"),
            None
        );
        assert_eq!(
            declared_contract_requirement(&v, "not-in-the-graph", "myapp-plugin"),
            None
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
        write_wrapper(&c, &dir, "myapp-plugin-a-b", "0.1.0", &stem, None).unwrap();

        let body = std::fs::read_to_string(dir.join("src").join("lib.rs")).unwrap();
        assert_eq!(body, "myapp_plugin::export!(myapp_plugin_a_b::create);\n");
    }

    #[test]
    fn writes_a_body_but_no_toolchain_pin() {
        let c = cfg();
        let (_tmp, dir) = generate(&c);

        assert!(dir.join("src").join("lib.rs").is_file());
        // No toolchain file is written for the wrapper — see the comment in `write_wrapper`
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

    #[test]
    fn plugin_source_dir_is_looked_up_by_name() {
        let meta = Metadata {
            target_directory: PathBuf::from("/target"),
            packages: vec![
                ("other".to_string(), PathBuf::from("/src/other")),
                (
                    "myapp-plugin-foo".to_string(),
                    PathBuf::from("/src/myapp-plugin-foo"),
                ),
            ],
        };

        assert_eq!(
            meta.plugin_source_dir("myapp-plugin-foo"),
            Some(Path::new("/src/myapp-plugin-foo"))
        );
        assert_eq!(meta.plugin_source_dir("absent"), None);
    }
}
