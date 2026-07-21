//! The Checkout.com adapter — one `patala_core::PaymentRail` talking to
//! Checkout.com's Hosted Payments Page. Ported from cackle's
//! `internal/payments/checkoutcom.go`. See `rail.rs`'s module docs for the
//! full `Provider` -> `PaymentRail` mapping and `PORTING.md` for the general
//! recipe this port follows.
//!
//! Gated behind the `checkoutcom` Cargo feature — see the crate root docs
//! and `Cargo.toml`.
//!
//! **UNVERIFIED AGAINST LIVE** (`PORTING.md` §10): no live Checkout.com
//! account was reachable from this environment. Every request/response
//! shape mirrors cackle's own `checkoutcom.go`, which itself carries an
//! explicit HONESTY note about its own confidence level on the Hosted
//! Payments Page request field names (see `rail.rs`'s module docs); every
//! test here mocks HTTP with `wiremock`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::CheckoutComConfig;
pub use rail::CheckoutComRail;
