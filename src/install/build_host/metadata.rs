//! The few values wanted out of `cargo metadata`.
//!
//! build-host already has the wrapper's manifest on disk, so it asks cargo about that:
//! where the build output goes, and — since the wrapper depends on the plugin — where
//! the plugin crate's source directory is. The latter is how the plugin's own
//! `<manifest_name>` is found and copied into the install dir.

use std::path::{Path, PathBuf};

use crate::error::{KitError, KitResult};
use crate::wrapper::cargo_metadata_json;

/// The few values we want out of `cargo metadata`.
pub(super) struct Metadata {
    pub(super) target_directory: PathBuf,
    /// Crate name → source directory.
    packages: Vec<(String, PathBuf)>,
}

impl Metadata {
    pub(super) fn plugin_source_dir(&self, crate_name: &str) -> Option<&Path> {
        self.packages
            .iter()
            .find(|(name, _)| name == crate_name)
            .map(|(_, dir)| dir.as_path())
    }
}

pub(super) fn cargo_metadata(cargo: &Path, manifest_path: &Path) -> KitResult<Metadata> {
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
