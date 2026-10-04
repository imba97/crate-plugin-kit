//! Generating the wrapper project that turns a plugin rlib into a cdylib.
//!
//! # Why a wrapper is needed
//!
//! `cargo install` only knows about bin targets, so a `crate-type = ["cdylib"]` crate
//! cannot be installed that way. And `cargo build` on a plugin crate directly does not
//! make Cargo produce a cdylib for a dependency either.
//!
//! So a wrapper project of a few dozen lines is generated:
//!
//! ```text
//! <build dir>/<crate>/
//! ├── Cargo.toml        # [lib] crate-type = ["cdylib"], depends on the plugin and the host contract crate
//! └── src/lib.rs        # one line: <contract>::export!(<plugin_crate>::create);
//! ```
//!
//! The benefit is that the plugin's own crate stays a plain rlib: crates.io consumes
//! it normally, unit tests can `use` it directly, and the plugin author does not have
//! to write a single `#[no_mangle]`.
//!
//! # Two callers, one implementation
//!
//! | Caller | Plugin source | Purpose |
//! | --- | --- | --- |
//! | [`crate::install::build_host`] | a published crate, pinned | install on the user's machine |
//! | [`crate::pack`] | a local checkout, as a path dependency | produce the release assets |
//!
//! They must not drift apart. If the wrapper here stops calling `export!`, the cdylib
//! has no entry symbol and `dlopen` fails — on someone else's machine. If it stops
//! copying the plugin's own contract requirement, the graph holds two copies of the
//! contract crate and the build fails with an E0308 that mentions neither version.
//!
//! # Why the contract requirement is copied verbatim
//!
//! Getting this wrong is not a cosmetic problem. If the wrapper's requirement and the
//! plugin's own requirement resolve to different versions, the graph holds two copies
//! of the contract crate; each defines its own `PackageManager`, and the `export!` in
//! the wrapper stops typechecking with an E0308 that the plugin author has no way to
//! see coming. Cargo will not unify them for us even when one version satisfies both,
//! because `0.0.x` releases are incompatible across patches by definition. Copying the
//! plugin's own requirement makes the two agree by construction.
//!
//! # Layout
//!
//! [`write`] is the entry point: it renders the two files (`metadata` says where the
//! plugin comes from, `render` turns that into the manifest text) and writes them.
//! The `cargo metadata` helpers live in `metadata`, next to the types describing what
//! the plugin declares.

use std::path::Path;

use crate::config::KitConfig;
use crate::error::KitResult;

mod metadata;
mod render;

use render::wrapper_toml;

pub(crate) use metadata::{
    cargo_metadata_json, declared_contract_dep, package_identity, ContractDep, PluginSource,
};

/// Writes the wrapper project into `dir` (which must already exist).
pub(crate) fn write(
    cfg: &KitConfig,
    dir: &Path,
    crate_name: &str,
    stem: &str,
    plugin: PluginSource<'_>,
    contract: Option<&ContractDep>,
) -> KitResult<()> {
    let crate_ident = crate_name.replace('-', "_");

    // Every generated file is rewritten on each call; the comment at the top of the
    // manifest says so, because the copies under the data dir and the temp dir look
    // editable but are not.
    let rewritten = match plugin {
        PluginSource::Registry { .. } => "every install rewrites it",
        PluginSource::Path { .. } => "plugin-asset rewrites it on every run",
    };

    let toml = wrapper_toml(cfg, crate_name, stem, plugin, contract, rewritten);

    std::fs::write(dir.join("Cargo.toml"), toml)?;

    let body = cfg.wrapper_body.replace("{crate_ident}", &crate_ident);
    std::fs::write(dir.join("src").join("lib.rs"), body)?;

    // No `rust-toolchain.toml` is written for the wrapper.
    //
    // A wrapper under a data dir has no toolchain file above it, so cargo uses the
    // current default toolchain — which is what we want: whatever toolchain you were
    // already using. Pinning a channel would instead trigger an unexpected rustup
    // download during install.
    //
    // The host and the plugin do not need the same toolchain anyway; the only thing
    // crossing the boundary is `#[repr(C)]` data.

    Ok(())
}

#[cfg(test)]
mod tests;
