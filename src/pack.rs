//! Producing the two files a plugin release carries.
//!
//! This is the mirror image of [`crate::install`]: instead of turning a published crate
//! into a cdylib on the user's machine, it turns a checkout into the assets that let
//! [`crate::install::prebuilt`] skip the build entirely.
//!
//! ```text
//! {crate}-{version}-{target}.{so|dylib|dll}   <-- the cdylib
//! {crate}-{version}-{target}.toml             <-- the plugin manifest, verbatim
//! ```
//!
//! # Why these names
//!
//! [`crate::install::prebuilt`] builds its download URL by putting these two names after
//! `.../releases/download/v{version}/`. Producing and consuming are one convention:
//! change either side alone and every release silently falls back to building from
//! source. Nothing here may be renamed without renaming it there too.
//!
//! # What it does
//!
//! 1. `cargo metadata` the plugin's manifest, for the crate name and version.
//! 2. Check that the plugin manifest beside it declares the same version — a manifest
//!    that lags behind `Cargo.toml` would publish assets nobody can consume.
//! 3. Generate the wrapper — the same one [`crate::install`] builds, coming from the
//!    private `wrapper` module — with the plugin as a **path** dependency, and
//!    `cargo build --release --target <triple>` it.
//! 4. Copy the cdylib and the plugin manifest into the output directory under the names
//!    above, and — when the build target is this machine — `dlopen` the result to prove
//!    it really exports the entry symbol.
//!
//! A failed build leaves the generated wrapper in the temp directory, which is where
//! someone looks to see why; a successful one removes it again.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::KitConfig;
use crate::error::{KitError, KitResult};
use crate::install::remove_dir_if_exists;
use crate::manifest::PluginManifest;
use crate::wrapper::{self, cargo_metadata_json, declared_contract_dep, PluginSource};

/// Options for [`build`].
#[derive(Debug, Clone)]
pub struct PackOptions {
    /// Where cargo puts its build.
    ///
    /// `None` uses `<plugin dir>/target`, which is the directory a CI cache action
    /// normally keeps — so the dependency builds are cached between releases.
    pub target_dir: Option<PathBuf>,

    /// `dlopen` the finished cdylib and look for the entry symbol.
    ///
    /// Defaults to `true`. It only happens when the build target is the machine doing
    /// the packing: a cross-compiled artifact cannot be loaded here, so that case is
    /// skipped silently rather than reported as a failure.
    pub verify_export: bool,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            target_dir: None,
            verify_export: true,
        }
    }
}

/// The two files that were written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedAssets {
    /// Full crate name, such as `bmux-plugin-cargo`.
    pub crate_name: String,
    /// The crate's own version.
    pub version: String,
    /// Target triple the cdylib was built for.
    pub target: String,
    /// Path of the written cdylib.
    pub library: PathBuf,
    /// Path of the written manifest.
    pub manifest: PathBuf,
}

impl PackedAssets {
    /// The base name both files share: `{crate}-{version}-{target}`.
    pub fn base_name(&self) -> String {
        format!("{}-{}-{}", self.crate_name, self.version, self.target)
    }
}

