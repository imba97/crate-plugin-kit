//! Error types.
//!
//! `anyhow::Error` is deliberately kept out of the public API: this crate is used
//! by hosts right at the `dlopen` boundary, where an enumerable, matchable error
//! type is far easier to diagnose than a type-erased one. The internals (the
//! `install` module) use anyhow for convenience and convert at the exit points.

use std::path::PathBuf;

/// Errors produced by the kit.
///
/// The variants fall into two groups:
///
/// - Caller-actionable: [`KitError::NotInstalled`] / [`KitError::AlreadyInstalled`] /
///   [`KitError::PluginInUse`] / [`KitError::CargoNotFound`] — the caller usually
///   takes another route or prompts the user;
/// - Broken environment or data: everything else. These can only be reported.
#[derive(Debug, thiserror::Error)]
pub enum KitError {
    /// The plugin to uninstall or update is not installed.
    #[error("plugin {name} is not installed")]
    NotInstalled {
        /// Full crate name.
        name: String,
    },

    /// The plugin to install is already present.
    ///
    /// `install` never overwrites an existing installation; use `update` to replace it.
    #[error("directory for plugin {name} already exists: {path}")]
    AlreadyInstalled {
        /// Full crate name.
        name: String,
        /// The existing directory.
        path: PathBuf,
    },

    /// The plugin's dynamic library is still loaded in this process, so it cannot be deleted.
    ///
    /// A `.dll` that is currently `dlopen`ed cannot be removed on Windows; on
    /// Linux/macOS the removal succeeds but the disk space is not reclaimed. Rather
    /// than gamble on it, ask the user to retry from a clean process.
    #[error(
        "the dynamic library of plugin {name} is still loaded in this process and cannot be \
         deleted. Retry in a new terminal (a new process does not load it), or exit the \
         current session and try again"
    )]
    PluginInUse {
        /// Full crate name.
        name: String,
    },

    /// Reading the manifest file failed.
    #[error("failed to read manifest ({path}): {source}")]
    ManifestRead {
        /// Manifest path.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// The manifest is not valid TOML, or a field has the wrong type.
    #[error("failed to parse manifest ({path}): {source}")]
    ManifestParse {
        /// Manifest path.
        path: PathBuf,
        /// Underlying TOML error.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// A required field in the manifest is missing or empty.
    #[error("manifest is missing field `{field}` ({path})")]
    ManifestMissingField {
        /// Field name, e.g. `plugin.name`.
        field: String,
        /// Manifest path.
        path: PathBuf,
    },

    /// No dynamic library matching the naming rules was found in the install dir.
    #[error("could not find the dynamic library of plugin {name}; tried: {tried}")]
    LibraryNotFound {
        /// File name stem.
        name: String,
        /// The file names that were tried, comma-separated.
        tried: String,
    },

    /// `dlopen` itself failed (corrupt file, wrong architecture, missing dependencies).
    #[error("failed to load the dynamic library ({path}): {source}")]
    LibraryLoad {
        /// Dynamic library path.
        path: PathBuf,
        /// Underlying libloading error.
        #[source]
        source: Box<libloading::Error>,
    },

    /// The library loaded, but it does not export the symbol we look for.
    ///
    /// Usually means the wrong thing was installed — e.g. a plain library placed in
    /// the plugin directory.
    #[error("the dynamic library does not export symbol `{symbol}`")]
    SymbolMissing {
        /// The missing symbol name.
        symbol: String,
    },

    /// The plugin's entry function returned a null pointer.
    #[error("the plugin's entry function returned a null pointer")]
    NullEntry,

    /// Timed out waiting for the install lock.
    #[error("timed out waiting for the file lock ({path}, waited {secs}s)")]
    LockTimeout {
        /// Lock file path.
        path: PathBuf,
        /// How many seconds were spent waiting.
        secs: u64,
    },

    /// An operation on the lock file itself failed.
    #[error("file lock operation failed ({path}): {source}")]
    LockIo {
        /// Lock file path.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// No `cargo` on `PATH`, so build-host installation is unavailable.
    ///
    /// The caller can use this to tell the user to install a Rust toolchain, or to
    /// fall back to prebuilt.
    #[error("could not find `cargo` on PATH; build-host installation needs a Rust toolchain")]
    CargoNotFound,

    /// `cargo build` failed. stderr has already been passed through to the user
    /// (stdio is inherited).
    #[error("build-host compilation failed ({})", describe_code(.code))]
    BuildFailed {
        /// Process exit code. `None` when the process was killed by a signal.
        code: Option<i32>,
    },

    /// The build succeeded, but there is no cdylib among its artifacts.
    #[error("no cdylib among the build artifacts (looked for {stem} in {dir})")]
    BuildArtifactMissing {
        /// Expected file name stem.
        stem: String,
        /// Directories that were searched.
        dir: String,
    },

    /// No `Cargo.toml` where one was expected.
    #[error("no Cargo.toml in {dir}")]
    NoManifest {
        /// The directory that was looked in.
        dir: PathBuf,
    },

    /// `cargo metadata` did not describe the crate whose manifest was given.
    ///
    /// Either the manifest is not part of the reported workspace, or the output could
    /// not be matched to it.
    #[error("cargo metadata does not describe the package at {manifest}")]
    NoPackage {
        /// Manifest path that was asked about.
        manifest: PathBuf,
    },

    /// The plugin manifest's `[plugin] version` disagrees with the crate's own version.
    ///
    /// Release assets carry both, and they have to be the same number: the manifest is
    /// what a host reads before it downloads anything, and the asset file name is built
    /// from the crate version.
    #[error(
        "the plugin manifest {path} declares version {declared}, but the crate version is \
         {crate_version}"
    )]
    VersionMismatch {
        /// Manifest path.
        path: PathBuf,
        /// Version declared inside the manifest.
        declared: String,
        /// Version declared by `Cargo.toml`.
        crate_version: String,
    },

    /// The network request failed. The message carries the URL and status code.
    #[error("network request failed: {0}")]
    Http(String),

    /// The registry returned content we could fetch but that is not what we expect.
    #[error("registry returned an unexpected response: {0}")]
    Registry(String),

    /// A plugin panicked during a cross-boundary call (caught by `catch_unwind` on
    /// the host side).
    #[error("plugin panicked in {what} (caught by catch_unwind)")]
    PluginPanicked {
        /// Which call site, e.g. `"entry()"`.
        what: String,
    },

    /// Neither HOME nor an equivalent system directory is available, so the data
    /// dir cannot be determined.
    #[error("cannot determine the host application's data dir (no HOME and no system equivalent)")]
    NoDataDir,

    /// IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// This crate's `Result` alias.
pub type KitResult<T> = std::result::Result<T, KitError>;

/// Say what happened to the build process, rather than printing `Option`'s `Debug` output at
/// the user -- `Some(101)` and `None` are implementation details, not an explanation.
fn describe_code(code: &Option<i32>) -> String {
    match code {
        Some(code) => format!("exit code {code}"),
        None => "killed by a signal".to_string(),
    }
}
