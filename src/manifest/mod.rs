//! Reading and writing the plugin manifest.
//!
//! # schema
//!
//! The `[plugin]` and `[lib]` sections are defined by this crate; a host may append
//! its own sections to the same file (for example bmux's `[detect]`), which this crate
//! preserves verbatim and does not interpret.
//!
//! ```toml
//! [plugin]
//! name       = "cargo"
//! version    = "0.1.0"
//! abi        = 1                              # optional, for the host's own use
//! family     = "rust"                         # optional, for the host's own use
//! repository = "https://github.com/o/r"       # optional, used for prebuilt downloads
//!
//! [lib]
//! stem = "bmux_plugin_cargo"                  # optional; derived from KitConfig by default
//!
//! [detect]                                    # <- the host's own section, untouched by this crate
//! strong = ["Cargo.lock"]
//! weak   = ["Cargo.toml"]
//! ```
//!
//! # How the manifest reaches the install dir
//!
//! The plugin crate's source root holds a copy of `<manifest_name>`. During a
//! build-host install this crate uses `cargo metadata` to locate the crate source dir
//! and copies that file verbatim into the install dir, so there is a single source of
//! truth and no extra exported symbol is needed.
//!
//! # Layout
//!
//! [`PluginManifest`] and its file handling are here; the section types it is made of
//! are in `sections`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{KitError, KitResult};

mod sections;

pub use sections::{LibSection, PluginSection};

/// A parsed manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// The `[plugin]` section.
    pub plugin: PluginSection,

    /// The `[lib]` section. Derived from [`crate::KitConfig::lib_stem`] when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lib: Option<LibSection>,

    /// Any other sections belonging to the host (`[detect]` and the like), preserved
    /// verbatim.
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl PluginManifest {
    /// Reads from a file.
    pub fn read(path: &Path) -> KitResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| KitError::ManifestRead {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path)
    }

    /// Parses from a string.
    pub fn parse(text: &str, path: &Path) -> KitResult<Self> {
        let parsed: Self = toml::from_str(text).map_err(|source| KitError::ManifestParse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;

        // An empty name is a common symptom of a broken file; reporting it here beats
        // letting it travel on.
        if parsed.plugin.name.trim().is_empty() {
            return Err(KitError::ManifestMissingField {
                field: "plugin.name".to_string(),
                path: path.to_path_buf(),
            });
        }
        if parsed.plugin.version.trim().is_empty() {
            return Err(KitError::ManifestMissingField {
                field: "plugin.version".to_string(),
                path: path.to_path_buf(),
            });
        }
        Ok(parsed)
    }

    /// Serializes back to TOML text.
    pub fn to_toml(&self) -> KitResult<String> {
        toml::to_string_pretty(self)
            .map_err(|e| KitError::Registry(format!("failed to serialize manifest: {e}")))
    }

    /// Writes to a file.
    pub fn write(&self, path: &Path) -> KitResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = self.to_toml()?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// The effective cdylib file name stem.
    ///
    /// An explicit declaration in the manifest wins; otherwise it is derived with
    /// `cfg.lib_stem(crate_name)`.
    pub fn effective_lib_stem(&self, cfg: &crate::KitConfig, crate_name: &str) -> String {
        self.lib
            .as_ref()
            .and_then(|l| l.stem.clone())
            .unwrap_or_else(|| cfg.lib_stem(crate_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[plugin]
name       = "cargo"
version    = "0.1.0"
abi        = 1
family     = "rust"
repository = "https://github.com/imba97/bmux-plugin-cargo"

[lib]
stem = "custom_stem"

[detect]
strong = ["Cargo.lock"]
weak   = ["Cargo.toml"]
"#;

    pub(super) fn parse(text: &str) -> PluginManifest {
        PluginManifest::parse(text, Path::new("test.toml")).expect("should parse")
    }

    #[test]
    fn optional_sections_may_be_absent() {
        let m = parse(
            r#"
[plugin]
name    = "bare"
version = "1.0.0"
"#,
        );
        assert!(m.lib.is_none());
        assert!(m.plugin.abi.is_none());
        assert!(m.plugin.family.is_none());
        assert!(m.extra.is_empty());
    }

    /// Key behavior: a host-defined section (here `[detect]`) must be preserved
    /// verbatim. This crate does not understand it, and rewriting the manifest must
    /// never drop it.
    #[test]
    fn preserves_host_defined_sections() {
        let m = parse(SAMPLE);
        assert!(m.extra.contains_key("detect"), "extra = {:?}", m.extra);
    }

    #[test]
    fn round_trips_through_toml() {
        let m = parse(SAMPLE);
        let text = m.to_toml().expect("should serialize");
        let again =
            PluginManifest::parse(&text, Path::new("again.toml")).expect("should read back");

        assert_eq!(again.plugin.name, "cargo");
        assert_eq!(again.plugin.abi, Some(1));
        assert_eq!(
            again.lib.as_ref().and_then(|l| l.stem.clone()).as_deref(),
            Some("custom_stem")
        );
        assert!(again.extra.contains_key("detect"));
    }

    #[test]
    fn rejects_empty_name() {
        let err = PluginManifest::parse(
            "[plugin]\nname = \"\"\nversion = \"1.0.0\"\n",
            Path::new("t.toml"),
        );
        assert!(matches!(err, Err(KitError::ManifestMissingField { .. })));
    }

    #[test]
    fn rejects_blank_name() {
        let err = PluginManifest::parse(
            "[plugin]\nname = \"   \"\nversion = \"1.0.0\"\n",
            Path::new("t.toml"),
        );
        assert!(matches!(err, Err(KitError::ManifestMissingField { .. })));
    }

    #[test]
    fn rejects_empty_version() {
        let err = PluginManifest::parse(
            "[plugin]\nname = \"a\"\nversion = \"\"\n",
            Path::new("t.toml"),
        );
        assert!(matches!(err, Err(KitError::ManifestMissingField { .. })));
    }

    #[test]
    fn rejects_a_missing_required_section() {
        let err = PluginManifest::parse("x = 1\n", Path::new("t.toml"));
        assert!(matches!(err, Err(KitError::ManifestParse { .. })));
    }

    #[test]
    fn read_reports_a_missing_file() {
        let err = PluginManifest::read(Path::new("definitely/not/here.toml"));
        assert!(matches!(err, Err(KitError::ManifestRead { .. })));
    }

    #[test]
    fn effective_stem_prefers_the_manifest_declaration() {
        let cfg = crate::KitConfig::new("myapp");
        let m = parse(SAMPLE);
        assert_eq!(
            m.effective_lib_stem(&cfg, "myapp-plugin-cargo"),
            "custom_stem"
        );
    }

    #[test]
    fn effective_stem_derives_when_the_manifest_is_silent() {
        let cfg = crate::KitConfig::new("myapp");
        let m = parse("[plugin]\nname = \"cargo\"\nversion = \"0.1.0\"\n");
        assert_eq!(
            m.effective_lib_stem(&cfg, "myapp-plugin-cargo"),
            "myapp_plugin_cargo"
        );
    }

    #[test]
    fn write_then_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("myapp-plugin.toml");

        let m = parse(SAMPLE);
        m.write(&path).expect("should write to disk");

        let back = PluginManifest::read(&path).expect("should read back");
        assert_eq!(back.plugin.name, "cargo");
        assert!(back.extra.contains_key("detect"));
    }
}
