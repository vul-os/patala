//! The Razorpay adapter — one `patala_core::PaymentRail` talking to
//! Razorpay's Orders/Payments API. Ported from cackle's
//! `internal/payments/razorpay.go`. See `rail.rs`'s module docs for the
//! full `Provider` -> `PaymentRail` mapping and `PORTING.md` for the
//! general recipe this crate's pilots established.
//!
//! Gated behind the `razorpay` Cargo feature — see the crate root docs and
//! `Cargo.toml`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::RazorpayConfig;
pub use rail::RazorpayRail;
