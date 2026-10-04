//! 安装记录缓存 `<plugins>/.plugins.json`。
//!
//! 这个文件是**加速与展示用**，不是事实来源 —— 事实来源永远是每个插件目录里的
//! manifest。缓存丢了、坏了，`list()` 会从磁盘重建。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::KitResult;

/// 当前缓存 schema 版本。
pub const CACHE_VERSION: u32 = 1;

/// `.plugins.json` 的内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    /// schema 版本。
    #[serde(rename = "cacheVersion", default = "default_cache_version")]
    pub cache_version: u32,

    /// crate 名 → 安装记录。
    #[serde(default)]
    pub plugins: BTreeMap<String, IndexEntry>,
}

fn default_cache_version() -> u32 {
    CACHE_VERSION
}

impl Default for Index {
    fn default() -> Self {
        Self {
            cache_version: CACHE_VERSION,
            plugins: BTreeMap::new(),
        }
    }
}

/// 一条安装记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// 安装的版本。
    pub version: String,

    /// manifest 里声明的 ABI 版本（宿主自用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi: Option<u32>,

    /// 安装目录的绝对路径。
    pub root: PathBuf,

    /// 安装来源。
    #[serde(default)]
    pub source: InstallSource,

    /// 安装时刻（Unix 秒）。用整数而不是 RFC3339 字符串，省掉一个日期库依赖。
    #[serde(rename = "installedAt", default)]
    pub installed_at: u64,
}

/// 插件是怎么装进来的。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallSource {
    /// 从 GitHub Releases 下载的预编译产物。
    Prebuilt,
    /// 本地 `cargo build` 编译出来的（默认路径）。
    #[default]
    BuildHost,
}

/// 当前 Unix 秒。取不到系统时间时返回 0（宁可时间戳不准，也不要让安装失败）。
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Index {
    /// 读缓存。文件不存在或解析失败都返回空索引 —— 缓存不是事实来源。
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut idx) => {
                idx.cache_version = CACHE_VERSION;
                idx
            }
            Err(_) => Self::default(),
        }
    }

    /// 写缓存（原子替换：先写临时文件再 rename）。
    pub fn save(&self, path: &Path) -> KitResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| crate::error::KitError::Registry(format!("序列化索引失败：{e}")))?;

        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 记一条。
    pub fn insert(&mut self, crate_name: &str, entry: IndexEntry) {
        self.plugins.insert(crate_name.to_string(), entry);
    }

    /// 删一条。
    pub fn remove(&mut self, crate_name: &str) -> Option<IndexEntry> {
        self.plugins.remove(crate_name)
    }

    /// 取一条。
    pub fn get(&self, crate_name: &str) -> Option<&IndexEntry> {
        self.plugins.get(crate_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(version: &str) -> IndexEntry {
        IndexEntry {
            version: version.to_string(),
            abi: Some(1),
            root: PathBuf::from("some").join("plugin"),
            source: InstallSource::BuildHost,
            installed_at: 1_767_225_600,
        }
    }

    #[test]
    fn insert_get_remove_round_trip() {
        let mut idx = Index::default();
        assert!(idx.get("a").is_none());

        idx.insert("a", entry("0.1.0"));
        assert_eq!(idx.get("a").unwrap().version, "0.1.0");

        let removed = idx.remove("a");
        assert_eq!(removed.unwrap().version, "0.1.0");
        assert!(idx.get("a").is_none());
        assert!(idx.remove("a").is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(".plugins.json");

        let mut idx = Index::default();
        idx.insert("myapp-plugin-foo", entry("0.2.0"));
        idx.save(&path).expect("应当能落盘");

        let back = Index::load(&path);
        assert_eq!(back.cache_version, CACHE_VERSION);
        let got = back.get("myapp-plugin-foo").unwrap();
        assert_eq!(got.version, "0.2.0");
        assert_eq!(got.abi, Some(1));
        assert_eq!(got.source, InstallSource::BuildHost);
        assert_eq!(got.installed_at, 1_767_225_600);
    }

    /// 缓存不是事实来源：文件不存在时给一个空索引，而不是报错。
    #[test]
    fn a_missing_file_yields_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let idx = Index::load(&dir.path().join("nope.json"));
        assert!(idx.plugins.is_empty());
        assert_eq!(idx.cache_version, CACHE_VERSION);
    }

    /// 同上：文件坏了也只当空的，不把整个 `list()` 拖垮。
    #[test]
    fn a_corrupt_file_yields_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".plugins.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let idx = Index::load(&path);
        assert!(idx.plugins.is_empty());
    }

    /// 旧缓存里 `source` 字段可能缺失 —— 要能读回并落到默认值。
    #[test]
    fn missing_optional_fields_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".plugins.json");
        std::fs::write(&path, r#"{"plugins":{"a":{"version":"0.1.0","root":"r"}}}"#).unwrap();

        let idx = Index::load(&path);
        let got = idx.get("a").unwrap();
        assert_eq!(got.source, InstallSource::BuildHost);
        assert_eq!(got.abi, None);
        assert_eq!(got.installed_at, 0);
    }

    /// 保存是"先写临时文件再 rename"，所以不该留下 `.tmp` 残骸。
    #[test]
    fn save_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".plugins.json");

        let mut idx = Index::default();
        idx.insert("a", entry("0.1.0"));
        idx.save(&path).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "留下了临时文件：{leftovers:?}");
    }

    #[test]
    fn now_unix_is_after_the_epoch() {
        assert!(now_unix() > 1_600_000_000);
    }
}
