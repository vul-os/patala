//! The Mercado Pago adapter — one `patala_core::PaymentRail` talking to
//! Mercado Pago's Checkout Pro (Preferences) and Payments APIs. Ported from
//! cackle's `internal/payments/mercadopago.go`. See `rail.rs`'s module docs
//! for the full `Provider` -> `PaymentRail` mapping and `PORTING.md` for the
//! general recipe this port follows.
//!
//! Gated behind the `mercadopago` Cargo feature — see the crate root docs
//! and `Cargo.toml`.
//!
//! **UNVERIFIED AGAINST LIVE** (`PORTING.md` §10): no live Mercado Pago
//! account was reachable from this environment. Every request/response
//! shape mirrors cackle's own `mercadopago.go`, which itself rates its own
//! confidence as "MEDIUM-HIGH on the webhook signature manifest template...
//! MEDIUM on the Preferences API request/response shape" and states it has
//! not been run against a real Mercado Pago test account either; every test
//! here mocks HTTP with `wiremock`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::MercadoPagoConfig;
pub use rail::MercadoPagoRail;
