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
- 📦 **直接从 crates.io 装** —— 安装走 `cargo` 本地编译，所以你**已经配好的 registry / 镜像自动生效**。
  prebuilt 产物只是可选加速项，从不是前置条件。
- 🧩 **泛型而非类型擦除** —— `load()` 返回的是瘦指针 `*const T`，不是 `dyn Trait`。
  没有 fat pointer 转换，边界上也就没有 UB。
- 🏷️ **所有宿主专有名字都是配置项** —— 应用 id、manifest 名、crate 前缀、入口符号、wrapper 内容。
  这个库一个都不写死。
- 🔒 **并发与卸载都安全** —— 跨进程安装锁；拒绝删除仍在加载中的动态库。

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
- [仓库](#仓库)
- [许可](#许可)

## 为什么需要它

`cargo install` **只认 bin target**，所以一个 `crate-type = ["cdylib"]` 的 crate 根本没有东西可装。
加一个 bin target 也没用 —— 那个 bin 会进 `~/.cargo/bin`，cdylib 依然不会出现在任何有用的位置。
直接构建插件 crate 也不行：Cargo 把依赖当 rlib 构建，**永远不会替依赖产出 cdylib**。

所以这个库改为生成一个极小的 **wrapper 工程**（约 30 行，`[lib] crate-type = ["cdylib"]`）来依赖插件，
让插件本体保持普通 rlib：

```text
插件 crate（crates.io 上的普通 rlib）
    │  作为依赖
    ▼
wrapper 工程（本库生成）
    │  cargo build --release
    ▼
libxxx.so / xxx.dll / libxxx.dylib
    │  拷到 <data-dir>/plugins/<crate>/
    ▼
运行时 dlopen
```

让插件保持 rlib 的好处：它对生态里的其它人来说就是个普通 crate；能被单元测试直接 `use`，
完全不用碰 `dlopen`；插件作者**一行 `#[no_mangle]` 都不用写** —— cdylib 的形状由宿主契约 crate 的
`export!` 宏生成。

## 安装

```bash
cargo add crate-plugin-kit
```

### 产物工具（只给插件作者用）

```bash
cargo install crate-plugin-kit --bin plugin-asset
```

可选，**使用**本库不需要它。它负责产出插件发版时要上传的那两个文件 —— 见
[prebuilt 产物](#prebuilt-产物)。

### 环境要求

- **Rust 1.88+** 用于构建（即 MSRV —— `libloading` 0.9 要求 1.88）。
- 运行期 PATH 上要有 **`cargo`**，供默认的 build-host 安装路径使用。安装是起子进程，
  没有东西被链接进来。prebuilt 路径不需要它。
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

`local_overrides` 之所以存在：wrapper 住在 `<data-dir>/build/` 下，**看不到你项目里的
`.cargo/config.toml`**。开发期要让 wrapper 对着本地检出构建，只能由宿主显式说明，
本库再把它写进 wrapper 自己的 `[patch.crates-io]`。

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

| 路径 | 做什么 | 前置条件 |
| --- | --- | --- |
| **build-host**（默认） | 生成 wrapper → `cargo build --release` → 拷产物 | 有 Rust 工具链 |
| **prebuilt** | 从 GitHub Releases 下载 cdylib + manifest | 插件作者发了产物 |

只要 prebuilt 不可用 —— 没发过、404、网络失败 —— 安装都会**静默回落到 build-host**，
而 `cargo` 那条往往在 GitHub 不通的时候反而能成。`cargo build` 的 stdio 是继承的，
所以你能看到进度；这一步可能要几分钟，把输出吞掉是很差的体验。版本用 `=x.y.z` 精确锁定，
因此 `.plugins.json` 里记的版本和真正编出来的 cdylib 不可能对不上。

## prebuilt 产物

是**两个裸文件**而不是压缩包 —— 这样就不必引入 tar、gzip、zip 三个 crate：

```text
{crate}-{version}-{target}.{so|dylib|dll}   ← cdylib 本体
{crate}-{version}-{target}.toml             ← manifest
```

例如 `myapp-plugin-foo-0.1.0-x86_64-pc-windows-msvc.dll` 加上配套的 `.toml`。

### 怎么产出

本库自带产出这两个文件的工具，于是发版侧不需要再实现一遍 wrapper、lib 名和命名约定：

```bash
cargo install crate-plugin-kit --bin plugin-asset
plugin-asset --id myapp
# dist/myapp-plugin-foo-0.1.0-x86_64-unknown-linux-gnu.so
# dist/myapp-plugin-foo-0.1.0-x86_64-unknown-linux-gnu.toml
```

`--id` 就是宿主传给 `KitConfig::new` 的那个字符串。在插件仓库里直接跑即可；
`--manifest-path`、`--out-dir`、`--target`、`--target-dir` 都可覆盖（`--help` 有全表）。
如果插件 manifest 的 `[plugin] version` 与 `Cargo.toml` 的版本不一致它会**拒绝执行**；
而当构建目标就是本机时，它还会 `dlopen` 一下产物，确认入口符号真的导出了。

所以发布矩阵只需要列出为哪个 target 构建，一个 target 一个 runner：

| Runner | 目标三元组 | 产物扩展名 |
| --- | --- | --- |
| `ubuntu-latest` | `x86_64-unknown-linux-gnu` | `.so` |
| `windows-latest` | `x86_64-pc-windows-msvc` | `.dll` |
| `macos-13` | `x86_64-apple-darwin` | `.dylib` |
| `macos-14` | `aarch64-apple-darwin` | `.dylib` |

扩展名跟着**目标三元组**走，不跟着打包机器走；安装时的查找则跟着宿主自己的三元组走 ——
于是某个产物只会被同 target 的宿主使用，没人用的 target 永远不会被下载。

### 怎么被找到

仓库地址取自 crates.io 上该 crate 的 `repository` 字段 —— 此时插件还没装上，
唯一能问的就是 registry。两个文件都挂在该仓库的 `v{version}` tag 上；`.toml` 里的版本
必须与请求的版本一致，否则下载会被拒绝。

## manifest 从哪来

插件 crate 的**源码根目录**里放一份 `<manifest_name>`。build-host 安装时，本库通过
`cargo metadata` 定位那个源码目录并把文件拷进安装目录 —— 于是插件的元信息跟着它的代码走，
永远不会和实现脱节。运行期检测因此仍然只是纯文件读取：**仅仅为了列出插件，不需要 `dlopen`**。

## 它不做什么

分界线只有一个问题：**这个操作需要知道 `T` 里有什么吗？**

| 操作 | 需要 `T` 的内容？ | 归属 |
| --- | --- | --- |
| 装 / 卸 / 列举 | 否 | 本库 |
| `dlopen`、取符号、cast 成 `*const T` | 否 | 本库 |
| 查 crates.io | 否 | 本库 |
| 校验 `abi_version` | **是** | 契约 crate |
| 调 `command()` / `name()` | **是** | 契约 crate |
| 管理跨边界字符串的生命周期 | **是** | 契约 crate |

本库完全不知道 `T` 里有什么，所以 **ABI 校验、`catch_unwind` 的调用点、跨边界生命周期
全都归契约 crate** —— 本库提供了 `panic::guard` 工具，但只有契约 crate 知道*哪些*调用跨了边界。
也不异步，更不限制插件来源：没有白名单、没有签名校验、没有沙箱，与 `cargo install` 同一套信任模型。

## 仓库

<https://github.com/imba97/crate-plugin-kit>

## 许可

MIT —— 见 [LICENSE](LICENSE)。
