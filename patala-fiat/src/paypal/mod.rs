//! The PayPal adapter — a fiat `patala_core::PaymentRail` talking to
//! PayPal's Orders v2 API. Ported from cackle's
//! `internal/payments/paypal.go`. See `rail.rs`'s module docs for the full
//! `Provider` -> `PaymentRail` mapping and `PORTING.md` for the general
//! recipe this port follows.
//!
//! Gated behind the `paypal` Cargo feature — see the crate root docs and
//! `Cargo.toml`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::PayPalConfig;
pub use rail::PayPalRail;
pub use webhook::{PayPalWebhookError, PayPalWebhookEvent, PayPalWebhookHeaders};