/// Builds a plugin checkout into the two files a release carries.
///
/// `cfg` supplies the naming rules (it is the same [`KitConfig`] the host installs
/// with); `plugin_dir` is the root of the plugin crate; `out_dir` is created if needed.
///
/// # Errors
///
/// - [`KitError::CargoNotFound`]: no `cargo` on `PATH`;
/// - [`KitError::NoManifest`]: no `Cargo.toml` in `plugin_dir`;
/// - [`KitError::NoPackage`]: `cargo metadata` did not describe that manifest;
/// - [`KitError::ManifestMissingField`] / [`KitError::ManifestParse`]: the plugin
///   manifest is missing or unreadable;
/// - [`KitError::VersionMismatch`]: the plugin manifest and `Cargo.toml` disagree;
/// - [`KitError::BuildFailed`] / [`KitError::BuildArtifactMissing`]: the wrapper build
///   failed or produced nothing loadable;
/// - [`KitError::LibraryLoad`] / [`KitError::SymbolMissing`]: the artifact does not
///   export the entry symbol, so publishing it would produce an unloadable plugin.
pub fn build(
    cfg: &KitConfig,
    plugin_dir: impl AsRef<Path>,
    out_dir: impl AsRef<Path>,
    options: &PackOptions,
) -> KitResult<PackedAssets> {
    let cargo = which::which("cargo").map_err(|_| KitError::CargoNotFound)?;

    let plugin_dir = plugin_dir.as_ref().canonicalize()?;
    let manifest_path = plugin_dir.join("Cargo.toml");
    if !manifest_path.is_file() {
        return Err(KitError::NoManifest { dir: plugin_dir });
    }

    // 1. what the crate calls itself, and which version this is
    let metadata = cargo_metadata_json(&cargo, &manifest_path)?;
    let (crate_name, version) = wrapper::package_identity(&metadata, &manifest_path)?;

    // 2. the manifest that travels with the cdylib has to agree with Cargo.toml.
    //    A host reads the manifest before it downloads anything, and the asset file
    //    name is built from the crate version; if the two disagree, one of them lies.
    let plugin_manifest = plugin_dir.join(&cfg.manifest_name);
    let manifest = PluginManifest::read(&plugin_manifest)?;
    if manifest.plugin.version != version {
        return Err(KitError::VersionMismatch {
            path: plugin_manifest,
            declared: manifest.plugin.version,
            crate_version: version,
        });
    }

    // 3. the wrapper, then the build
    let target = cfg.effective_target().to_string();
    let target_dir = options
        .target_dir
        .clone()
        .unwrap_or_else(|| plugin_dir.join("target"));

    // A directory of this run's own. Sharing one by crate name would mean two runs — two
    // tests, or two pack calls in one program — deleting and writing the same files at
    // once; on Windows deleting a directory another process still has open fails outright
    // with "access denied".
    let wrapper_dir = wrapper_dir(cfg, &crate_name);
    std::fs::create_dir_all(wrapper_dir.join("src"))?;

    let stem = cfg.lib_stem(&crate_name);
    let contract = declared_contract_dep(&metadata, &crate_name, &cfg.contract_crate);
    wrapper::write(
        cfg,
        &wrapper_dir,
        &crate_name,
        &stem,
        PluginSource::Path { dir: &plugin_dir },
        contract.as_ref(),
    )?;

    // stdio is inherited: this can take a while and the caller should see cargo's output
    let status = Command::new(&cargo)
        .arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(wrapper_dir.join("Cargo.toml"))
        .arg("--target")
        .arg(&target)
        .arg("--target-dir")
        .arg(&target_dir)
        .status()
        .map_err(KitError::Io)?;

    if !status.success() {
        return Err(KitError::BuildFailed {
            code: status.code(),
        });
    }

    // 4. name it the way `prebuilt` looks for it
    let release_dir = target_dir.join(&target).join("release");
    let built =
        crate::loader::find_library_for_target(&release_dir, &stem, &target).map_err(|_| {
            KitError::BuildArtifactMissing {
                stem: stem.clone(),
                dir: release_dir.display().to_string(),
            }
        })?;

    let out_dir = out_dir.as_ref();
    std::fs::create_dir_all(out_dir)?;

    let base = format!("{crate_name}-{version}-{target}");
    let library = out_dir.join(format!(
        "{base}.{}",
        crate::loader::lib_ext_for_target(&target)
    ));
    std::fs::copy(&built, &library)?;

    let manifest_out = out_dir.join(format!("{base}.toml"));
    std::fs::copy(&plugin_manifest, &manifest_out)?;

    if options.verify_export && target == crate::TARGET_TRIPLE {
        // The one check that cannot be made by inspecting file names: the symbol a host
        // resolves before it calls anything. Catching it here costs a `dlopen`; catching
        // it in the field costs a release.
        //
        // `()` as the entry type: the pointer is never dereferenced, so no layout
        // assumption is made (see `loader::open`).
        let _loaded = unsafe { crate::loader::open::<()>(&library, &cfg.entry_symbol) }?;
    }

    // Only worth keeping while something went wrong; a caller that packs repeatedly should
    // not leave a trail of scaffolding in the temp directory. This removes the whole per-run
    // directory, the crate directory inside it included.
    //
    // Failure is ignored on purpose: see `wrapper_dir` for why removing it can be refused on
    // Windows, and a leftover directory is not a reason to fail a successful pack.
    if let Some(run_root) = wrapper_dir.parent() {
        let _ = remove_dir_if_exists(run_root);
    }

    Ok(PackedAssets {
        crate_name,
        version,
        target,
        library,
        manifest: manifest_out,
    })
}

