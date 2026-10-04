//! Everything that goes through `cargo metadata`.
//!
//! Locating the plugin crate and reading what it declares cannot be done from the
//! filesystem alone — a crate's real name, its version and its source directory are
//! what cargo resolved, not what a path suggests. So the raw JSON of
//! `cargo metadata --format-version 1` is fetched once and the few values wanted are
//! picked out of it here.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{KitError, KitResult};

/// Where the wrapper gets the plugin crate from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PluginSource<'a> {
    /// A published crate, pinned exactly: the version recorded in the install record
    /// and the cdylib that was actually built must be the same one.
    Registry { version: &'a str },

    /// A local checkout, as a path dependency. Used when producing release assets:
    /// the version being published is not on the registry yet.
    Path { dir: &'a Path },
}

/// What the plugin declares about the contract crate.
///
/// Taken from the plugin crate's own manifest, which is the only place that knows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ContractDep {
    /// Version requirement, such as `=0.0.3`.
    pub req: Option<String>,

    /// Set when the plugin depends on the contract crate by path.
    ///
    /// The wrapper then mirrors the path as well, so packing a checkout works before
    /// the contract crate has ever been published.
    pub path: Option<PathBuf>,
}

/// Runs `cargo metadata --format-version 1` for a manifest and hands back the raw JSON.
pub(crate) fn cargo_metadata_json(
    cargo: &Path,
    manifest_path: &Path,
) -> KitResult<serde_json::Value> {
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

/// The contract dependency `crate_name` declares, as cargo resolved it.
pub(crate) fn declared_contract_dep(
    v: &serde_json::Value,
    crate_name: &str,
    contract: &str,
) -> Option<ContractDep> {
    let dep = v
        .get("packages")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(|x| x.as_str()) == Some(crate_name))?
        .get("dependencies")?
        .as_array()?
        .iter()
        .find(|d| d.get("name").and_then(|x| x.as_str()) == Some(contract))?;

    let req = dep.get("req").and_then(|x| x.as_str()).map(str::to_owned);
    let path = dep.get("path").and_then(|x| x.as_str()).map(PathBuf::from);

    if req.is_none() && path.is_none() {
        return None;
    }

    Some(ContractDep { req, path })
}

/// The `[package]` identity of the crate whose manifest is `manifest_path`.
///
/// `cargo metadata` describes every member of the workspace the manifest belongs to,
/// so the right package is picked by its manifest path — not by "the first one", which
/// would silently pack a sibling crate in a multi-crate repository.
pub(crate) fn package_identity(
    v: &serde_json::Value,
    manifest_path: &Path,
) -> KitResult<(String, String)> {
    let packages = v
        .get("packages")
        .and_then(|x| x.as_array())
        .ok_or_else(|| KitError::NoPackage {
            manifest: manifest_path.to_path_buf(),
        })?;

    let wanted = manifest_path
        .canonicalize()
        .unwrap_or_else(|_| manifest_path.to_path_buf());

    let found = packages.iter().find(|p| {
        p.get("manifest_path")
            .and_then(|x| x.as_str())
            .map(PathBuf::from)
            .map(|mp| mp.canonicalize().unwrap_or(mp) == wanted)
            .unwrap_or(false)
    });

    let pkg = match found {
        Some(p) => p,
        // A single-member manifest: cargo reports exactly one package, so the match by
        // path is redundant rather than wrong. Anything else is a genuine ambiguity.
        None if packages.len() == 1 => &packages[0],
        None => {
            return Err(KitError::NoPackage {
                manifest: manifest_path.to_path_buf(),
            })
        }
    };

    let name = pkg
        .get("name")
        .and_then(|x| x.as_str())
        .ok_or_else(|| KitError::NoPackage {
            manifest: manifest_path.to_path_buf(),
        })?;
    let version = pkg.get("version").and_then(|x| x.as_str()).ok_or_else(|| {
        KitError::ManifestMissingField {
            field: "package.version".to_string(),
            path: manifest_path.to_path_buf(),
        }
    })?;

    Ok((name.to_string(), version.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            declared_contract_dep(&v, "myapp-plugin-foo", "myapp-plugin"),
            Some(ContractDep {
                req: Some("=0.0.3".to_string()),
                path: None,
            })
        );
    }

    /// A plugin that does not depend on the contract crate at all leaves the caller on
    /// the configured fallback rather than inventing something.
    #[test]
    fn a_missing_requirement_is_none() {
        let v = metadata_with("something-else", "^2");
        assert_eq!(
            declared_contract_dep(&v, "myapp-plugin-foo", "myapp-plugin"),
            None
        );
        assert_eq!(
            declared_contract_dep(&v, "not-in-the-graph", "myapp-plugin"),
            None
        );
    }

    /// A checkout that points at the contract crate by path keeps that path.
    #[test]
    fn a_path_dependency_on_the_contract_crate_is_read() {
        let v = serde_json::json!({
            "packages": [{
                "name": "myapp-plugin-foo",
                "dependencies": [{
                    "name": "myapp-plugin",
                    "req": "^0.1.0",
                    "path": "/local/contract",
                }],
            }],
        });

        let dep = declared_contract_dep(&v, "myapp-plugin-foo", "myapp-plugin").unwrap();
        assert_eq!(dep.path, Some(PathBuf::from("/local/contract")));
        assert_eq!(dep.req.as_deref(), Some("^0.1.0"));
    }

    #[test]
    fn picks_the_package_by_manifest_path() {
        let v = serde_json::json!({
            "packages": [
                { "name": "sibling", "version": "9.9.9", "manifest_path": "/repo/sibling/Cargo.toml" },
                { "name": "mine", "version": "0.1.0", "manifest_path": "/repo/mine/Cargo.toml" },
            ],
        });

        let got = package_identity(&v, Path::new("/repo/mine/Cargo.toml")).unwrap();
        assert_eq!(got, ("mine".to_string(), "0.1.0".to_string()));
    }

    #[test]
    fn a_single_package_manifest_needs_no_path_match() {
        let v = serde_json::json!({
            "packages": [{ "name": "only", "version": "0.2.0", "manifest_path": "/elsewhere/Cargo.toml" }],
        });

        assert_eq!(
            package_identity(&v, Path::new("/repo/Cargo.toml")).unwrap(),
            ("only".to_string(), "0.2.0".to_string())
        );
    }

    #[test]
    fn an_unknown_package_is_an_error() {
        let v = serde_json::json!({
            "packages": [
                { "name": "a", "version": "1.0.0", "manifest_path": "/repo/a/Cargo.toml" },
                { "name": "b", "version": "1.0.0", "manifest_path": "/repo/b/Cargo.toml" },
            ],
        });

        assert!(matches!(
            package_identity(&v, Path::new("/repo/c/Cargo.toml")),
            Err(KitError::NoPackage { .. })
        ));
    }

    #[test]
    fn empty_metadata_is_an_error() {
        assert!(matches!(
            package_identity(&serde_json::json!({}), Path::new("/repo/Cargo.toml")),
            Err(KitError::NoPackage { .. })
        ));
    }
}
