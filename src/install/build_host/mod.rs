//! build-host: generates a wrapper project, `cargo build`s a cdylib out of it, and
//! copies that into the install dir.
//!
//! # Why a wrapper is needed
//!
//! `cargo install` only knows about bin targets, so a `crate-type = ["cdylib"]` crate
//! cannot be installed that way. And `cargo build` on a plugin crate directly does
//! not make Cargo produce a cdylib for a dependency either.
//!
//! So a wrapper project of a few dozen lines is generated. The generator itself lives in
//! the private `wrapper` module, because [`crate::pack`] — the release-asset path — builds
//! the exact same wrapper from a local checkout:
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
//!
//! # Layout
//!
//! [`install`] is the whole flow. `metadata` holds the `cargo metadata` parsing, and
//! `probe_contract_requirement` below is the one step that has to run *before* the
//! wrapper exists.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cache::InstallSource;
use crate::config::{KitConfig, KitPaths};
use crate::error::{KitError, KitResult};
use crate::install::{remove_dir_if_exists, Installed};
use crate::loader;
use crate::wrapper::{self, cargo_metadata_json, declared_contract_dep, ContractDep, PluginSource};

mod metadata;

use metadata::cargo_metadata;

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

    // Ask the plugin crate what it declares about the contract crate, and have the wrapper ask
    // for exactly the same thing. Getting this wrong is not cosmetic: the private `wrapper`
    // module explains what two disagreeing requirements do to the build.
    let probe_dir = paths.build_dir(&format!("{crate_name}-probe"));
    let contract = probe_contract_requirement(cfg, &cargo, &probe_dir, crate_name, version)?;
    remove_dir_if_exists(&probe_dir)?;

    wrapper::write(
        cfg,
        &wrapper_dir,
        crate_name,
        &stem,
        PluginSource::Registry { version },
        contract.as_ref(),
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
//
// The generated files live in the private `wrapper` module, because `crate::pack` builds the
// same wrapper from a local checkout. What stays here is the part that is specific to
// installing from a registry: asking cargo what the *published* crate declares.

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

/// Ask cargo what the plugin crate declares about the contract crate.
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
) -> KitResult<Option<ContractDep>> {
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
    Ok(declared_contract_dep(&v, crate_name, &cfg.contract_crate))
}
