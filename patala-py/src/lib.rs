//! # patala-py
//!
//! The Python packaging of patala's one UniFFI surface.
//!
//! **There is no binding definition in this file, and there must never be
//! one.** Every exported type, constructor and method lives in
//! [`patala_uniffi`], which owns the single `#[uniffi::export]` surface for
//! *every* target language and declares its namespace explicitly as
//! `"patala"` (`PATALA.md` §5's "M×1, never M×N"). This crate exists for one
//! reason: to produce the `cdylib` a Python wheel ships
//! (`libpatala_py.{dylib,so}`), with `patala-py`'s own name, version and
//! metadata, from that shared surface.
//!
//! ## Why this crate is now three lines instead of thirteen hundred
//!
//! It used to hold the whole surface, and that made `patala_py` the UniFFI
//! *namespace* — UniFFI derives one from the crate that calls
//! `setup_scaffolding!()`. Every generator then named its output after
//! Python: `uniffi-bindgen-go` emitted a file beginning `package patala_py`,
//! which `patala-go` had to alias away at every import. With ten more
//! languages on the way, all ten would have inherited a Python-flavoured name
//! for a surface with nothing Python-specific in it. So the definitions moved
//! to [`patala_uniffi`], which calls `uniffi::setup_scaffolding!("patala")`,
//! and this crate became its Python-facing packaging.
//!
//! ## How the cdylib still works
//!
//! UniFFI's scaffolding is a set of `#[no_mangle] extern "C"` functions.
//! `rustc` re-exports the `#[no_mangle]` symbols of every linked rlib from a
//! `cdylib`, so `libpatala_py.{dylib,so}` carries [`patala_uniffi`]'s complete
//! scaffolding *and* its embedded metadata — which is what `uniffi-bindgen
//! --library` reads. `patala-py/tests/scaffolding.rs` and the `smoke-python`
//! target both check that empirically rather than trusting the paragraph you
//! just read.
//!
//! The re-export below is what makes the linker keep that rlib, and it also
//! means `patala_py::PatalaRail` still resolves for any Rust code that
//! referred to this crate directly.
//!
//! ## Feature flags
//!
//! Unchanged for callers: `solana`, `stellar`, `hyperswitch`, `fiat`,
//! `fiat-<provider>` and `fiat-all` mean exactly what they always did. Each
//! now forwards to the identically-named feature on [`patala_uniffi`]
//! (`patala-py/Cargo.toml`), and `scripts/check-features.sh` fails the build
//! if any of them stops forwarding.

pub use patala_uniffi::*;
