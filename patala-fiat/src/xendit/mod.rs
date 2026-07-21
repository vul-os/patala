//! The Xendit adapter — one `patala_core::PaymentRail` talking to Xendit's
//! Invoices API. Ported from cackle's `internal/payments/xendit.go`. See
//! `rail.rs`'s module docs for the full `Provider` -> `PaymentRail` mapping
//! and `PORTING.md` for the general recipe this port follows.
//!
//! Gated behind the `xendit` Cargo feature — see the crate root docs and
//! `Cargo.toml`. Deliberately the LEANEST feature in this crate: Xendit's
//! webhook auth is a static shared-secret token compare (no MAC, no digest),
//! so this feature pulls in only `reqwest` — no `hmac`/`sha2`/`hex`/`url`/
//! `base64` at all.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::XenditConfig;
pub use rail::XenditRail;
