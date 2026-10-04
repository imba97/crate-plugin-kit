//! 端到端：真的编出一个 cdylib，真的 `dlopen` 它，真的跨边界调用。
//!
//! 这是这个 crate 唯一一个能证明"加载链路真的通了"的测试 —— 其余单测都只是在
//! 验证文件读写和字符串推导。
//!
//! 覆盖的路径：
//!
//! ```text
//! cargo build --release   （fixture 是个零依赖的 cdylib）
//!   → 拷进插件目录 + 写 manifest
//!   → CratePluginKit::list()      只读文件，不 dlopen
//!   → CratePluginKit::load()      dlopen + 取符号
//!   → 读结构体字段 / 调函数指针    真的跨了一次边界
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate_plugin_kit::{find_library, CratePluginKit, KitConfig};

/// 与 `tests/fixtures/toy-plugin/src/lib.rs` 里的 `ToyEntry` **逐字段对应**。
///
/// 这就是宿主契约 crate 里那个结构的缩影；真实项目里它由契约 crate 定义，
/// 并且 `abi_version` 会被严格比对。
#[repr(C)]
struct ToyEntry {
    abi_version: u32,
    name_ptr: *const u8,
    name_len: usize,
    add: extern "C" fn(u32, u32) -> u32,
}

/// 刻意**不写** `[lib]` 段 —— 让 `KitConfig::lib_stem()` 的推导路径也被走到。
const MANIFEST: &str = r#"
[plugin]
name    = "toy"
version = "0.1.0"
abi     = 1
family  = "test"

# 宿主自己的段，kit 不认识也不该动它
[detect]
strong = ["toy.lock"]
"#;

/// 把 fixture 复制到临时目录并编译，返回临时目录（保活）与编出来的 cdylib 路径。
///
/// **必须复制出去再编**：fixture 住在仓库的 `tests/fixtures/` 下，就地编译的话
/// cargo 会一路上溯找到仓库根的 `rust-toolchain.toml`，在 CI 里可能触发一次
/// 多余的工具链下载。临时目录之下没有那个文件。
fn build_fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("应当能建临时目录");

    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("toy-plugin");
    let dst = tmp.path().join("toy-plugin");
    copy_tree(&src, &dst);

    let target_dir = tmp.path().join("target");

    // 直接叫 `cargo`：它一定在 PATH 上（我们此刻正跑在 cargo test 里），
    // 而且它是个真正的 .exe，不是 Windows 上那种 .cmd shim。
    let status = Command::new("cargo")
        .args(["build", "--release", "--manifest-path"])
        .arg(dst.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .status()
        .expect("应当能起 cargo");
    assert!(status.success(), "fixture 应当编译成功");

    let lib = find_library(&target_dir.join("release"), "toyapp_plugin_toy")
        .expect("应当在产物里找到 fixture 的 cdylib");

    (tmp, lib)
}

/// 递归复制目录（只处理普通文件与目录，够 fixture 用了）。
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("应当能建目标目录");
    for entry in std::fs::read_dir(from).expect("应当能读源目录") {
        let entry = entry.expect("目录项应当可读");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("应当能取类型").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("应当能复制文件");
        }
    }
}

/// 一个装好 fixture 的插件库，返回 (保活用的临时目录, kit)。
fn store_with_fixture(lib: &Path) -> (tempfile::TempDir, CratePluginKit<ToyEntry>) {
    let tmp = tempfile::tempdir().expect("应当能建临时目录");
    let root = tmp.path().join("store");

    let plugin_dir = root.join("plugins").join("toyapp-plugin-toy");
    std::fs::create_dir_all(&plugin_dir).expect("应当能建插件目录");

    // manifest 按默认命名：`{id}-plugin.toml`
    std::fs::write(plugin_dir.join("toyapp-plugin.toml"), MANIFEST).expect("应当能写 manifest");

    // cdylib 按它原本的文件名落地，loader 会自己按平台拼候选名
    std::fs::copy(lib, plugin_dir.join(lib.file_name().unwrap())).expect("应当能复制 cdylib");

    let cfg = KitConfig::new("toyapp")
        .with_data_dir(&root)
        .with_lock_timeout(Duration::from_millis(500));

    let kit = CratePluginKit::<ToyEntry>::new(cfg).expect("应当能建起 kit");
    (tmp, kit)
}

