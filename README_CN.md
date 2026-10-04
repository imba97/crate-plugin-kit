# crate-plugin-kit

[![Release](https://github.com/imba97/crate-plugin-kit/actions/workflows/release.yaml/badge.svg)](https://github.com/imba97/crate-plugin-kit/actions/workflows/release.yaml)
[![crates.io](https://img.shields.io/crates/v/crate-plugin-kit.svg)](https://crates.io/crates/crate-plugin-kit)
[![docs.rs](https://img.shields.io/docsrs/crate-plugin-kit)](https://docs.rs/crate-plugin-kit)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](#许可)
[![MSRV](https://img.shields.io/badge/rust-1.88%2B-blue.svg)](#环境要求)

> English: [README.md](README.md)

一个**基于 cargo 的插件系统**：从 crates.io 安装 `cdylib` 插件，并在运行时加载。
它是 npm 生态 [`npm-plugin-kit`](https://github.com/imba97/npm-plugin-kit) 的 Rust 对应物 ——
把「装一个插件，然后加载它」这件事搬到 cargo 上。

```console
$ cargo install my-plugin          # 这个插件是个 cdylib crate
error: there is nothing to install

$ # crate-plugin-kit 改成这么做：
  myapp-plugin-foo  0.1.0
    wrapper   ~/.myapp/build/myapp-plugin-foo/          cargo build --release
    library   ~/.myapp/plugins/myapp-plugin-foo/libmyapp_plugin_foo.so
    manifest  ~/.myapp/plugins/myapp-plugin-foo/myapp-plugin.toml
```

## 特性

- 🔌 **运行期加载，编译期零耦合** —— 插件是 `cdylib`，按需 `dlopen`，没有任何东西链接进你的二进制。
- 🧩 **泛型而非类型擦除** —— `load()` 返回的是瘦指针 `*const T`，不是 `dyn Trait`。
  没有 fat pointer 转换，边界上也就没有 UB。
- 🏷️ **所有宿主专有名字都是配置项** —— 应用 id、manifest 名、crate 前缀、入口符号、wrapper 内容。
  这个库一个都不写死。
- 📦 **直接从 crates.io 装** —— 一次调用，不用手工下载或 vendor。
- 🔨 **默认走 build-host** —— 安装走 `cargo build`，所以你**已经配好的 registry / 镜像自动生效**。
- ⚡ **prebuilt 加速** —— 可选地从 GitHub Releases 拉预编译产物，任何一步失败都回落到本地编译。
- 📄 **manifest 跟着代码走** —— 插件的元信息放在它自己仓库根目录，安装时拷贝，**永远不会和实现脱节**。
- 🔒 **跨进程安装锁** —— 两个终端同时操作也不会写坏插件库。
- 🚫 **拒绝不安全的卸载** —— 拒绝删除仍在加载中的动态库，而不是去赌。赌错的代价是 UB。
- 🪶 **零 async** —— 没有 runtime，没有 `tokio`。装 = 起子进程，载 = `dlopen`。
- 📚 **公开 API 全文档** —— `#![deny(missing_docs)]`。

## 目录

- [为什么需要它](#为什么需要它)
- [安装](#安装)
- [快速开始](#快速开始)
- [配置](#配置)
- [磁盘布局](#磁盘布局)
- [安装流程](#安装流程)
- [prebuilt 产物](#prebuilt-产物)
- [manifest 从哪来](#manifest-从哪来)
- [它不做什么](#它不做什么)
- [开发](#开发)
- [仓库](#仓库)
- [许可](#许可)

## 为什么需要它

`cargo install` **只认 bin target**。一个 `crate-type = ["cdylib"]` 的 crate 没有 bin，
安装会直接失败：

```console
$ cargo install my-plugin
error: there is nothing to install
```

加一个 bin target 也没用 —— `cargo install` 只会把那个 bin 装进 `~/.cargo/bin`，
cdylib 依然不会出现在任何有用的位置。直接构建插件 crate 也不行，因为 Cargo 把依赖
当 rlib 构建，**永远不会替依赖产出 cdylib**。

所以这个库改为生成一个极小的 **wrapper 工程**，让插件本体保持普通 rlib：

```text
插件 crate（crates.io 上的普通 rlib）
    │  作为依赖
    ▼
wrapper 工程（本库生成，约 30 行，[lib] crate-type = ["cdylib"]）
    │  cargo build --release
    ▼
libxxx.so / xxx.dll / libxxx.dylib
    │  拷到 <data-dir>/plugins/<crate>/
    ▼
运行时 dlopen
```

让插件保持 rlib 的好处：

- 对生态里的其它人来说，它就是个普通 crate；
- 能被单元测试直接 `use`，完全不用碰 `dlopen`；
- 插件作者**一行 `#[no_mangle]` 都不用写** —— cdylib 的形状由宿主契约 crate 的
  `export!` 宏生成。

## 安装

```bash
cargo add crate-plugin-kit
```

### 环境要求

- **Rust 1.88+** 用于构建（即 MSRV —— `libloading` 0.9 要求 1.88）。
- 运行期 PATH 上要有 **`cargo`**，供默认的 build-host 安装路径使用。它没有被链接进来 ——
  安装是起子进程。prebuilt 路径不需要它。
- Linux / macOS / Windows。

## 快速开始

```rust
use crate_plugin_kit::{CratePluginKit, KitConfig};

// 你的契约 crate 定义的 #[repr(C)] 入口结构体
#[repr(C)]
struct MyHostEntry {
    abi_version: u32,
    // ...
}

let mut cfg = KitConfig::new("myapp");
cfg.crate_prefix   = "myapp-plugin-".into();
cfg.entry_symbol   = b"myapp_plugin_entry_v1".to_vec();
cfg.contract_crate = "myapp-plugin".into();
cfg.wrapper_body   = "myapp_plugin::export!({crate_ident}::create);\n".into();

let kit = CratePluginKit::<MyHostEntry>::new(cfg)?;

kit.install("foo", None)?;                    // 装 myapp-plugin-foo
for info in kit.list()? {
    println!("{} {}", info.name, info.version);
}

let plugin = kit.load("myapp-plugin-foo")?;   // *const MyHostEntry
let entry: *const MyHostEntry = plugin.entry();
```

`name` 可以写短名（`"foo"`，会自动补 crate 前缀），也可以写完整 crate 名。
版本传 `None` 表示从 crates.io 取最新版。

## 配置

[`KitConfig`](https://docs.rs/crate-plugin-kit/latest/crate_plugin_kit/struct.KitConfig.html)
是**所有宿主专有名字**的所在地。这个库一个都不写死。

| 字段 | 含义 |
| --- | --- |
| `id` | 应用 id。数据目录为 `~/.{id}`，同时进 HTTP User-Agent。 |
| `manifest_name` | 插件 manifest 文件名，如 `myapp-plugin.toml`。 |
| `crate_prefix` | 插件 crate 前缀。`install("foo")` → `myapp-plugin-foo`。 |
| `entry_symbol` | 要找的导出符号，如 `myapp_plugin_entry_v1`。 |
| `lib_stem_prefix` | cdylib 文件名前缀，如 `myapp_plugin_`。 |
| `contract_crate` / `contract_version` | 宿主的契约 crate —— 生成的 wrapper 依赖它。 |
| `wrapper_edition` / `wrapper_body` | 生成 wrapper 的 `edition` 与 `src/lib.rs` 内容。`{crate_ident}` 会被替换。 |
| `data_dir` | 覆盖 `~/.{id}`。测试时有用。 |
| `lock_timeout` | 等待安装锁的时长。默认 30s。 |
| `prefer_prebuilt` | 是否先试 GitHub Releases 再回落本地编译。默认 `true`。 |
| `target_triple` | 覆盖编译期 target。 |
| `registry` | crates.io 基地址。可以指向镜像。 |
| `local_overrides` | `crate 名 → 本地路径`。会写进 wrapper 的 `[patch.crates-io]`。**仅开发期**。 |

`KitConfig::new(id)` 会填一套保守默认值；你至少还要设 `crate_prefix`、`entry_symbol`、
`lib_stem_prefix`、`contract_crate`、`wrapper_body`，否则生成的 wrapper 编不过。

### 为什么需要 `local_overrides`

wrapper 住在 `<data-dir>/build/` 下，**看不到你项目里的 `.cargo/config.toml`**。
开发期要让 wrapper 对着本地检出构建，只能由宿主显式说明，本库再把它写进 wrapper 自己的
`[patch.crates-io]`。

## 磁盘布局

```text
~/.{id}/
├── plugins/
│   ├── myapp-plugin-foo/
│   │   ├── myapp-plugin.toml     ← manifest（安装时写入）
│   │   └── libmyapp_plugin_foo.so
│   └── .plugins.json             ← 安装记录缓存
├── build/                        ← 生成的 wrapper
│   └── myapp-plugin-foo/
└── .lock                         ← 跨进程安装锁
```

`.plugins.json` **是缓存，不是事实来源**。丢了或坏了，`list()` 会从磁盘上的 manifest 重建。

## 安装流程

两条路。**默认是 build-host**，因为它走 `cargo`，天然使用用户已经配好的 registry / 镜像。
prebuilt 走 GitHub，会绕开这些配置 —— 所以它只当加速项。

| 路径 | 做什么 | 前置条件 |
| --- | --- | --- |
| **build-host**（默认） | 生成 wrapper → `cargo build --release` → 拷产物 | 有 Rust 工具链 |
| **prebuilt** | 从 GitHub Releases 下载 cdylib + manifest | 插件作者发了产物 |

```text
install(name, version)
  ├─ 取锁 <data-dir>/.lock
  ├─ 规范化 crate 名（补前缀）
  ├─ 确定版本（None 就查 crates.io）
  ├─ 清掉旧的安装目录
  ├─ [prefer_prebuilt] 试 prebuilt ── 成功 ──▶ 完
  │                                  └─ 不可用 ─┐
  └──────────────────────────────────────────◄──┘
     生成 wrapper → cargo metadata → cargo build --release
     → 找 cdylib → 拷 cdylib + manifest → 更新 .plugins.json
```

只要 prebuilt 不可用 —— 没发过、404、网络失败 —— 安装都会**静默回落到 build-host**。
两条路互相独立，而 `cargo` 那条往往在 GitHub 不通的时候反而能成。

`cargo build` 的 stdio 是继承的，所以你能看到进度。这一步可能要几分钟，
把输出吞掉是很差的体验。

版本用 `=x.y.z` 精确锁定，因此 `.plugins.json` 里记的版本和真正编出来的 cdylib 不可能对不上。

## prebuilt 产物

是**两个裸文件**而不是压缩包 —— 这样就不必引入 tar、gzip、zip 三个 crate：

```text
{crate}-{version}-{target}.{so|dylib|dll}   ← cdylib 本体
{crate}-{version}-{target}.toml             ← manifest
```

例如：

```text
myapp-plugin-foo-0.1.0-x86_64-pc-windows-msvc.dll
myapp-plugin-foo-0.1.0-x86_64-pc-windows-msvc.toml
```

仓库地址取自 crates.io 上该 crate 的 `repository` 字段 —— 此时插件还没装上，
唯一能问的就是 registry。

## manifest 从哪来

插件 crate 的**源码根目录**里放一份 `<manifest_name>`。build-host 安装时，
本库通过 `cargo metadata` 定位 crate 源码目录，把该文件拷进安装目录：

```text
cargo metadata --format-version 1 --manifest-path <wrapper>/Cargo.toml
  → 在 packages[] 里找 name 等于插件 crate 的那一项
  → 取它 manifest_path 的父目录 —— 那就是 crate 源码目录
  → 从那里拷 <manifest_name>
```

这保证了单一事实来源：插件的元信息跟着它的代码走。而运行期检测仍然是纯文件读取 ——
**仅仅为了列出插件，不需要 `dlopen`**。

## 它不做什么

- **不做 ABI 校验。** 它完全不知道 `T` 里有什么。校验 `abi_version` 是宿主契约 crate 的事。
- **不负责 `catch_unwind` 的调用点。** 库提供了工具（`panic::guard`），但只有契约 crate
  知道*哪些*调用跨了边界，所以得由*它*去包。
- **不异步。** 安装是起一个 `cargo build`，加载是 `dlopen`。两者都是同步的。
- **不限制插件来源。** 没有白名单、没有签名校验、没有沙箱 —— 与 `cargo install` 同一套信任模型。

分界线只有一个问题：**这个操作需要知道 `T` 里有什么吗？**

| 操作 | 需要 `T` 的内容？ | 归属 |
| --- | --- | --- |
| 装 / 卸 / 列举 | 否 | 本库 |
| `dlopen`、取符号、cast 成 `*const T` | 否 | 本库 |
| 查 crates.io | 否 | 本库 |
| 校验 `abi_version` | **是** | 契约 crate |
| 调 `command()` / `name()` | **是** | 契约 crate |
| 管理跨边界字符串的生命周期 | **是** | 契约 crate |

## 开发

```text
crate-plugin-kit/
├── Cargo.toml            # [package] + 单元素 [workspace]
├── build.rs              # 注入 target triple
├── docs/proposal.md      # 设计说明
├── src/
│   ├── lib.rs            # 公开出口 + crate 文档
│   ├── config.rs         # KitConfig / KitPaths
│   ├── error.rs          # KitError
│   ├── manifest.rs       # manifest schema 与读写
│   ├── store.rs          # CratePluginKit<T>
│   ├── loader.rs         # 泛型 dlopen，无类型擦除
│   ├── lock.rs           # 跨进程文件锁
│   ├── cache.rs          # .plugins.json
│   ├── registry.rs       # crates.io 查询
│   ├── panic.rs          # catch_unwind 工具
│   └── install/
│       ├── build_host.rs # 生成 wrapper + cargo build（默认）
│       └── prebuilt.rs   # GitHub Releases 下载（加速）
└── tests/
    ├── load.rs           # 端到端：编出 cdylib、dlopen、跨边界调用
    └── fixtures/toy-plugin/   # 那个测试用的零依赖 cdylib
```

```bash
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo package --locked
```

`tests/load.rs` 是最要紧的那个：它把 fixture 编成真的 `cdylib`，拷进一个临时插件库，
然后 `dlopen` 它并跨边界调用一个函数指针。其余测试都只是在验证文件读写和字符串推导。

### 关于 `rust-toolchain.toml`

**刻意没有这个文件**，两个原因：

1. 钉死的 channel 会盖过 CI 里测 MSRV 的那个 job —— 而 MSRV 正是 `Cargo.toml` 里
   `rust-version` 真正承诺的东西；
2. 钉死具体版本会迫使贡献者额外下载一整套工具链，而 `rust-version` + CI
   已经覆盖了真正的问题。

生成的 wrapper 也不写它 —— 直接用你当前在用的那套工具链。

## 仓库

<https://github.com/imba97/crate-plugin-kit>

## 许可

MIT —— 见 [LICENSE](LICENSE)。
