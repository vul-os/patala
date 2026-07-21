//! The OpenNode adapter — a hosted, CUSTODIAL Bitcoin/Lightning checkout
//! `patala_core::PaymentRail`. Ported from cackle's
//! `internal/payments/opennode.go`. See `rail.rs`'s module docs for the
//! full `Provider` -> `PaymentRail` mapping, the `RailClass`/`holds_funds`
//! reasoning (this adapter genuinely diverges from `btcpay`/`lnbits`'s
//! self-hosted/non-custodial classification — see that module doc for why),
//! and `PORTING.md` for the general recipe this port follows.
//!
//! Gated behind the `opennode` Cargo feature — see the crate root docs and
//! `Cargo.toml`.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::OpenNodeConfig;
pub use rail::OpenNodeRail;
