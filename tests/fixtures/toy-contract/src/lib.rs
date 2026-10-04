//! A stand-in for a host contract crate.
//!
//! A real one carries the `#[repr(C)]` entry struct and the `export!` macro. Here it only
//! has to exist under the name the wrapper depends on, so that the fixture plugin can
//! depend on it **by path** — which is what `pack` is expected to mirror into the wrapper.

/// Stands in for whatever a contract crate offers its plugins.
pub fn contract_version() -> &'static str {
    "0.1.0"
}
