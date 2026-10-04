//! 错误类型。
//!
//! 刻意**不**把 `anyhow::Error` 用在公开 API 上：这个 crate 会被宿主在靠近
//! `dlopen` 边界的地方使用，一个可枚举、可匹配的错误类型比类型擦除的 error
//! 更好排查。内部（`install` 模块）用 anyhow 图省事，出口处再收进来。

use std::path::PathBuf;

/// kit 的错误。
///
/// 变体分两类：
///
/// - **调用方能处理的**：[`KitError::NotInstalled`] / [`KitError::AlreadyInstalled`] /
///   [`KitError::PluginInUse`] / [`KitError::CargoNotFound`] —— 调用方通常据此换一条路或提示用户；
/// - **环境/数据坏了的**：其余。基本只能上报。
#[derive(Debug, thiserror::Error)]
pub enum KitError {
    /// 要卸载/更新的插件没装。
    #[error("插件 {name} 未安装")]
    NotInstalled {
        /// 完整 crate 名。
        name: String,
    },

    /// 要安装的插件已经在了。
    ///
    /// 这是刻意的：`install` 不覆盖已有的安装，替换请走 `update`。
    #[error("插件 {name} 的目录已存在：{path}")]
    AlreadyInstalled {
        /// 完整 crate 名。
        name: String,
        /// 已存在的目录。
        path: PathBuf,
    },

    /// 插件的动态库仍在本进程里加载着，删不掉。
    ///
    /// Windows 上正在 `dlopen` 的 `.dll` 无法删除；Linux/macOS 上删得掉但空间不回收。
    /// 与其赌，不如让用户换一个干净的进程。
    #[error(
        "插件 {name} 的动态库仍在本进程中加载，无法删除。\
         请在一个新终端里重试（新进程不会加载它），或退出当前会话后再执行"
    )]
    PluginInUse {
        /// 完整 crate 名。
        name: String,
    },

    /// 读 manifest 文件失败。
    #[error("读 manifest 失败（{path}）：{source}")]
    ManifestRead {
        /// manifest 路径。
        path: PathBuf,
        /// 底层 IO 错误。
        #[source]
        source: std::io::Error,
    },

    /// manifest 不是合法 TOML，或字段类型不对。
    #[error("解析 manifest 失败（{path}）：{source}")]
    ManifestParse {
        /// manifest 路径。
        path: PathBuf,
        /// 底层 TOML 错误。
        #[source]
        source: Box<toml::de::Error>,
    },

    /// manifest 里某个必填字段缺失或为空。
    #[error("manifest 缺少字段 `{field}`（{path}）")]
    ManifestMissingField {
        /// 字段名，例如 `plugin.name`。
        field: String,
        /// manifest 路径。
        path: PathBuf,
    },

    /// 安装目录里找不到符合命名规则的动态库。
    #[error("找不到插件 {name} 的动态库。试过：{tried}")]
    LibraryNotFound {
        /// 文件名主干。
        name: String,
        /// 尝试过的文件名，逗号分隔。
        tried: String,
    },

    /// `dlopen` 本身失败（文件损坏、架构不符、缺依赖库）。
    #[error("加载动态库失败（{path}）：{source}")]
    LibraryLoad {
        /// 动态库路径。
        path: PathBuf,
        /// 底层 libloading 错误。
        #[source]
        source: Box<libloading::Error>,
    },

    /// 动态库加载成功，但没有我们找的那个导出符号。
    ///
    /// 常见于"装错了东西"—— 比如把一个普通库放进了插件目录。
    #[error("动态库里没有导出符号 `{symbol}`")]
    SymbolMissing {
        /// 缺失的符号名。
        symbol: String,
    },

    /// 插件的入口函数返回了空指针。
    #[error("插件的入口函数返回了空指针")]
    NullEntry,

    /// 等安装锁超时。
    #[error("等待文件锁超时（{path}，等了 {secs} 秒）")]
    LockTimeout {
        /// 锁文件路径。
        path: PathBuf,
        /// 等待了多少秒。
        secs: u64,
    },

    /// 锁文件本身操作失败。
    #[error("文件锁操作失败（{path}）：{source}")]
    LockIo {
        /// 锁文件路径。
        path: PathBuf,
        /// 底层 IO 错误。
        #[source]
        source: std::io::Error,
    },

    /// `PATH` 里没有 `cargo`，build-host 装不了。
    ///
    /// 调用方可以据此提示用户"装个 Rust 工具链"，或改走 prebuilt。
    #[error("PATH 里找不到 `cargo`；build-host 安装需要 Rust 工具链")]
    CargoNotFound,

    /// `cargo build` 失败。stderr 已经透传给用户了（stdio 继承）。
    #[error("build-host 编译失败（退出码 {code:?}）")]
    BuildFailed {
        /// 进程退出码。被信号杀掉时为 `None`。
        code: Option<i32>,
    },

    /// 编译成功了，但产物里找不到 cdylib。
    #[error("编译产物里找不到 cdylib（找的是 {stem}，目录 {dir}）")]
    BuildArtifactMissing {
        /// 期望的文件名主干。
        stem: String,
        /// 找过的目录。
        dir: String,
    },

    /// 网络请求失败。字符串里带 URL 与状态码。
    #[error("网络请求失败：{0}")]
    Http(String),

    /// registry 返回了能拿到、但不符合预期的内容。
    #[error("registry 返回了意外的响应：{0}")]
    Registry(String),

    /// 跨边界调用时插件 panic 了（宿主侧 `catch_unwind` 捕获）。
    #[error("插件在 {what} 时 panic（已由 catch_unwind 捕获）")]
    PluginPanicked {
        /// 哪个调用点，例如 `"entry()"`。
        what: String,
    },

    /// 既没有 HOME 也没有等价的系统目录，数据目录定不下来。
    #[error("宿主应用的数据目录无法确定（既没有 HOME 也没有系统等价物）")]
    NoDataDir,

    /// IO 错误。
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// 本 crate 的 `Result` 别名。
pub type KitResult<T> = std::result::Result<T, KitError>;
