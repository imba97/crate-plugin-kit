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
//! ├── src/lib.rs        # one line: <contract>::export!(<plugin_crate>::create);
//! └── target/           # cargo's build cache, kept between installs -- see `reset_wrapper`
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
//!
//! The two generated files are rewritten on every install; the target directory beside
//! them is not. That asymmetry is deliberate and load-bearing — [`reset_wrapper`] says why.

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

/// Where the plugin crate being installed comes from.
///
/// The two shapes differ in exactly three places — the dependency the probe asks cargo
/// about, the source the wrapper is written for, and what the install record says
/// afterwards. Everything else (the wrapper, the build, finding the artifact, landing the
/// cdylib and the manifest) is [`install_with`], so an install from a directory cannot
/// drift from an install from crates.io.
enum Spec<'a> {
    /// A published crate, pinned exactly: the version recorded must be the version built.
    Registry {
        crate_name: &'a str,
        version: &'a str,
    },
    /// A checkout on this machine, with its manifest beside its `Cargo.toml`.
    Local {
        crate_name: &'a str,
        version: &'a str,
        dir: &'a Path,
    },
}

impl Spec<'_> {
    fn crate_name(&self) -> &str {
        match self {
            Spec::Registry { crate_name, .. } | Spec::Local { crate_name, .. } => crate_name,
        }
    }

    fn version(&self) -> &str {
        match self {
            Spec::Registry { version, .. } | Spec::Local { version, .. } => version,
        }
    }

    /// The dependency line the probe manifest needs to reach the plugin crate.
    ///
    /// A literal TOML string for the path: a Windows path is full of backslashes, and a
    /// basic string would read them as escapes.
    fn probe_dependency(&self) -> String {
        match self {
            Spec::Registry {
                crate_name,
                version,
            } => format!("{crate_name} = \"={version}\""),
            Spec::Local {
                crate_name, dir, ..
            } => {
                format!("{crate_name} = {{ path = '{}' }}", dir.display())
            }
        }
    }

    fn plugin_source(&self) -> PluginSource<'_> {
        match self {
            Spec::Registry { version, .. } => PluginSource::Registry { version },
            Spec::Local { dir, .. } => PluginSource::Path { dir },
        }
    }

    /// How the install record describes it. A published crate built on this machine is
    /// `BuildHost`; a checkout is `Local`, with the directory kept for `update`.
    fn install_source(&self) -> InstallSource {
        match self {
            Spec::Registry { .. } => InstallSource::BuildHost,
            Spec::Local { dir, .. } => InstallSource::Local {
                path: dir.to_path_buf(),
            },
        }
    }

    /// The directory the plugin's manifest is copied from.
    ///
    /// A checkout knows it; a published crate has to be asked of cargo, which resolved it
    /// into the registry source directory.
    fn manifest_source<'a>(&'a self, from_cargo: Option<&'a Path>) -> Option<&'a Path> {
        match self {
            Spec::Local { dir, .. } => Some(dir),
            Spec::Registry { .. } => from_cargo,
        }
    }
}

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
    install_with(
        cfg,
        paths,
        &Spec::Registry {
            crate_name,
            version,
        },
    )
}

/// Installs a plugin from a checkout on this machine.
///
/// `crate_name` and `version` are what the checkout's own manifest declares; the build is
/// the same one a published plugin gets.
pub fn install_from_path(
    cfg: &KitConfig,
    paths: &KitPaths,
    crate_name: &str,
    version: &str,
    dir: &Path,
) -> KitResult<Installed> {
    install_with(
        cfg,
        paths,
        &Spec::Local {
            crate_name,
            version,
            dir,
        },
    )
}

