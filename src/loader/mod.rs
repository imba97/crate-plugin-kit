//! Generic `dlopen` loading.
//!
//! # No type erasure here
//!
//! `T` is the host's own `#[repr(C)]` entry struct (for bmux, that is
//! `BmuxPluginV1`). `load()` returns a `*const T` — a thin pointer, not a `dyn Trait`
//! — so there is no "convert a fat pointer between two different vtables" UB.
//!
//! This crate neither knows nor needs to know which fields `T` has; it only reads
//! the symbol address and casts it to `*const T`. Reading those fields safely is the
//! host contract crate's job.
//!
//! # What the caller has to guarantee
//!
//! 1. `T` and the struct the plugin actually exports have the same layout (backed by
//!    the host's `abi_version` field);
//! 2. every read of a field of `T` is wrapped in [`crate::panic::guard`].
//!
//! # Layout
//!
//! `open` holds the `dlopen` itself, `naming` decides which file names a cdylib can
//! have. The two are independent on purpose: `pack` and `prebuilt` name artifacts for a
//! target triple without ever loading anything.

mod naming;
mod open;

pub use naming::{
    find_library, find_library_for_target, lib_ext_for_target, library_candidates,
    library_candidates_for_target,
};
pub use open::{open, LoadedPlugin};
