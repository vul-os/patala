//! The BTCPay Server adapter — a self-hosted, non-custodial Bitcoin/
//! Lightning `patala_core::PaymentRail`. Ported from cackle's
//! `internal/payments/btcpay.go`. See `rail.rs`'s module docs for the full
//! `Provider` -> `PaymentRail` mapping and `PORTING.md` for the general
//! recipe this port follows.
//!
//! Gated behind the `btcpay` Cargo feature — see the crate root docs and
//! `Cargo.toml`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::BTCPayConfig;
pub use rail::BTCPayRail;
