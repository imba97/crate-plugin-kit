//! 测试用的假插件。
//!
//! 它只做一件事：导出一个 C ABI 入口，返回一个指向静态结构体的指针。
//! 结构体里除了数据，还有一个**函数指针** —— 这样宿主侧的测试可以真的跨
//! `dlopen` 边界调用一次代码，而不只是读几个字段。
//!
//! 它零依赖，所以测试能离线编译。

/// 宿主与插件共同约定的入口结构体布局。
///
/// ⚠️ 宿主侧 `tests/load.rs` 里有一份**逐字段对应**的定义。两边必须保持一致 ——
/// 这正是宿主契约 crate 里的 `abi_version` 字段要守的东西。
#[repr(C)]
pub struct ToyEntry {
    /// ABI 版本。宿主加载后第一件事就是比对它。
    pub abi_version: u32,
    /// UTF-8 名字的起始地址。**内存归本插件所有**，宿主只读、不许 free。
    pub name_ptr: *const u8,
    /// 名字的字节长度。
    pub name_len: usize,
    /// 一个货真价实的跨边界函数指针。
    pub add: extern "C" fn(u32, u32) -> u32,
}

// SAFETY: `ENTRY` 是编译期常量，写一次之后从不修改；里面的裸指针指向 `NAME`，
// 也是一个不可变静态量。所以跨线程共享它是安全的。
//
// 每个写 `static ENTRY: ...` 的插件作者都会撞上这条 —— 编译器不会自动相信
// "这个静态量里面那个裸指针其实只读"。
unsafe impl Sync for ToyEntry {}

static NAME: &[u8] = b"toy";

/// 用 `wrapping_add` 而不是 `+`：溢出时宁可回绕，也不要在插件里 panic ——
/// panic 想穿过 `extern "C"` 边界会被 abort，那会把宿主一起带走。
extern "C" fn add(a: u32, b: u32) -> u32 {
    a.wrapping_add(b)
}

static ENTRY: ToyEntry = ToyEntry {
    abi_version: 1,
    name_ptr: NAME.as_ptr(),
    name_len: NAME.len(),
    add,
};

/// 唯一的导出符号。
///
/// # Safety
///
/// 返回的是一个指向 `'static` 数据的指针，调用方只要不写它就安全。
#[no_mangle]
pub extern "C" fn toyapp_plugin_entry_v1() -> *const ToyEntry {
    &ENTRY
}
