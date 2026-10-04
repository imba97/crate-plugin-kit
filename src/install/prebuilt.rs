//! prebuilt: downloads a cdylib plus manifest straight from GitHub Releases.
//!
//! This is an accelerator path, not the default. It goes through GitHub, which
//! bypasses the cargo registry or mirror the user has configured, so it may not be
//! reachable:
//!
//! - the default stays build-host (`cargo build`, which naturally picks up the mirror
//!   configuration);
//! - prebuilt is only used when build-host is explicitly turned off, or when the
//!   caller wants to try saving a few minutes first;
//! - a `None` from any step means "no usable prebuilt", and the caller must fall back
//!   to build-host.
//!
//! # Artifact naming convention
//!
//! A release carries two bare files (not archives, which saves three dependencies —
//! tar/gzip/zip):
//!
//! ```text
//! {crate}-{version}-{target}.{so|dylib|dll}   <-- the cdylib itself
//! {crate}-{version}-{target}.toml             <-- the manifest
//! ```
//!
//! Example: `bmux-plugin-cargo-0.1.0-x86_64-pc-windows-msvc.dll`
//!
//! The repository URL comes from the crates.io `repository` field — at this point the
//! plugin is not installed yet, so the registry is the only thing we can ask.

use std::path::PathBuf;

use crate::cache::InstallSource;
use crate::config::{KitConfig, KitPaths};
use crate::error::{KitError, KitResult};
use crate::install::{remove_dir_if_exists, Installed};
use crate::manifest::PluginManifest;
use crate::registry::Registry;

/// Size limit for a single artifact. Guards against downloading something hundreds of
/// megabytes large.
const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;

/// Tries to install a prebuilt.
///
/// - `Ok(Some(_))`: installed.
/// - `Ok(None)`: no usable prebuilt (none published, or assets missing); the caller
///   should fall back to build-host.
/// - `Err(_)`: a hard error such as a network failure. The caller may still fall back,
///   but the error is worth surfacing first.
pub fn try_install(
    cfg: &KitConfig,
    paths: &KitPaths,
    registry: &Registry,
    crate_name: &str,
    version: &str,
) -> KitResult<Option<Installed>> {
    // 1. the repository URL can only come from the registry
    let Some(info) = registry.view(crate_name)? else {
        return Ok(None);
    };
    let Some(repo) = info.repository.as_deref() else {
        return Ok(None);
    };
    let Some((owner, name)) = parse_github_repo(repo) else {
        // Not GitHub, so there is no release URL we could guess
        return Ok(None);
    };

    let target = cfg.effective_target();
    let ext = platform_lib_ext();
    let base = format!("https://github.com/{owner}/{name}/releases/download/v{version}/{crate_name}-{version}-{target}");

    let lib_url = format!("{base}.{ext}");
    let manifest_url = format!("{base}.toml");

    // 2. fetch the cdylib first. A 404 means "no prebuilt was published".
    let Some(lib_bytes) = registry.try_download(&lib_url, MAX_ASSET_BYTES)? else {
        return Ok(None);
    };

    // 3. the manifest has to be published alongside it. Without it this prebuilt is
    //    unusable — no manifest means nothing can be detected, so installing it
    //    achieves nothing.
    let Some(manifest_bytes) = registry.try_download(&manifest_url, MAX_ASSET_BYTES)? else {
        return Ok(None);
    };

    // 4. the contents have to be consistent: the name and version in the manifest must
    //    be the ones we just downloaded
    let manifest_text = String::from_utf8(manifest_bytes)
        .map_err(|e| KitError::Registry(format!("{manifest_url} is not valid UTF-8: {e}")))?;
    let manifest = PluginManifest::parse(&manifest_text, &PathBuf::from(&manifest_url))?;

    if manifest.plugin.version != version {
        return Err(KitError::Registry(format!(
            "prebuilt manifest version mismatch: the asset declares {}, but {version} was requested",
            manifest.plugin.version
        )));
    }

    // 5. land it
    let plugin_dir = paths.plugin_dir(crate_name);
    remove_dir_if_exists(&plugin_dir)?;
    std::fs::create_dir_all(&plugin_dir)?;

    let lib_name = format!("{}.{ext}", manifest.effective_lib_stem(cfg, crate_name));
    let dest_lib = plugin_dir.join(&lib_name);
    std::fs::write(&dest_lib, lib_bytes)?;

    manifest.write(&paths.manifest_path(crate_name, &cfg.manifest_name))?;

    Ok(Some(Installed {
        crate_name: crate_name.to_string(),
        version: version.to_string(),
        dir: plugin_dir,
        library: dest_lib,
        source: InstallSource::Prebuilt,
    }))
}

/// cdylib extension for the current platform.
fn platform_lib_ext() -> &'static str {
    match std::env::consts::OS {
        "windows" => "dll",
        "macos" => "dylib",
        _ => "so",
    }
}

/// Extracts `(owner, repo)` from a GitHub URL.
///
/// These forms are recognized:
///
/// ```text
/// https://github.com/owner/repo
/// https://github.com/owner/repo.git
/// https://github.com/owner/repo/
/// git+https://github.com/owner/repo
/// ```
pub fn parse_github_repo(url: &str) -> Option<(String, String)> {
    let rest = url
        .trim()
        .strip_prefix("git+")
        .unwrap_or(url)
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;

    let mut parts = rest.trim_end_matches('/').split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");

    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::parse_github_repo;

    #[test]
    fn parses_common_forms() {
        let want = Some(("imba97".to_string(), "bmux-plugin-cargo".to_string()));
        assert_eq!(
            parse_github_repo("https://github.com/imba97/bmux-plugin-cargo"),
            want
        );
        assert_eq!(
            parse_github_repo("https://github.com/imba97/bmux-plugin-cargo.git"),
            want
        );
        assert_eq!(
            parse_github_repo("https://github.com/imba97/bmux-plugin-cargo/"),
            want
        );
        assert_eq!(
            parse_github_repo("git+https://github.com/imba97/bmux-plugin-cargo"),
            want
        );
    }

    #[test]
    fn rejects_non_github() {
        assert_eq!(parse_github_repo("https://gitlab.com/a/b"), None);
        assert_eq!(parse_github_repo("https://github.com/onlyowner"), None);
    }
}
