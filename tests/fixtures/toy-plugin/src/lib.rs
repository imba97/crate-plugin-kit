//! A fake plugin for tests.
//!
//! It does one thing: export a C ABI entry point that returns a pointer to a static
//! struct. Besides data, the struct holds a function pointer — so the host-side test
//! can really call code across the `dlopen` boundary instead of only reading a few
//! fields.
//!
//! It has zero dependencies, so the test can compile it offline.

/// The entry struct layout the host and the plugin agree on.
///
/// `tests/load.rs` on the host side has a field-for-field counterpart definition. The
/// two must stay in sync — which is exactly what the `abi_version` field in a host
/// contract crate guards.
#[repr(C)]
pub struct ToyEntry {
    /// ABI version. Comparing it is the first thing the host does after loading.
    pub abi_version: u32,
    /// Start address of the UTF-8 name. The memory belongs to this plugin; the host
    /// only reads it and must not free it.
    pub name_ptr: *const u8,
    /// Byte length of the name.
    pub name_len: usize,
    /// A real function pointer that crosses the boundary.
    pub add: extern "C" fn(u32, u32) -> u32,
}

// SAFETY: `ENTRY` is a compile-time constant that is written once and never mutated,
// and the raw pointer inside it points at `NAME`, also an immutable static. Sharing it
// across threads is therefore safe.
//
// Every plugin author writing `static ENTRY: ...` runs into this — the compiler will
// not take it on faith that the raw pointer inside the static is in fact read-only.
unsafe impl Sync for ToyEntry {}

static NAME: &[u8] = b"toy";

/// `wrapping_add` rather than `+`: on overflow, wrapping beats panicking inside the
/// plugin — a panic trying to cross the `extern "C"` boundary aborts, which would take
/// the host down with it.
extern "C" fn add(a: u32, b: u32) -> u32 {
    a.wrapping_add(b)
}

static ENTRY: ToyEntry = ToyEntry {
    abi_version: 1,
    name_ptr: NAME.as_ptr(),
    name_len: NAME.len(),
    add,
};

/// The only exported symbol.
///
/// # Safety
///
/// What is returned is a pointer to `'static` data; the caller is safe as long as it
/// does not write through it.
#[no_mangle]
pub extern "C" fn toyapp_plugin_entry_v1() -> *const ToyEntry {
    &ENTRY
}
