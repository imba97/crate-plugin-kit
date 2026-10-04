//! 泛型 `dlopen` 加载。
//!
//! # 这里没有类型擦除
//!
//! `T` 是**宿主自己的 `#[repr(C)]` 入口结构体**（对 bmux 来说就是 `BmuxPluginV1`）。
//! `load()` 返回的是 `*const T` —— **瘦指针**，不是 `dyn Trait`，所以不存在
//! "两个不同 vtable 的 fat pointer 互转"那种 UB。
//!
//! 本 crate **不知道也不需要知道** `T` 里有什么字段；它只负责把符号地址取出来
//! 并 cast 成 `*const T`。怎么安全地读那些字段是宿主契约 crate 的事。
//!
//! # 调用方必须保证的事
//!
//! 1. `T` 与插件实际导出的结构体**布局一致**（靠宿主的 `abi_version` 字段兜底）；
//! 2. 每一次读 `T` 的字段之前，要自己包 [`crate::panic::guard`]。

use std::path::{Path, PathBuf};

use libloading::Library;

use crate::error::{KitError, KitResult};

/// 一个已加载的插件。
///
/// 持有 `Library` 句柄 —— **drop 掉这个结构体会把插件卸载**。
/// 所以只要还想用它导出的东西，就得让它活着。
pub struct LoadedPlugin<T> {
    /// 必须持有。字段名以下划线开头是刻意的：它唯一的职责就是活到结构体被 drop。
    _lib: Library,
    entry: *const T,
    path: PathBuf,
}

impl<T> LoadedPlugin<T> {
    /// 入口结构体的指针。
    ///
    /// # Safety
    ///
    /// 调用方保证 `T` 的布局与插件导出的结构体一致。本 crate 无法验证这一点 ——
    /// 那是宿主 `abi_version` 字段的职责。
    pub fn entry(&self) -> *const T {
        self.entry
    }

