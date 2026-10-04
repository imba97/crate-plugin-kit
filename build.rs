//! Injects the target triple at compile time, for
//! [`crate::KitConfig::effective_target`] to use.
//!
//! An environment variable rather than a `cfg!` check, because prebuilt asset names
//! need the full triple (`x86_64-pc-windows-msvc`) while `std::env::consts` only
//! gives `windows` / `x86_64`.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=CPK_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
