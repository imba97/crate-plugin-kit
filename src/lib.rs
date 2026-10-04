//! # crate-plugin-kit
//!
//! 一个**基于 cargo 的插件系统**：从 crates.io 安装 cdylib 插件，并在运行时加载。
//!
//! 定位上对标 npm 生态的 [`npm-plugin-kit`](https://github.com/imba97/npm-plugin-kit)，
//! 把「插件安装 + 运行时加载」这套东西搬到 cargo / `dlopen` 这边。
//!
//! ## 它解决的核心问题
//!
//! `cargo install` 只认 **bin target**。一个 `crate-type = ["cdylib"]` 的 crate
//! 没有 bin，`cargo install` 会直接失败。所以 cdylib 插件**不能靠 `cargo install` 分发**。
//!
//! 本 crate 的做法是生成一个极小的 **wrapper 工程**，让插件本体的 crate 保持普通 rlib：
//!
//! ```text
//! 插件 crate（crates.io 上的普通 rlib）
//!     ↓  作为依赖
//! wrapper 工程（本 crate 生成，几十行，[lib] crate-type = ["cdylib"]）
//!     ↓  cargo build --release
//! libxxx.so / xxx.dll / libxxx.dylib
//!     ↓  拷到 <data-dir>/plugins/<crate>/
//! 运行时 dlopen
//! ```
//!
//! ## 快速上手
//!
//! ```no_run
//! use crate_plugin_kit::{CratePluginKit, KitConfig};
//!
//! # #[repr(C)]
//! # struct MyHostEntry { pub abi_version: u32, /* ... */ }
//! let mut cfg = KitConfig::new("myapp");
//! cfg.crate_prefix = "myapp-plugin-".into();
//! cfg.entry_symbol = b"myapp_plugin_entry_v1".to_vec();
//! cfg.contract_crate = "myapp-plugin".into();
//! cfg.wrapper_body = "myapp_plugin::export!({crate_ident}::create);\n".into();
//!
//! let kit = CratePluginKit::<MyHostEntry>::new(cfg)?;
//!
//! kit.install("foo", None)?;                   // 装 myapp-plugin-foo
//! let infos = kit.list()?;
//! let plugin = kit.load("myapp-plugin-foo")?;  // *const MyHostEntry
//! # Ok::<(), crate_plugin_kit::KitError>(())
//! ```
//!
//! ## 设计要点
//!
//! | 要点 | 说明 |
//! | ---- | ---- |
//! | **本 crate 不含任何宿主专有名字** | `myapp` / `myapp-plugin.toml` / 入口符号全都由 [`KitConfig`] 提供 |
//! | **泛型而非类型擦除** | [`CratePluginKit<T>`] 的 `load()` 返回 `*const T`（**瘦指针**），不存在 `dyn Trait` 的 fat pointer 互转 |
//! | **不做 ABI 校验** | 那是宿主契约 crate 的职责（它才知道 `T` 里有什么）。本 crate 只负责把符号取出来 |
//! | **全栈同步** | 没有 async runtime。装插件 = 起 `cargo build`、加载 = `dlopen`，都是同步的 |
//! | **build-host 是默认** | 它走 `cargo`，天然使用用户已配好的 registry / 镜像；prebuilt 从 GitHub 下载会绕开这些，所以只当加速项 |
//!
//! ## 与「宿主契约 crate」的分工
//!
//! ```text
//! crate-plugin-kit  ← 通用：装卸、列举、dlopen、crates.io 查询、文件锁
//!      ↑ 依赖
//! 宿主契约 crate    ← 宿主专有：ABI 结构体定义、abi_version 校验、
//!                     调用入口函数、跨边界内存所有权、catch_unwind 落点
//!      ↑ 依赖
//! 宿主程序 / 插件
//! ```
//!
//! 本 crate **不知道也不该知道** `T` 里有什么。知道得越多，就越不通用。

#![deny(missing_docs)]
#![warn(clippy::all)]

pub mod cache;
pub mod config;
pub mod error;
pub mod install;
pub mod loader;
pub mod lock;
pub mod manifest;
pub mod panic;
pub mod registry;
pub mod store;

pub use config::{KitConfig, KitPaths, DEFAULT_LOCK_TIMEOUT};
pub use error::{KitError, KitResult};
pub use loader::{find_library, library_candidates, LoadedPlugin};
pub use manifest::{LibSection, PluginManifest, PluginSection};
pub use registry::{CrateInfo, CrateSummary, Registry};
pub use store::{CratePluginKit, PluginInfo};

/// 编译期注入的 target triple，例如 `x86_64-pc-windows-msvc`。
///
/// prebuilt 资产名按它来拼。见 `build.rs`。
pub const TARGET_TRIPLE: &str = env!("CPK_TARGET");

/// 本 crate 的版本。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
