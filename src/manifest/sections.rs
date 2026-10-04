//! The sections a manifest is made of.
//!
//! Only two sections are defined by this crate: `[plugin]`, which every host reads, and
//! `[lib]`, which lets a plugin override the derived cdylib stem. Everything else a host
//! adds is carried through untouched in the `extra` tables of `PluginManifest` and
//! [`PluginSection`].
//!
//! The types are only data: nothing here reads or writes a file.

use serde::{Deserialize, Serialize};

/// The `[plugin]` section of a manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSection {
    /// Plugin name. This is the authoritative source — after loading, a host should
    /// check that the plugin's self-declared name matches it.
    pub name: String,

    /// Plugin version.
    pub version: String,

    /// Cross-boundary ABI version. For the host's own use; this crate only moves it
    /// around.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi: Option<u32>,

    /// Ecosystem family. For the host's own use; this crate only moves it around.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,

    /// Plugin repository URL. Needed when downloading a prebuilt, to build the release
    /// URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,

    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Any other keys, preserved verbatim.
    #[serde(flatten)]
    pub extra: toml::Table,
}

/// The `[lib]` section of a manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibSection {
    /// cdylib file name stem (platform-independent, without the `lib` prefix and the
    /// extension).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem: Option<String>,

    /// Any other keys, preserved verbatim.
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[cfg(test)]
mod tests {
    use crate::manifest::tests::parse;

    #[test]
    fn parses_the_plugin_section() {
        let m = parse(
            r#"
[plugin]
name       = "cargo"
version    = "0.1.0"
abi        = 1
family     = "rust"
repository = "https://github.com/imba97/bmux-plugin-cargo"
"#,
        );
        assert_eq!(m.plugin.name, "cargo");
        assert_eq!(m.plugin.version, "0.1.0");
        assert_eq!(m.plugin.abi, Some(1));
        assert_eq!(m.plugin.family.as_deref(), Some("rust"));
        assert_eq!(
            m.plugin.repository.as_deref(),
            Some("https://github.com/imba97/bmux-plugin-cargo")
        );
    }

    #[test]
    fn parses_the_lib_section() {
        let m = parse("[plugin]\nname = \"cargo\"\nversion = \"0.1.0\"\n\n[lib]\nstem = \"s\"\n");
        assert_eq!(m.lib.as_ref().and_then(|l| l.stem.as_deref()), Some("s"));
    }
}
