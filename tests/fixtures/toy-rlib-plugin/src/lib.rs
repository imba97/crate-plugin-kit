//! A stand-in for a real plugin crate: rlib only, no exported symbols, one factory.
//!
//! The wrapper that `pack` generates is what turns this into a cdylib and exports the
//! entry symbol, which is why nothing here mentions `#[no_mangle]`.

/// What a generated wrapper would call (`<contract>::export!({crate_ident}::create)` in
/// a real project).
pub fn create() -> u32 {
    7
}
