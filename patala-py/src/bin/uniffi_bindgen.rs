//! Generates the Python (or Swift/Kotlin/...) bindings from this crate's
//! `#[uniffi::export]` surface in `src/lib.rs` — no separately installed
//! `uniffi-bindgen` binary or `maturin` required. See
//! `patala-py/README.md` "Build & run".
//!
//! Usage (from the workspace root):
//! ```text
//! cargo build -p patala-py
//! cargo run -p patala-py --bin uniffi-bindgen -- generate \
//!     --library target/debug/libpatala_py.dylib \
//!     --language python \
//!     --out-dir patala-py/bindings/python
//! ```
fn main() {
    uniffi::uniffi_bindgen_main()
}
