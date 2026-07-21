//! The Midtrans adapter (Indonesia — Snap API, IDR-only). One
//! `patala_core::PaymentRail` talking to Midtrans's Snap + Core APIs. Ported
//! from cackle's `internal/payments/midtrans.go`. See `rail.rs`'s module
//! docs for the full `Provider` -> `PaymentRail` mapping and `PORTING.md`
//! for the general recipe this follows.
//!
//! Gated behind the `midtrans` Cargo feature — see the crate root docs and
//! `Cargo.toml`.
//!
//! **UNVERIFIED AGAINST LIVE**: same disclosure as every rail in this crate
//! beyond `manual` (`PORTING.md` §10). Cackle's own file header rates its
//! confidence MEDIUM-HIGH.
//!
//! **Currency-exponent note, ported verbatim from cackle's own file header**
//! because it is exactly the class of bug `PORTING.md` §8 warns about: ISO
//! 4217 formally assigns IDR a 2-decimal exponent (this crate's
//! [`crate::currency`] table agrees), so `amount_minor` for an IDR order is
//! in sen, NOT whole rupiah, even though nobody actually uses sen. Midtrans's
//! own wire format, however, carries `gross_amount` as the plain whole-rupiah
//! face value. This adapter bridges that gap by always routing through
//! [`crate::currency::minor_to_major_string`]/
//! [`crate::currency::major_string_to_minor`] using IDR's real (2-decimal)
//! exponent — it never treats IDR as zero-decimal.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::MidtransConfig;
pub use rail::MidtransRail;
