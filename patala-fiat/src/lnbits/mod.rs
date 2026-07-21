//! The LNbits / Lightning adapter — a self-hosted, non-custodial
//! `patala_core::PaymentRail` for small, instant, on-site-friendly payments.
//! Ported from cackle's `internal/payments/lnbits.go`. See `rail.rs`'s
//! module docs for the full `Provider` -> `PaymentRail` mapping and
//! `PORTING.md` for the general recipe this port follows.
//!
//! Gated behind the `lnbits` Cargo feature — see the crate root docs and
//! `Cargo.toml`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::LNbitsConfig;
pub use rail::LNbitsRail;
