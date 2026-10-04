//! `catch_unwind` 工具。
//!
//! # 为什么需要它，以及它为什么**不能**是唯一一道防线
//!
//! 插件是 `dlopen` 进来的代码。它 panic 一次，宿主整个进程就没了。
//! 所以宿主要在**每一次**跨边界调用外面包一层 `catch_unwind`。
//!
//! ⚠️ 但从 Rust 1.81 起，**让 panic 越过 `extern "C"` 边界会直接 abort** ——
//! 也就是说：
//!
//! ```text
//! 插件内部 panic
//!    └─ 如果从插件的 extern "C" 函数里逃出来 → abort（宿主侧 catch_unwind 完全没用）
//! ```
//!
//! 所以正确做法是**两层**：
//!
//! | 层 | 在哪 | 干什么 |
//! | - | ---- | ------ |
//! | 内层 | 插件侧（契约 crate 的 `export!` 宏生成） | 把 panic 转成错误码，**不让它碰到 `extern "C"` 边界** |
//! | 外层 | 宿主侧（就是这里） | 兜住"插件用 `panic=abort` 编的""插件忘了包"之类的意外 |
//!
//! 外层这道防线在插件自己没包住时仍然有效 —— 前提是插件也是 `panic=unwind` 编的。
//! 这就是宿主 `Cargo.toml` 要写 `panic = "unwind"` 的原因。

use crate::error::{KitError, KitResult};

/// 把一次跨边界调用包进 `catch_unwind`。
///
/// `what` 只用于错误信息，例如 `"entry()"` / `"command()"`。
///
/// # 例子
///
/// ```no_run
/// use crate_plugin_kit::panic::guard;
///
/// // 假装这是一次读到插件导出符号、然后调用它的过程
/// fn call_plugin(entry: *const (), arg: u32) -> u32 {
///     guard("command()", || unsafe { read_field(entry) + arg }).unwrap_or(0)
/// }
///
/// /// # Safety
/// /// 调用方保证 `entry` 指向一个布局匹配的结构体。
/// unsafe fn read_field(_entry: *const ()) -> u32 { 1 }
/// ```
pub fn guard<T>(what: &str, f: impl FnOnce() -> T) -> KitResult<T> {
    // `AssertUnwindSafe`：我们只要求"panic 不要掀翻进程"，
    // 不承诺捕获之后继续使用捕获现场的那些可变状态 —— 调用方拿到 Err 就中止当前操作。
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|_| {
        KitError::PluginPanicked {
            what: what.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_normal_values() {
        let got = guard("test()", || 42).unwrap();
        assert_eq!(got, 42);
    }

    #[test]
    fn catches_panic() {
        let got: KitResult<()> = guard("test()", || panic!("boom"));
        assert!(matches!(got, Err(KitError::PluginPanicked { .. })));
    }
}
