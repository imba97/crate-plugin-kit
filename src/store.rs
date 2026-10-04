//! [`CratePluginKit`]：本 crate 的门面。
//!
//! # 泛型参数 `T`
//!
//! `T` 是**宿主自己的 `#[repr(C)]` 入口结构体**。本 crate 不使用它的任何字段 ——
//! 它只出现在 [`CratePluginKit::load`] 的返回类型里，用来把 `dlopen` 出来的符号
//! 转成一个**瘦指针** `*const T`。
//!
//! 这样就没有"类型擦除 → 还原"那一层：不存在两个 vtable 之间的 `fat pointer` 互转，
//! 也就没有那类 UB。
//!
//! # 线程安全
//!
//! [`CratePluginKit`] 内部有一个 `RefCell` 记录"本进程加载过哪些插件"（用于
//! [`CratePluginKit::uninstall`] 拒绝删除正在使用的库）。因此它**不是 `Sync`** ——
//! 一个进程里建一个、串行用即可（CLI 就是这种用法）。需要并发的话，把 kit 放在
//! 各线程自己的作用域里。

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use crate::cache::{now_unix, Index, IndexEntry, InstallSource};
use crate::config::{KitConfig, KitPaths};
use crate::error::{KitError, KitResult};
use crate::install::{self, build_host, prebuilt, Installed};
use crate::loader::{self, LoadedPlugin};
use crate::lock::FileLock;
use crate::manifest::PluginManifest;
use crate::registry::{CrateInfo, CrateSummary, Registry};

/// 一个已安装插件的概览。
#[derive(Debug, Clone)]
pub struct PluginInfo {
    /// 插件自报名（manifest 的 `plugin.name`）。
    pub name: String,
    /// 完整 crate 名，如 `bmux-plugin-cargo`。
    pub crate_name: String,
    /// 版本。
    pub version: String,
    /// 生态分组（宿主自用字段，可能为空）。
    pub family: Option<String>,
    /// ABI 版本（宿主自用字段，可能为空）。
    pub abi: Option<u32>,
    /// 安装目录。
    pub dir: PathBuf,
    /// 怎么装进来的。
    pub source: InstallSource,
}

/// 通用插件管理库。
///
/// 建一个、串行用。
pub struct CratePluginKit<T> {
    cfg: KitConfig,
    paths: KitPaths,
    registry: Registry,
    /// 本进程已经 `dlopen` 过的 crate 名。`uninstall` 拿它挡"删正在用的库"。
    loaded: RefCell<BTreeSet<String>>,
    /// `fn() -> T` 而不是 `T`：不假装拥有 `T`，也不要求 `T: Send/Sync`。
    _host: PhantomData<fn() -> T>,
}

impl<T> CratePluginKit<T> {
    /// 按配置建一个 kit 实例。会创建数据目录。
    pub fn new(cfg: KitConfig) -> KitResult<Self> {
        let paths = cfg.paths()?;
        std::fs::create_dir_all(&paths.plugins)?;
        std::fs::create_dir_all(&paths.build)?;

        let registry = Registry::new(&cfg);

        Ok(Self {
            cfg,
            paths,
            registry,
            loaded: RefCell::new(BTreeSet::new()),
            _host: PhantomData,
        })
    }

    /// 配置。
    pub fn config(&self) -> &KitConfig {
        &self.cfg
    }

    /// 各项路径。
    pub fn paths(&self) -> &KitPaths {
        &self.paths
    }

    /// registry 客户端。
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    // ---- 安装 / 卸载 ------------------------------------------------------

    /// 安装一个插件。
    ///
    /// `name` 可以写短名（`"cargo"`，会自动补 [`KitConfig::crate_prefix`]），
    /// 也可以写完整 crate 名。`version` 给 `None` 就去 crates.io 查最新版。
    ///
    /// # Errors
    ///
    /// - [`KitError::AlreadyInstalled`]：已经装过了。用 [`Self::update`] 替换。
    pub fn install(&self, name: &str, version: Option<&str>) -> KitResult<Installed> {
        let crate_name = self.cfg.normalize_crate_name(name);

        let _lock = self.lock()?;

        let dir = self.paths.plugin_dir(&crate_name);
        if dir.is_dir() {
            return Err(KitError::AlreadyInstalled {
                name: crate_name,
                path: dir,
            });
        }

        let version = self.resolve_version(&crate_name, version)?;
        let installed = self.install_locked(&crate_name, &version)?;
        self.record(&installed)?;
        Ok(installed)
    }

