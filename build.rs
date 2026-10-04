//! 编译期把 target triple 注入进去，供 [`crate::KitConfig::effective_target`] 使用。
//!
//! 用环境变量而不是 `cfg!` 判断：prebuilt 资产名要的是**完整三元组**
//! （`x86_64-pc-windows-msvc`），而 `std::env::consts` 只给得到 `windows` / `x86_64`。

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=CPK_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
