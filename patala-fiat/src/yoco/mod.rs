//! The Yoco adapter (South Africa — Checkouts API, ZAR-only). One
//! `patala_core::PaymentRail` talking to Yoco's Checkouts API. Ported from
//! cackle's `internal/payments/yoco.go`. See `rail.rs`'s module docs for the
//! full `Provider` -> `PaymentRail` mapping and `PORTING.md` for the general
//! recipe this follows.
//!
//! Gated behind the `yoco` Cargo feature — see the crate root docs and
//! `Cargo.toml`.
//!
//! **UNVERIFIED AGAINST LIVE**: same disclosure as every rail in this crate
//! beyond `manual` (`PORTING.md` §10). Cackle's own file header rates its
//! confidence MEDIUM-HIGH: Yoco explicitly documents using the Svix webhook
//! standard, which this port implements faithfully (signed-content
//! template + timestamp-tolerance replay guard) — see `webhook.rs`.
//!
//! Yoco is ZAR-only, and its `amount` field is already an integer minor
//! unit (cents) — matching `PayRequest::amount_minor` directly, no
//! currency-exponent conversion needed or attempted (unlike Flutterwave/
//! iyzico/Midtrans).

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::YocoConfig;
pub use rail::YocoRail;