#[test]
fn loads_a_real_cdylib_and_calls_across_the_boundary() {
    let (_build_tmp, lib) = build_fixture();
    let (_store_tmp, kit) = store_with_fixture(&lib);

    // ---- list：只读文件，不 dlopen ---------------------------------------
    let all = kit.list().expect("list 应当成功");
    assert_eq!(all.len(), 1, "应当只列出一个插件：{all:?}");
    assert_eq!(all[0].name, "toy", "名字取自 manifest");
    assert_eq!(all[0].crate_name, "toyapp-plugin-toy");
    assert_eq!(all[0].version, "0.1.0");
    assert_eq!(all[0].family.as_deref(), Some("test"));

    // ---- manifest_of：宿主的段要被保留 -----------------------------------
    let manifest = kit.manifest_of("toy").expect("manifest 应当可读");
    assert!(manifest.extra.contains_key("detect"), "宿主的段不能丢");

    // ---- load：真的 dlopen ------------------------------------------------
    let loaded = kit.load("toy").expect("应当能加载");
    let entry = loaded.entry();
    assert!(!entry.is_null(), "入口指针不该为空");
    assert_eq!(loaded.path().file_name(), lib.file_name());

    // SAFETY: `ToyEntry` 与 fixture 里那份 `#[repr(C)]` 定义逐字段对应。
    // 真实项目里这一步之前会有一次 `abi_version` 比对（契约 crate 的职责）。
    unsafe {
        assert_eq!((*entry).abi_version, 1, "ABI 版本应当是 1");

        let name = std::slice::from_raw_parts((*entry).name_ptr, (*entry).name_len);
        assert_eq!(name, b"toy", "跨边界读到的名字");

        // 真的调了一次跨边界的函数指针 —— 这是 dlopen 链路通没通的硬证据
        assert_eq!(((*entry).add)(2, 3), 5);
        assert_eq!(((*entry).add)(u32::MAX, 1), 0, "wrapping_add 语义");
    }
}

/// 加载过之后就不许删了 —— Windows 上正在 `dlopen` 的 `.dll` 根本删不掉，
/// 而在 Linux 上删掉也只是让空间不回收。与其赌，不如拒绝。
#[test]
fn uninstall_is_refused_after_the_plugin_was_loaded() {
    let (_build_tmp, lib) = build_fixture();
    let (_store_tmp, kit) = store_with_fixture(&lib);

    // 加载之前可以正常卸
    // （这里只验证"加载之后被拒"，所以先加载）
    let _loaded = kit.load("toy").expect("应当能加载");

    match kit.uninstall("toy") {
        Err(crate_plugin_kit::KitError::PluginInUse { name }) => {
            assert_eq!(name, "toyapp-plugin-toy");
        }
        other => panic!("期望 PluginInUse，得到 {other:?}"),
    }

    // 目录必须原封不动 —— 拒绝就真的是没动
    assert!(kit.paths().plugin_dir("toyapp-plugin-toy").is_dir());
}

/// 装的是个垃圾文件时，要报「没有导出符号」，而不是 panic 或静默通过。
#[test]
fn loading_a_non_plugin_reports_a_missing_symbol() {
    let tmp = tempfile::tempdir().expect("应当能建临时目录");
    let root = tmp.path().join("store");

    let plugin_dir = root.join("plugins").join("toyapp-plugin-toy");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("toyapp-plugin.toml"), MANIFEST).unwrap();

    // 用**真正的动态库**冒充插件：它加载得起来，但没有我们要的符号。
    // 本测试自己的二进制就是个动态链接的可执行文件，拿它当素材最省事。
    let impostor = std::env::current_exe().expect("应当能拿到本测试的路径");
    let stem = "toyapp_plugin_toy";
    let name = match std::env::consts::OS {
        "windows" => format!("{stem}.dll"),
        "macos" => format!("lib{stem}.dylib"),
        _ => format!("lib{stem}.so"),
    };
    std::fs::copy(&impostor, plugin_dir.join(&name)).expect("应当能复制");

    let cfg = KitConfig::new("toyapp")
        .with_data_dir(&root)
        .with_lock_timeout(Duration::from_millis(500));
    let kit = CratePluginKit::<ToyEntry>::new(cfg).unwrap();

    // 结果取决于平台：要么这个文件根本 dlopen 不了（可执行文件不是共享库），
    // 要么加载成功但找不到符号。两者都是"正确地报告了失败"，都不是 panic。
    let err = match kit.load("toy") {
        Err(e) => e,
        // 它不可能成功：符号确实不存在。真成功了说明 load 的校验有漏洞。
        Ok(_) => panic!("这个文件不该被当成插件加载成功"),
    };
    assert!(
        matches!(
            err,
            crate_plugin_kit::KitError::SymbolMissing { .. }
                | crate_plugin_kit::KitError::LibraryLoad { .. }
        ),
        "期望 SymbolMissing 或 LibraryLoad，得到 {err:?}"
    );
}
