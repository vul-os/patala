//! The Adyen adapter — one `patala_core::PaymentRail` talking to Adyen's
//! Checkout API (Pay by Link). Ported from cackle's
//! `internal/payments/adyen.go`. See `rail.rs`'s module docs for the full
//! `Provider` -> `PaymentRail` mapping and `PORTING.md` for the general
//! recipe this port follows.
//!
//! Gated behind the `adyen` Cargo feature — see the crate root docs and
//! `Cargo.toml`.
//!
//! **UNVERIFIED AGAINST LIVE** (`PORTING.md` §10): no live Adyen account was
//! reachable from this environment. Every request/response shape mirrors
//! cackle's own `adyen.go`, which cites docs.adyen.com throughout; every
//! test here mocks HTTP with `wiremock`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::AdyenConfig;
pub use rail::AdyenRail;
