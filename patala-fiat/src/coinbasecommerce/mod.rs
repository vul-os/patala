//! The Coinbase Commerce adapter — a hosted, CUSTODIAL crypto checkout
//! `patala_core::PaymentRail`, same tier as `opennode` in this crate. Ported
//! from cackle's `internal/payments/coinbasecommerce.go`. See `rail.rs`'s
//! module docs for the full `Provider` -> `PaymentRail` mapping, the
//! `RailClass`/`holds_funds` reasoning, and `PORTING.md` for the general
//! recipe this port follows.
//!
//! Gated behind the `coinbasecommerce` Cargo feature — see the crate root
//! docs and `Cargo.toml`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::CoinbaseCommerceConfig;
pub use rail::CoinbaseCommerceRail;