    /// 动态库文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// 刻意**不**实现 `Send` / `Sync`：`T` 里装的是裸函数指针，它指向的代码是否线程安全
// 本 crate 无从得知。需要跨线程传递的宿主应该自己用 `unsafe impl` 明确表态。

/// 打开一个 cdylib 并调用它的入口函数。
///
/// # Safety
///
/// - `path` 指向的必须是本 kit 装出来的插件（或布局等价的动态库）；
/// - `T` 必须与那个插件导出的结构体布局一致。
///
/// # Errors
///
/// - [`KitError::LibraryLoad`]：`dlopen` 失败。
/// - [`KitError::SymbolMissing`]：没有 `symbol` 这个导出符号。
/// - [`KitError::NullEntry`]：入口函数返回了空指针。
pub unsafe fn open<T>(path: &Path, symbol: &[u8]) -> KitResult<LoadedPlugin<T>> {
    let lib = Library::new(path).map_err(|source| KitError::LibraryLoad {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;

    let entry = {
        let f: libloading::Symbol<'_, unsafe extern "C" fn() -> *const T> =
            lib.get(symbol).map_err(|source| {
                if is_symbol_not_found(&source) {
                    // 「库里没有这个符号」是常见情况（装错了东西 / 装的是别的插件），
                    // 单独报比笼统的"加载失败"好读得多。
                    KitError::SymbolMissing {
                        symbol: String::from_utf8_lossy(symbol).into_owned(),
                    }
                } else {
                    KitError::LibraryLoad {
                        path: path.to_path_buf(),
                        source: Box::new(source),
                    }
                }
            })?;
        f()
    };

    if entry.is_null() {
        return Err(KitError::NullEntry);
    }

    Ok(LoadedPlugin {
        _lib: lib,
        entry,
        path: path.to_path_buf(),
    })
}

/// `libloading` 表示"库里没有这个符号"的错误，**在各平台的变体名不一样**：
///
/// | 平台 | 变体 |
/// | ---- | ---- |
/// | Unix | `DlSym` / `DlSymUnknown` |
/// | Windows | `GetProcAddress` / `GetProcAddressUnknown` |
///
/// 不区分平台地 match 是编译不过的（变体只在对应平台存在），所以按 `cfg` 拆开。
fn is_symbol_not_found(e: &libloading::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(
            e,
            libloading::Error::DlSym { .. } | libloading::Error::DlSymUnknown
        )
    }

    #[cfg(windows)]
    {
        matches!(
            e,
            libloading::Error::GetProcAddress { .. } | libloading::Error::GetProcAddressUnknown
        )
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

/// 按平台给出 cdylib 文件名的候选列表，**按优先级排列**。
///
/// 会顺带列出别的平台的命名 —— 万一安装目录里躺着一个错平台的产物，
/// 报错信息里能看见"试过哪些"比只说"找不到"有用。
pub fn library_candidates(stem: &str) -> Vec<String> {
    let mut out = Vec::new();

    match std::env::consts::OS {
        "windows" => out.push(format!("{stem}.dll")),
        "macos" => out.push(format!("lib{stem}.dylib")),
        _ => out.push(format!("lib{stem}.so")),
    }

    // 兜底：有些构建系统（或手工打包）不带 lib 前缀 / 扩展名不一致。
    for ext in ["so", "dylib", "dll"] {
        for name in [format!("lib{stem}.{ext}"), format!("{stem}.{ext}")] {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }

    out
}

/// 在一个目录里按候选列表找 cdylib，返回第一个存在的。
pub fn find_library(dir: &Path, stem: &str) -> KitResult<PathBuf> {
    let candidates = library_candidates(stem);

    for name in &candidates {
        let p = dir.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }

    Err(KitError::LibraryNotFound {
        name: stem.to_string(),
        tried: candidates.join(", "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 当前平台该找的文件名必须排在候选列表第一位 —— 否则会优先命中错平台的产物。
    #[test]
    fn the_current_platform_name_comes_first() {
        let c = library_candidates("myapp_plugin_foo");
        let first = c.first().expect("候选列表不该为空");

        let expected = match std::env::consts::OS {
            "windows" => "myapp_plugin_foo.dll",
            "macos" => "libmyapp_plugin_foo.dylib",
            _ => "libmyapp_plugin_foo.so",
        };
        assert_eq!(first, expected);
    }

    /// 兜底项要覆盖所有平台命名 —— 这样"目录里躺着一个错平台的产物"时，
    /// 报错信息里的"试过哪些"才有意义。
    #[test]
    fn candidates_cover_every_platform_spelling() {
        let c = library_candidates("stem");
        for name in [
            "stem.dll",
            "libstem.dylib",
            "libstem.so",
            "stem.so",
            "stem.dylib",
            "libstem.dll",
        ] {
            assert!(c.iter().any(|x| x == name), "候选里缺 {name}：{c:?}");
        }
    }

    #[test]
    fn candidates_have_no_duplicates() {
        let c = library_candidates("stem");
        let mut sorted = c.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), c.len(), "候选里有重复：{c:?}");
    }

    #[test]
    fn find_library_picks_the_matching_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("unrelated.txt"), b"x").unwrap();

        let want = library_candidates("myapp_plugin_foo")[0].clone();
        std::fs::write(dir.path().join(&want), b"not really a library").unwrap();

        let found = find_library(dir.path(), "myapp_plugin_foo").expect("应当找到");
        assert_eq!(found.file_name().unwrap().to_string_lossy(), want);
    }

    #[test]
    fn find_library_reports_what_it_tried() {
        let dir = tempfile::tempdir().unwrap();

        let err = find_library(dir.path(), "myapp_plugin_foo");
        match err {
            Err(KitError::LibraryNotFound { name, tried }) => {
                assert_eq!(name, "myapp_plugin_foo");
                assert!(tried.contains("myapp_plugin_foo"), "tried = {tried}");
            }
            other => panic!("期望 LibraryNotFound，得到 {other:?}"),
        }
    }

    /// 目录里的同名**目录**不算命中 —— 只有文件才算。
    #[test]
    fn find_library_ignores_a_directory_with_the_right_name() {
        let dir = tempfile::tempdir().unwrap();
        let want = &library_candidates("myapp_plugin_foo")[0];
        std::fs::create_dir(dir.path().join(want)).unwrap();

        assert!(find_library(dir.path(), "myapp_plugin_foo").is_err());
    }

    /// 打开一个根本不是动态库的文件，要报 `LibraryLoad` 而不是 panic。
    #[test]
    fn opening_a_non_library_reports_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.bin");
        std::fs::write(&path, b"this is not a shared object").unwrap();

        // SAFETY: 这里就是要它失败，不涉及任何布局假设。
        let got = unsafe { open::<u32>(&path, b"whatever") };
        assert!(
            matches!(got, Err(KitError::LibraryLoad { .. })),
            "期望 LibraryLoad，得到 {:?}",
            got.err()
        );
    }
}
