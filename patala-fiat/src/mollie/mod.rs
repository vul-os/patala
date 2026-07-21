//! The Mollie adapter — one `patala_core::PaymentRail` talking to Mollie's
//! Payments API. Ported from cackle's `internal/payments/mollie.go`. See
//! `rail.rs`'s module docs for the full `Provider` -> `PaymentRail` mapping
//! and `PORTING.md` for the general recipe this port follows.
//!
//! Gated behind the `mollie` Cargo feature — see the crate root docs and
//! `Cargo.toml`. Note this is the ONE adapter in this crate needing no
//! `hmac`/`sha2`/`hex` deps at all: Mollie's classic webhook carries no
//! signature (see `webhook.rs`'s module docs) so there is nothing to HMAC.
//!
//! **UNVERIFIED AGAINST LIVE** (`PORTING.md` §10): no live Mollie account
//! was reachable from this environment. Every request/response shape
//! mirrors cackle's own `mollie.go`; every test here mocks HTTP with
//! `wiremock`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::MollieConfig;
pub use rail::MollieRail;