    /// 更新（或重装）一个插件。已存在就替换，不存在就当安装。
    pub fn update(&self, name: &str, version: Option<&str>) -> KitResult<Installed> {
        let crate_name = self.cfg.normalize_crate_name(name);

        let _lock = self.lock()?;

        let version = self.resolve_version(&crate_name, version)?;
        let installed = self.install_locked(&crate_name, &version)?;
        self.record(&installed)?;
        Ok(installed)
    }

    /// 卸载一个插件。
    ///
    /// # Errors
    ///
    /// - [`KitError::NotInstalled`]：本来就没装。
    /// - [`KitError::PluginPanicked`] 之外的一个"正在使用"错误：本进程加载过它的库。
    ///   Windows 上正在 `dlopen` 的 `.dll` 删不掉；Linux 上删得掉但空间不回收。
    ///   与其赌，不如让用户换一个干净的进程。
    pub fn uninstall(&self, name: &str) -> KitResult<()> {
        let crate_name = self.cfg.normalize_crate_name(name);

        let _lock = self.lock()?;

        let dir = self.paths.plugin_dir(&crate_name);
        if !dir.is_dir() {
            return Err(KitError::NotInstalled { name: crate_name });
        }

        if self.loaded.borrow().contains(&crate_name) {
            return Err(KitError::PluginInUse {
                name: crate_name.clone(),
            });
        }

        std::fs::remove_dir_all(&dir)?;

        let mut idx = Index::load(&self.paths.index_file);
        idx.remove(&crate_name);
        idx.save(&self.paths.index_file)?;

        Ok(())
    }

    // ---- 查询 ------------------------------------------------------------

    /// 列出已安装的插件。
    ///
    /// **以磁盘上的 manifest 为准**，`.plugins.json` 只用来补充"来源 / 安装时间"
    /// 这类 manifest 里没有的信息。所以缓存丢了也不会漏插件。
    pub fn list(&self) -> KitResult<Vec<PluginInfo>> {
        let idx = Index::load(&self.paths.index_file);
        let mut out = Vec::new();

        let entries = match std::fs::read_dir(&self.paths.plugins) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };

        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let crate_name = entry.file_name().to_string_lossy().into_owned();

            // 目录在但 manifest 没了/坏了 —— 跳过，不要因为一个坏插件让整个 list 失败
            let manifest_path = entry.path().join(&self.cfg.manifest_name);
            let Ok(manifest) = PluginManifest::read(&manifest_path) else {
                continue;
            };

            let source = idx.get(&crate_name).map(|e| e.source).unwrap_or_default();

