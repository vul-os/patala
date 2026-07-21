//! The PayU India ("PayU Biz") adapter — one `patala_core::PaymentRail`
//! talking to PayU's hash-signed hosted checkout form and its
//! server-to-server Verify Payment API. Ported from cackle's
//! `internal/payments/payu.go`. See `rail.rs`'s module docs for the full
//! `Provider` -> `PaymentRail` mapping and `PORTING.md` for the general
//! recipe this port follows.
//!
//! NOT PayU LatAm or PayU Global — different products, different APIs,
//! same caveat cackle's own file header carries.
//!
//! Gated behind the `payu` Cargo feature — see the crate root docs and
//! `Cargo.toml`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::PayUConfig;
pub use rail::PayURail;