/// The install itself: one implementation for both sources.
fn install_with(cfg: &KitConfig, paths: &KitPaths, spec: &Spec<'_>) -> KitResult<Installed> {
    let crate_name = spec.crate_name();
    let cargo = which::which("cargo").map_err(|_| KitError::CargoNotFound)?;

    let wrapper_dir = paths.build_dir(crate_name);
    reset_wrapper(&wrapper_dir)?;
    std::fs::create_dir_all(wrapper_dir.join("src"))?;

    let stem = cfg.lib_stem(crate_name);

    // Ask the plugin crate what it declares about the contract crate, and have the wrapper ask
    // for exactly the same thing. Getting this wrong is not cosmetic: the private `wrapper`
    // module explains what two disagreeing requirements do to the build.
    let probe_dir = paths.build_dir(&format!("{crate_name}-probe"));
    let contract = probe_contract_requirement(cfg, &cargo, &probe_dir, spec)?;
    remove_dir_if_exists(&probe_dir)?;

    wrapper::write(
        cfg,
        &wrapper_dir,
        crate_name,
        &stem,
        spec.plugin_source(),
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

    copy_manifest(cfg, &plugin_dir, spec.manifest_source(plugin_src))?;

    Ok(Installed {
        crate_name: crate_name.to_string(),
        version: spec.version().to_string(),
        dir: plugin_dir,
        library: dest_lib,
        source: spec.install_source(),
    })
}

// ---- the wrapper directory ------------------------------------------------

/// Clears the wrapper project, keeping the cargo target directory inside it.
///
/// The wrapper is regenerated from scratch on every install, so its two generated files must
/// never survive from a previous run — a `Cargo.toml` still naming a plugin that has since been
/// removed would be built instead of the one being installed. What must survive is `target/`:
/// cargo puts its build cache there (the wrapper is its own single-element workspace, so there
/// is no workspace above it to place one), and deleting it makes every install recompile the
/// whole plugin stack — the plugin, the contract crate and every dependency of both. That is
/// the difference between an update that finishes in milliseconds and one that takes minutes.
///
/// So this removes the wrapper's *source*, not the wrapper: the files `wrapper::write` is about
/// to overwrite, plus anything a previous version of the kit might have left in an unexpected
/// place. `target` is the only thing named, because it is the only thing that has to be.
fn reset_wrapper(dir: &Path) -> KitResult<()> {
    if !dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == "target" {
            continue;
        }
        remove_entry(&entry.path())?;
    }

    Ok(())
}

/// Removes one file or directory tree.
fn remove_entry(path: &Path) -> KitResult<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
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
    spec: &Spec<'_>,
) -> KitResult<Option<ContractDep>> {
    std::fs::create_dir_all(dir.join("src"))?;
    // Its own workspace, so the wrapper's Cargo.toml above it does not claim it.
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n\
             [workspace]\n\n[dependencies]\n{}\n",
            spec.probe_dependency()
        ),
    )?;
    std::fs::write(dir.join("src").join("lib.rs"), "")?;

    let v = cargo_metadata_json(cargo, &dir.join("Cargo.toml"))?;
    Ok(declared_contract_dep(
        &v,
        spec.crate_name(),
        &cfg.contract_crate,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of [`reset_wrapper`]: the generated files go, cargo's cache stays.
    ///
    /// If `target/` is removed here, every install — `plugin add` as much as `plugin update` —
    /// recompiles the plugin and its entire dependency graph from scratch. That failure is
    /// invisible: the install still succeeds, it is just slow. So it is asserted here, where it
    /// costs a second, instead of being noticed as "why does this take minutes again".
    #[test]
    fn reset_wrapper_keeps_the_target_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("myapp-plugin-foo");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target").join("release")).unwrap();

        std::fs::write(dir.join("Cargo.toml"), "# stale\n").unwrap();
        std::fs::write(dir.join("src").join("lib.rs"), "// stale\n").unwrap();
        std::fs::write(dir.join("target").join("release").join("cached"), "x").unwrap();

        reset_wrapper(&dir).unwrap();

        assert!(
            !dir.join("Cargo.toml").exists() && !dir.join("src").exists(),
            "a stale generated file must not survive into the next build"
        );
        assert!(
            dir.join("target").join("release").join("cached").is_file(),
            "cargo's cache is what makes an update cheap"
        );
    }

    /// A wrapper directory that is not there yet is not an error: a first install creates it.
    #[test]
    fn reset_wrapper_accepts_a_missing_directory() {
        let tmp = tempfile::tempdir().unwrap();

        reset_wrapper(&tmp.path().join("never-built")).unwrap();
    }
}