            out.push(PluginInfo {
                name: manifest.plugin.name.clone(),
                crate_name,
                version: manifest.plugin.version.clone(),
                family: manifest.plugin.family.clone(),
                abi: manifest.plugin.abi,
                dir: entry.path(),
                source,
            });
        }

        out.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));
        Ok(out)
    }

    /// 读某个插件的 manifest。
    pub fn manifest_of(&self, name: &str) -> KitResult<PluginManifest> {
        let crate_name = self.cfg.normalize_crate_name(name);
        let path = self
            .paths
            .manifest_path(&crate_name, &self.cfg.manifest_name);
        PluginManifest::read(&path)
    }

    /// 把 `name` 下的相对路径解析成绝对路径。
    pub fn resolve(&self, name: &str, relative: impl AsRef<Path>) -> PathBuf {
        let crate_name = self.cfg.normalize_crate_name(name);
        self.paths.plugin_dir(&crate_name).join(relative)
    }

    // ---- 加载 ------------------------------------------------------------

    /// 加载插件，拿到 `*const T`。
    ///
    /// 加载过之后 [`Self::uninstall`] 会拒绝删除它，直到进程退出。
    pub fn load(&self, name: &str) -> KitResult<LoadedPlugin<T>> {
        let crate_name = self.cfg.normalize_crate_name(name);

        let manifest = self.manifest_of(&crate_name)?;
        let dir = self.paths.plugin_dir(&crate_name);
        let stem = manifest.effective_lib_stem(&self.cfg, &crate_name);
        let lib_path = loader::find_library(&dir, &stem)?;

        // SAFETY: `lib_path` 是本 kit 装出来的；`T` 与它导出的结构体布局是否一致
        // 由宿主的 abi_version 字段兜底 —— 那是宿主契约 crate 的职责，不是本 crate 的。
        let plugin = unsafe { loader::open::<T>(&lib_path, &self.cfg.entry_symbol)? };

        self.loaded.borrow_mut().insert(crate_name);
        Ok(plugin)
    }

    // ---- registry --------------------------------------------------------

    /// 搜 crate。
    pub fn search(&self, keyword: &str, limit: usize) -> KitResult<Vec<CrateSummary>> {
        self.registry.search(keyword, limit)
    }

    /// 按精确名查 crate。
    pub fn view(&self, name: &str) -> KitResult<Option<CrateInfo>> {
        let crate_name = self.cfg.normalize_crate_name(name);
        self.registry.view(&crate_name)
    }

    // ---- 内部 ------------------------------------------------------------

    fn lock(&self) -> KitResult<FileLock> {
        FileLock::acquire(&self.paths.lock_file, self.cfg.lock_timeout)
    }

    /// 没给版本就查最新版。
    fn resolve_version(&self, crate_name: &str, version: Option<&str>) -> KitResult<String> {
        if let Some(v) = version {
            return Ok(v.to_string());
        }
        match self.registry.view(crate_name)? {
            Some(info) if !info.version.is_empty() => Ok(info.version),
            Some(_) => Err(KitError::Registry(format!(
                "{crate_name} 在 registry 上没有可用版本"
            ))),
            None => Err(KitError::Registry(format!(
                "registry 上找不到 {crate_name}"
            ))),
        }
    }

    /// 真正的安装动作。调用方必须已经持锁。
    fn install_locked(&self, crate_name: &str, version: &str) -> KitResult<Installed> {
        // 先把旧目录清掉（update 路径）
        install::remove_dir_if_exists(&self.paths.plugin_dir(crate_name))?;

        if self.cfg.prefer_prebuilt {
            match prebuilt::try_install(&self.cfg, &self.paths, &self.registry, crate_name, version)
            {
                Ok(Some(installed)) => return Ok(installed),
                // 没有 prebuilt 资产 —— 这正是回落 build-host 的信号
                Ok(None) => {}
                // 网络坏 / 资产损坏：也回落。build-host 走的是 cargo，
                // 跟 GitHub 是两条路，很可能反而能通。
                Err(_) => {}
            }
        }

        build_host::install(&self.cfg, &self.paths, crate_name, version)
    }

    fn record(&self, installed: &Installed) -> KitResult<()> {
        let mut idx = Index::load(&self.paths.index_file);

        let abi = PluginManifest::read(
            &self
                .paths
                .manifest_path(&installed.crate_name, &self.cfg.manifest_name),
        )
        .ok()
        .and_then(|m| m.plugin.abi);

        idx.insert(
            &installed.crate_name,
            IndexEntry {
                version: installed.version.clone(),
                abi,
                root: installed.dir.clone(),
                source: installed.source,
                installed_at: now_unix(),
            },
        );
        idx.save(&self.paths.index_file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 测试用的宿主 ABI 结构体。
    ///
    /// 内容是任意的 —— 这个 crate 从不读 `T` 的字段，它只把符号 cast 成 `*const T`。
    #[repr(C)]
    struct FakeEntry {
        abi_version: u32,
    }

    const MANIFEST: &str = r#"
[plugin]
name    = "foo"
version = "0.1.0"
abi     = 1
family  = "node"

[detect]
strong = ["fake.lock"]
"#;

    fn kit(root: &Path) -> CratePluginKit<FakeEntry> {
        let cfg = KitConfig::new("myapp")
            .with_data_dir(root)
            .with_lock_timeout(Duration::from_millis(500));
        CratePluginKit::new(cfg).expect("应当能建起来")
    }

    /// 手工放一个"已安装"的插件目录，省掉编译步骤。
    fn place_plugin(root: &Path, crate_name: &str) -> PathBuf {
        let dir = root.join("plugins").join(crate_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("myapp-plugin.toml"), MANIFEST).unwrap();
        dir
    }

    #[test]
    fn new_creates_the_data_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");

        let k = kit(&root);
        assert!(k.paths().plugins.is_dir(), "plugins 目录应当被建出来");
        assert!(k.paths().build.is_dir(), "build 目录应当被建出来");
    }

    #[test]
    fn a_fresh_store_lists_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let k = kit(&tmp.path().join("store"));
        assert!(k.list().unwrap().is_empty());
    }

    /// 目录根本不存在时，`list` 也该给空列表而不是报错。
    #[test]
    fn list_tolerates_a_missing_plugins_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        std::fs::remove_dir_all(&root).unwrap();
        assert!(k.list().unwrap().is_empty());
    }

    /// **关键行为**：磁盘上的 manifest 才是事实来源，不是 `.plugins.json`。
    /// 手工放进去的插件（缓存里没有记录）也必须被列出来。
    #[test]
    fn list_reads_manifests_from_disk_not_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        place_plugin(&root, "myapp-plugin-foo");

        let all = k.list().unwrap();
        assert_eq!(all.len(), 1, "缓存是空的，但磁盘上有插件");
        let info = &all[0];
        assert_eq!(info.name, "foo");
        assert_eq!(info.crate_name, "myapp-plugin-foo");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.abi, Some(1));
        assert_eq!(info.family.as_deref(), Some("node"));
        // 缓存里没有记录 → 落到默认来源
        assert_eq!(info.source, InstallSource::BuildHost);
    }

    /// 坏掉的 manifest 不该让整个 `list()` 失败 —— 跳过它就好。
    #[test]
    fn list_skips_a_directory_with_a_broken_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        place_plugin(&root, "myapp-plugin-good");
        let bad = root.join("plugins").join("myapp-plugin-bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("myapp-plugin.toml"), "this is not toml").unwrap();

        // 连 manifest 都没有的目录
        let empty = root.join("plugins").join("myapp-plugin-empty");
        std::fs::create_dir_all(&empty).unwrap();

        let all = k.list().unwrap();
        assert_eq!(all.len(), 1, "只应当列出好的那个：{all:?}");
        assert_eq!(all[0].crate_name, "myapp-plugin-good");
    }

    #[test]
    fn list_is_sorted_by_crate_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        for name in ["myapp-plugin-c", "myapp-plugin-a", "myapp-plugin-b"] {
            place_plugin(&root, name);
        }

        let names: Vec<_> = k
            .list()
            .unwrap()
            .into_iter()
            .map(|i| i.crate_name)
            .collect();
        assert_eq!(
            names,
            vec!["myapp-plugin-a", "myapp-plugin-b", "myapp-plugin-c"]
        );
    }

    #[test]
    fn manifest_of_reads_the_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);
        place_plugin(&root, "myapp-plugin-foo");

        let m = k.manifest_of("foo").expect("短名应当也能用");
        assert_eq!(m.plugin.name, "foo");
        assert!(m.extra.contains_key("detect"), "宿主的段要保留");
    }

    #[test]
    fn manifest_of_reports_a_missing_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let k = kit(&tmp.path().join("store"));
        assert!(matches!(
            k.manifest_of("nope"),
            Err(KitError::ManifestRead { .. })
        ));
    }

    #[test]
    fn uninstall_reports_a_plugin_that_is_not_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let k = kit(&tmp.path().join("store"));

        assert!(matches!(
            k.uninstall("nope"),
            Err(KitError::NotInstalled { .. })
        ));
    }

    #[test]
    fn uninstall_removes_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);
        let dir = place_plugin(&root, "myapp-plugin-foo");

        assert!(dir.is_dir());
        k.uninstall("foo").expect("应当能卸掉");
        assert!(!dir.exists(), "目录应当被删掉");
        assert!(k.list().unwrap().is_empty());
    }

    /// `install` 不覆盖已有安装 —— 替换走 `update`。
    ///
    /// 这个检查发生在取锁之后、任何网络/编译之前，所以测试不需要联网。
    #[test]
    fn install_refuses_when_already_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);
        place_plugin(&root, "myapp-plugin-foo");

        match k.install("foo", Some("0.1.0")) {
            Err(KitError::AlreadyInstalled { name, .. }) => {
                assert_eq!(name, "myapp-plugin-foo")
            }
            other => panic!("期望 AlreadyInstalled，得到 {other:?}"),
        }
    }

    #[test]
    fn resolve_points_into_the_plugin_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let k = kit(&root);

        assert_eq!(
            k.resolve("foo", "extra.txt"),
            root.join("plugins")
                .join("myapp-plugin-foo")
                .join("extra.txt")
        );
        // 短名与全名要落到同一个地方
        assert_eq!(k.resolve("foo", "x"), k.resolve("myapp-plugin-foo", "x"));
    }

    #[test]
    fn config_is_exposed() {
        let tmp = tempfile::tempdir().unwrap();
        let k = kit(&tmp.path().join("store"));
        assert_eq!(k.config().id, "myapp");
        assert_eq!(k.config().crate_prefix, "myapp-plugin-");
    }
}
