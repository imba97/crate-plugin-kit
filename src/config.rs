//! [`KitConfig`]：把一个 kit 实例里所有跟宿主应用相关的名字参数化。
//!
//! **本 crate 里没有任何一处硬编码具体宿主应用的名字。**
//! 所有 `bmux` / `bmux-plugin.toml` / `bmux_plugin_entry_v1` 之类的东西，
//! 都由这里提供。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::{KitError, KitResult};

/// 默认的锁等待时间。
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// 一个 kit 实例的全部可配置项。
#[derive(Debug, Clone)]
pub struct KitConfig {
    /// 应用 id。决定数据目录 `~/.{id}`，也用作默认的 HTTP User-Agent 一部分。
    pub id: String,

    /// 插件 manifest 的文件名，如 `"bmux-plugin.toml"`。
    ///
    /// manifest 的 `[plugin]` / `[lib]` 两段由本 crate 定义（见 [`crate::manifest`]）。
    /// 宿主可以往同一个文件里追加自己的段（例如 bmux 的 `[detect]`），本 crate 会原样忽略。
    pub manifest_name: String,

    /// 插件 crate 名前缀，如 `"bmux-plugin-"`。
    ///
    /// `install("cargo")` 会先补成 `"bmux-plugin-cargo"`；已经带前缀的原样使用。
    pub crate_prefix: String,

    /// cdylib 导出的入口符号，如 `b"bmux_plugin_entry_v1"`。
    pub entry_symbol: Vec<u8>,

    /// cdylib 文件名主干前缀，如 `"bmux_plugin_"`。
    ///
    /// 与「crate 名去掉前缀」拼接后，再加平台扩展名，得到实际文件名。
    /// 例：`bmux-plugin-cargo` → `bmux_plugin_` + `cargo` → `libbmux_plugin_cargo.so`。
    pub lib_stem_prefix: String,

    /// 宿主的**契约 crate** 名，如 `"bmux-plugin"`。生成的 wrapper 工程要依赖它。
    pub contract_crate: String,

    /// 契约 crate 的版本要求，如 `"0.1"`。
    pub contract_version: String,

    /// 生成的 wrapper 工程使用的 edition。
    pub wrapper_edition: String,

    /// wrapper 工程 `src/lib.rs` 的内容模板。
    ///
    /// `{crate_ident}` 会被替换成插件 crate 的 ident（`-` 换成 `_`）。
    /// 例（bmux）：`"bmux_plugin::export!({crate_ident}::create);\n"`
    pub wrapper_body: String,

    /// 数据目录覆盖。`None` = 用 `~/.{id}`。
    pub data_dir: Option<PathBuf>,

    /// 等待安装锁的超时时间。
    pub lock_timeout: Duration,

    /// 安装时是否优先尝试 prebuilt 产物。
    pub prefer_prebuilt: bool,

    /// 覆盖 target triple。`None` = 用编译期注入的 `TARGET`（见 `build.rs`）。
    pub target_triple: Option<String>,

    /// crates.io 索引地址。可以指向镜像。
    pub registry: String,

    /// 本地路径覆盖：crate 名 → 本地目录。
    ///
    /// 生成 wrapper 工程时会写成它的 `[patch.crates-io]`。**开发期用** ——
    /// wrapper 住在 `~/.{id}/build/` 下，读不到宿主项目里的 `.cargo/config.toml`，
    /// 所以本地联调必须在这里显式指路。生产环境留空。
    pub local_overrides: BTreeMap<String, PathBuf>,
}

impl KitConfig {
    /// 用最少的信息起一个配置，其余字段给保守默认值。
    ///
    /// 默认值全部由 `id` 推导，并且**互相自洽** —— 例如 `id = "myapp"` 会得到：
    ///
    /// | 字段 | 值 |
    /// | ---- | -- |
    /// | `manifest_name` | `myapp-plugin.toml` |
    /// | `crate_prefix` | `myapp-plugin-` |
    /// | `lib_stem_prefix` | `myapp_plugin_` |
    /// | `entry_symbol` | `myapp_plugin_entry_v1` |
    /// | `contract_crate` | `myapp-plugin` |
    ///
    /// 仍然要自己填的是 `wrapper_body`（它引用的路径来自宿主的契约 crate）和
    /// `contract_version`。
    ///
    /// `entry_symbol` 不需要以 NUL 结尾 —— `libloading` 会自己补。
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        let snake = id.replace('-', "_");
        Self {
            manifest_name: format!("{id}-plugin.toml"),
            crate_prefix: format!("{id}-plugin-"),
            entry_symbol: format!("{snake}_plugin_entry_v1").into_bytes(),
            lib_stem_prefix: format!("{snake}_plugin_"),
            contract_crate: format!("{id}-plugin"),
            contract_version: "0.1".to_string(),
            wrapper_edition: "2021".to_string(),
            wrapper_body: format!("{snake}_plugin::export!({{crate_ident}}::create);\n"),
            data_dir: None,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            prefer_prebuilt: true,
            target_triple: None,
            registry: "https://crates.io".to_string(),
            local_overrides: BTreeMap::new(),
            id,
        }
    }

    /// 覆盖数据目录。
    pub fn with_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(dir.into());
        self
    }

    /// 覆盖锁等待时间。
    pub fn with_lock_timeout(mut self, t: Duration) -> Self {
        self.lock_timeout = t;
        self
    }

    /// 本次构建的 target triple。优先用配置里的覆盖值。
    pub fn effective_target(&self) -> &str {
        self.target_triple
            .as_deref()
            .unwrap_or(crate::TARGET_TRIPLE)
    }

    /// 把 `install()` 收到的名字规范成完整 crate 名。
    ///
    /// 已带前缀的原样返回；否则补前缀。
    pub fn normalize_crate_name(&self, name: &str) -> String {
        if name.starts_with(&self.crate_prefix) {
            name.to_string()
        } else {
            format!("{}{}", self.crate_prefix, name)
        }
    }

    /// 由 crate 名推出 cdylib 文件名主干（不含 `lib` 前缀与扩展名）。
    ///
    /// `bmux-plugin-cargo` → `bmux_plugin_cargo`
    pub fn lib_stem(&self, crate_name: &str) -> String {
        let tail = crate_name
            .strip_prefix(&self.crate_prefix)
            .unwrap_or(crate_name);
        format!("{}{}", self.lib_stem_prefix, tail.replace('-', "_"))
    }

    /// 解析出各项路径。会按需创建目录。
    pub fn paths(&self) -> KitResult<KitPaths> {
        let root = match &self.data_dir {
            Some(d) => d.clone(),
            None => default_data_dir(&self.id)?,
        };
        Ok(KitPaths {
            plugins: root.join("plugins"),
            build: root.join("build"),
            lock_file: root.join(".lock"),
            index_file: root.join("plugins").join(".plugins.json"),
            root,
        })
    }
}

