//! Generates the Python (or Swift/Kotlin/...) bindings from this crate's
//! `#[uniffi::export]` surface in `src/lib.rs` — no separately installed
//! `uniffi-bindgen` binary or `maturin` required. See
//! `patala-uniffi/README.md` "Build & run".
//!
//! Usage (from the workspace root):
//! ```text
//! cargo build -p patala-uniffi
//! cargo run -p patala-uniffi --bin uniffi-bindgen -- generate \
//!     --library target/debug/libpatala_uniffi.dylib \
//!     --language python \
//!     --out-dir patala-uniffi/bindings/python
//! ```
fn main() {
    uniffi::uniffi_bindgen_main()
}
