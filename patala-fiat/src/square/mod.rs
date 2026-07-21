//! The Square adapter — one `patala_core::PaymentRail` talking to Square's
//! Payment Links (Checkout) API. Ported from cackle's
//! `internal/payments/square.go`. See `rail.rs`'s module docs for the full
//! `Provider` -> `PaymentRail` mapping and `PORTING.md` for the general
//! recipe this port follows.
//!
//! Gated behind the `square` Cargo feature — see the crate root docs and
//! `Cargo.toml`.

pub mod config;
mod models;
// **Deliberate exception to this crate's standard "proof is private/opaque"
// layout** (see `PORTING.md`'s file-layout table, and `proof.rs`'s own
// module doc for why): Square's structural payment-id gap requires an
// external caller to actively round-trip `ChargeProof` between `charge()`
// and a later `verify()` call, so `ChargeProof` itself must be reachable
// outside this crate, unlike every other adapter's `proof` module.
pub mod proof;
pub mod rail;
pub mod webhook;

pub use config::SquareConfig;
pub use proof::ChargeProof;
pub use rail::SquareRail;
