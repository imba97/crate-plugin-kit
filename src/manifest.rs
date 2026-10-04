//! 插件 manifest 的读写。
//!
//! # schema
//!
//! `[plugin]` 与 `[lib]` 两段由**本 crate** 定义；宿主可以往同一个文件里追加
//! 自己的段（例如 bmux 的 `[detect]`），本 crate 原样保留、不做解释。
//!
//! ```toml
//! [plugin]
//! name       = "cargo"
//! version    = "0.1.0"
//! abi        = 1                              # 可选，宿主自用
//! family     = "rust"                         # 可选，宿主自用
//! repository = "https://github.com/o/r"       # 可选，prebuilt 下载用
//!
//! [lib]
//! stem = "bmux_plugin_cargo"                  # 可选；缺省由 KitConfig 推导
//!
//! [detect]                                    # ← 宿主自己的段，本 crate 不碰
//! strong = ["Cargo.lock"]
//! weak   = ["Cargo.toml"]
//! ```
//!
//! # manifest 是怎么进到安装目录的
//!
//! **插件 crate 的源码根目录里就放一份 `<manifest_name>`**，build-host 安装时
//! 本 crate 用 `cargo metadata` 定位 crate 源码目录，把这个文件原样拷进安装目录。
//!
//! 这样：
//!
//! - **单一事实来源** —— detect 信息跟着插件代码走，不会两边不同步；
//! - **安装后是纯文件** —— 检测阶段只读文件，不需要 `dlopen`；
//! - **不改 ABI** —— 不需要多导出一个"把 manifest 交出来"的符号。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{KitError, KitResult};

/// 解析后的 manifest。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// `[plugin]` 段。
    pub plugin: PluginSection,

    /// `[lib]` 段。缺省时由 [`crate::KitConfig::lib_stem`] 推导。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lib: Option<LibSection>,

    /// 宿主自己的其它段（`[detect]` 之类），原样保留。
    #[serde(flatten)]
    pub extra: toml::Table,
}

/// manifest 的 `[plugin]` 段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSection {
    /// 插件名。**这是权威来源** —— 加载后宿主应校验插件自报名是否与它一致。
    pub name: String,

    /// 插件版本。
    pub version: String,

    /// 跨边界 ABI 版本。宿主自用，本 crate 只做搬运。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi: Option<u32>,

    /// 生态分组。宿主自用，本 crate 只做搬运。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,

    /// 插件仓库地址。prebuilt 下载时需要它来拼 Release URL。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,

    /// 人类可读描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// 其它键，原样保留。
    #[serde(flatten)]
    pub extra: toml::Table,
}

/// manifest 的 `[lib]` 段。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibSection {
    /// cdylib 文件名主干（平台无关，不含 `lib` 前缀与扩展名）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem: Option<String>,

    /// 其它键，原样保留。
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl PluginManifest {
    /// 从文件读。
    pub fn read(path: &Path) -> KitResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| KitError::ManifestRead {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path)
    }

    /// 从字符串解析。
    pub fn parse(text: &str, path: &Path) -> KitResult<Self> {
        let parsed: Self = toml::from_str(text).map_err(|source| KitError::ManifestParse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;

        // 空名字是常见的"文件写坏了"症状，单独报出来比让它一路带下去好。
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

    /// 序列化回 TOML 文本。
    pub fn to_toml(&self) -> KitResult<String> {
        toml::to_string_pretty(self)
            .map_err(|e| KitError::Registry(format!("序列化 manifest 失败：{e}")))
    }

    /// 写到文件。
    pub fn write(&self, path: &Path) -> KitResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = self.to_toml()?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// 生效的 cdylib 文件名主干。
    ///
    /// 优先用 manifest 里的显式声明；没有就用 `cfg.lib_stem(crate_name)` 推导。
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
    use std::path::Path;

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

    fn parse(text: &str) -> PluginManifest {
        PluginManifest::parse(text, Path::new("test.toml")).expect("应当解析成功")
    }

    #[test]
    fn parses_the_plugin_section() {
        let m = parse(SAMPLE);
        assert_eq!(m.plugin.name, "cargo");
        assert_eq!(m.plugin.version, "0.1.0");
        assert_eq!(m.plugin.abi, Some(1));
        assert_eq!(m.plugin.family.as_deref(), Some("rust"));
        assert_eq!(
            m.plugin.repository.as_deref(),
            Some("https://github.com/imba97/bmux-plugin-cargo")
        );
    }

    /// 关键行为：宿主自己的段（这里是 `[detect]`）必须被**原样保留**。
    /// 这个 crate 不认识它，重写 manifest 时绝不能把它丢掉。
    #[test]
    fn preserves_host_defined_sections() {
        let m = parse(SAMPLE);
        assert!(m.extra.contains_key("detect"), "extra = {:?}", m.extra);
    }

    #[test]
    fn round_trips_through_toml() {
        let m = parse(SAMPLE);
        let text = m.to_toml().expect("应当能序列化");
        let again = PluginManifest::parse(&text, Path::new("again.toml")).expect("应当能读回");

        assert_eq!(again.plugin.name, "cargo");
        assert_eq!(again.plugin.abi, Some(1));
        assert_eq!(
            again.lib.as_ref().and_then(|l| l.stem.clone()).as_deref(),
            Some("custom_stem")
        );
        assert!(again.extra.contains_key("detect"));
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
        m.write(&path).expect("应当能落盘");

        let back = PluginManifest::read(&path).expect("应当能读回");
        assert_eq!(back.plugin.name, "cargo");
        assert!(back.extra.contains_key("detect"));
    }
}
