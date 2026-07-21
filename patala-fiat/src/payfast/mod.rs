//! The PayFast adapter (South Africa — Onsite/Redirect + ITN, ZAR-only).
//! One `patala_core::PaymentRail` talking to PayFast's Onsite payment flow
//! and ITN webhook. Ported from cackle's `internal/payments/payfast.go`.
//! See `rail.rs`'s module docs for the full `Provider` -> `PaymentRail`
//! mapping and `PORTING.md` for the general recipe this follows.
//!
//! PayFast is the one processor in this batch Hyperswitch doesn't cover, so
//! this direct adapter matters more than the other four.
//!
//! Gated behind the `payfast` Cargo feature — see the crate root docs and
//! `Cargo.toml`.
//!
//! **UNVERIFIED AGAINST LIVE**: same disclosure as every rail in this crate
//! beyond `manual` (`PORTING.md` §10). Cackle's own file header rates its
//! confidence HIGH on the ITN verification model (the security-critical
//! part — signature + mandatory validate-callback), MEDIUM on `Begin`'s
//! exact request shape (PayFast's canonical integration is an HTML form
//! auto-POSTed to their process endpoint, not a bare redirect link — see
//! `rail.rs`'s module docs on `proof::ChargeProof`).
//!
//! PayFast is ZAR-only. Amounts are decimal strings in major units (e.g.
//! `"100.00"` for R100) — a straightforward 2-decimal conversion via
//! [`crate::currency`], no zero/three-decimal ambiguity.
//!
//! **Not implemented, disclosed** (cackle's own file header): PayFast's own
//! documented anti-fraud checklist for an ITN has THREE steps — (1) verify
//! the signature, (2) verify the source IP is one of PayFast's published
//! ranges, (3) POST the notification back to PayFast's own `validate`
//! endpoint and require the literal response `"VALID"`. This port
//! implements (1) and (3) — arguably the strongest of the three, since it
//! round-trips through PayFast itself — but NOT (2) (source IP
//! allowlisting), since that requires deployment-specific network
//! configuration this crate has no way to see; callers deploying this in
//! production should add IP filtering at their ingress/load balancer, as
//! PayFast recommends.

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::PayFastConfig;
pub use rail::PayFastRail;
