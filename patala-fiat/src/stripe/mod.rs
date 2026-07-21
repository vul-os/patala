//! The Stripe pilot adapter — one `patala_core::PaymentRail` talking to
//! Stripe's Checkout Sessions API. Ported from cackle's
//! `internal/payments/stripe.go`. See `rail.rs`'s module docs for the full
//! `Provider` -> `PaymentRail` mapping and `PORTING.md` for the general
//! recipe this pilot establishes.
//!
//! Gated behind the `stripe` Cargo feature — see the crate root docs and
//! `Cargo.toml`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::StripeConfig;
pub use rail::StripeRail;