/// Where this run's generated wrapper lives: `<temp>/<id>-plugin-asset-<pid>-<n>/<crate>`.
///
/// Unique per call, not per crate: two runs at once (two threads packing, or two `plugin
/// asset` processes) must not delete and rewrite each other's files. The crate name stays in
/// the path so a leftover directory is still identifiable, and the process id separates
/// concurrent processes.
///
/// A failed build leaves the directory behind, which is the point — that is where someone
/// looks. A successful one removes the per-run directory (its parent) again.
fn wrapper_dir(cfg: &KitConfig, crate_name: &str) -> PathBuf {
    static RUN: AtomicU64 = AtomicU64::new(0);

    let run = RUN.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "{}-plugin-asset-{}-{run}",
            cfg.id,
            std::process::id()
        ))
        .join(crate_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_name_is_crate_version_target() {
        let assets = PackedAssets {
            crate_name: "bmux-plugin-cargo".to_string(),
            version: "0.1.0".to_string(),
            target: "x86_64-pc-windows-msvc".to_string(),
            library: PathBuf::from("a.dll"),
            manifest: PathBuf::from("a.toml"),
        };

        assert_eq!(
            assets.base_name(),
            "bmux-plugin-cargo-0.1.0-x86_64-pc-windows-msvc"
        );
    }

    /// The regression this exists for: one directory per crate, shared by concurrent runs,
    /// meant one run could be deleting files another was writing or compiling — which on
    /// Windows fails outright with "access denied".
    #[test]
    fn every_run_gets_its_own_wrapper_dir() {
        let cfg = KitConfig::new("myapp");

        let first = wrapper_dir(&cfg, "myapp-plugin-foo");
        let second = wrapper_dir(&cfg, "myapp-plugin-foo");
        assert_ne!(first, second, "{first:?} must not be reused");

        // Still identifiable: the crate name is the last component and the app id leads.
        assert!(first.ends_with("myapp-plugin-foo"), "{}", first.display());
        assert!(
            first
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().starts_with("myapp-plugin-asset"))
                .unwrap_or(false),
            "{}",
            first.display()
        );
    }

    #[test]
    fn two_crates_do_not_share_a_directory() {
        let cfg = KitConfig::new("myapp");

        assert_ne!(
            wrapper_dir(&cfg, "myapp-plugin-foo"),
            wrapper_dir(&cfg, "myapp-plugin-bar")
        );
    }

    #[test]
    fn verify_export_is_on_by_default() {
        assert!(PackOptions::default().verify_export);
        assert!(PackOptions::default().target_dir.is_none());
    }

    #[test]
    fn a_missing_manifest_is_reported_as_such() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = KitConfig::new("myapp");

        let err = build(
            &cfg,
            tmp.path(),
            tmp.path().join("dist"),
            &PackOptions::default(),
        )
        .unwrap_err();

        assert!(matches!(err, KitError::NoManifest { .. }), "{err:?}");
    }
}
