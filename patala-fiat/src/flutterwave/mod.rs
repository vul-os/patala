//! The Flutterwave adapter (NG/GH/KE/UG/TZ/ZA/... — Standard Payments). One
//! `patala_core::PaymentRail` talking to Flutterwave's v3 API. Ported from
//! cackle's `internal/payments/flutterwave.go`. See `rail.rs`'s module docs
//! for the full `Provider` -> `PaymentRail` mapping and `PORTING.md` for the
//! general recipe this follows.
//!
//! Gated behind the `flutterwave` Cargo feature — see the crate root docs
//! and `Cargo.toml`.
//!
//! **UNVERIFIED AGAINST LIVE**: same disclosure as every rail in this crate
//! beyond `manual` (`PORTING.md` §10) — no live Flutterwave sandbox account
//! was reachable from this environment. Cackle's own file header rates its
//! confidence "MEDIUM": the v3 API shape is well documented, but the one
//! point most worth double-checking against a real account is the
//! major-unit `amount` wire convention this port carries forward exactly
//! (see `rail.rs`'s module docs).

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::FlutterwaveConfig;
pub use rail::FlutterwaveRail;