/// 由 `directories` 决定的默认数据目录 `~/.{id}`。
fn default_data_dir(id: &str) -> KitResult<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or(KitError::NoDataDir)?;
    Ok(home.join(format!(".{id}")))
}

/// kit 用到的所有路径。
#[derive(Debug, Clone)]
pub struct KitPaths {
    /// `<data-dir>` 本身。
    pub root: PathBuf,
    /// 已安装插件的目录，`<root>/plugins`。
    pub plugins: PathBuf,
    /// build-host 脚手架目录，`<root>/build`。
    pub build: PathBuf,
    /// 安装排他锁文件，`<root>/.lock`。
    pub lock_file: PathBuf,
    /// 安装记录缓存，`<root>/plugins/.plugins.json`。
    pub index_file: PathBuf,
}

impl KitPaths {
    /// 某个插件的安装目录，`<plugins>/<crate_name>`。
    pub fn plugin_dir(&self, crate_name: &str) -> PathBuf {
        self.plugins.join(crate_name)
    }

    /// 某个插件的 manifest 路径。
    pub fn manifest_path(&self, crate_name: &str, manifest_name: &str) -> PathBuf {
        self.plugin_dir(crate_name).join(manifest_name)
    }

    /// 某个插件的 build-host 脚手架目录。
    pub fn build_dir(&self, crate_name: &str) -> PathBuf {
        self.build.join(crate_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> KitConfig {
        KitConfig::new("myapp")
    }

    #[test]
    fn new_derives_names_from_the_id() {
        let c = cfg();
        assert_eq!(c.id, "myapp");
        assert_eq!(c.manifest_name, "myapp-plugin.toml");
        assert_eq!(c.crate_prefix, "myapp-plugin-");
        assert_eq!(c.lib_stem_prefix, "myapp_plugin_");
        assert_eq!(c.contract_crate, "myapp-plugin");
    }

    #[test]
    fn normalizes_short_and_full_crate_names() {
        let c = cfg();
        assert_eq!(c.normalize_crate_name("foo"), "myapp-plugin-foo");
        // 已经带前缀的原样返回 —— 否则会变成 myapp-plugin-myapp-plugin-foo
        assert_eq!(
            c.normalize_crate_name("myapp-plugin-foo"),
            "myapp-plugin-foo"
        );
    }

    #[test]
    fn derives_lib_stem_from_crate_name() {
        let c = cfg();
        assert_eq!(c.lib_stem("myapp-plugin-foo"), "myapp_plugin_foo");
        // 短横线要换成下划线
        assert_eq!(c.lib_stem("myapp-plugin-a-b"), "myapp_plugin_a_b");
    }

    #[test]
    fn target_falls_back_to_compile_time_triple() {
        let c = cfg();
        assert_eq!(c.effective_target(), crate::TARGET_TRIPLE);

        let mut overridden = cfg();
        overridden.target_triple = Some("aarch64-apple-darwin".to_string());
        assert_eq!(overridden.effective_target(), "aarch64-apple-darwin");
    }

    #[test]
    fn paths_honour_the_data_dir_override() {
        let root = PathBuf::from("some").join("dir");
        let c = cfg().with_data_dir(&root);
        let p = c.paths().expect("override 时不该碰 HOME");

        assert_eq!(p.root, root);
        assert_eq!(p.plugins, root.join("plugins"));
        assert_eq!(p.build, root.join("build"));
        assert_eq!(p.lock_file, root.join(".lock"));
        assert_eq!(p.index_file, root.join("plugins").join(".plugins.json"));
    }

    #[test]
    fn plugin_paths_sit_under_the_plugins_dir() {
        let root = PathBuf::from("some").join("dir");
        let c = cfg().with_data_dir(&root);
        let p = c.paths().unwrap();

        assert_eq!(
            p.plugin_dir("myapp-plugin-foo"),
            root.join("plugins").join("myapp-plugin-foo")
        );
        assert_eq!(
            p.build_dir("myapp-plugin-foo"),
            root.join("build").join("myapp-plugin-foo")
        );
        assert_eq!(
            p.manifest_path("myapp-plugin-foo", "myapp-plugin.toml"),
            root.join("plugins")
                .join("myapp-plugin-foo")
                .join("myapp-plugin.toml")
        );
    }

    #[test]
    fn with_lock_timeout_replaces_the_default() {
        let c = cfg().with_lock_timeout(Duration::from_millis(5));
        assert_eq!(c.lock_timeout, Duration::from_millis(5));
    }
}
