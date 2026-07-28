//! # patala-hyperswitch
//!
//! The fiat side of `PATALA.md` §4: one `patala-core::PaymentRail` that talks
//! to a **self-hosted [Hyperswitch](https://github.com/juspay/hyperswitch)**
//! instance over HTTP, presenting its whole processor set
//! (Stripe/Paystack/Xendit/... -- 100+ connectors, Apache-2.0, Rust,
//! self-hostable) as a single `RailClass::CustodialReversible` rail.
//!
//! **This crate ADOPTS Hyperswitch; it does not rebuild any processor.**
//! There is no per-processor code path here -- exactly one HTTP client, one
//! `PaymentRail` impl, and the processor actually used on a given charge is
//! whatever the self-hosted Hyperswitch instance (and this crate's
//! `connector` config field) routes to. See `PATALA.md` §2.
//!
//! ## Non-custodial invariant
//!
//! `patala` itself never holds funds (`PATALA.md` §1, §8). This rail's
//! [`patala_core::RailCapabilities::holds_funds`] is `true` -- that describes
//! **Hyperswitch's underlying processor's** custody (a real card/bank rail
//! genuinely does hold money in flight), never this crate's or patala's own.
//! No code path in this crate receives, stores, or forwards actual funds; it
//! only ever sends/receives JSON describing a request to move money that
//! Hyperswitch and its connector carry out.
//!
//! ## UNVERIFIED AGAINST LIVE
//!
//! **No live Hyperswitch instance was reachable from the environment this
//! crate was written in.** Every request/response shape here was checked
//! against Hyperswitch's own published OpenAPI spec and source (see
//! `README.md` "Sources"), and every unit test in this crate mocks HTTP with
//! `wiremock` -- there is no live-network test, ignored or otherwise, because
//! there is no live instance to point one at from here. Do not treat a green
//! `cargo test -p patala-hyperswitch` as proof this works against a real
//! Hyperswitch deployment; it proves this crate builds the requests Hyperswitch's
//! own docs describe and parses the responses those docs describe. See
//! `README.md` for what remains NEEDS-CONFIRMATION.
//!
//! ## Layout
//!
//! | Module | What it is |
//! |---|---|
//! | [`config`] | [`HyperswitchConfig`] -- base URL + API key from env/config, never hardcoded. |
//! | [`rail`] | [`HyperswitchRail`] -- the `PaymentRail` implementation itself. |
//! | `models` | Wire DTOs matching Hyperswitch's OpenAPI spec (private -- not part of the seam). |
//! | `proof` | What this rail embeds in a [`patala_core::Receipt::proof`] (private). |
//! | [`webhook`] | [`webhook::verify_webhook_signature`] -- fail-closed HMAC check for Hyperswitch's outgoing webhooks, reached through [`patala_core::PaymentRail::verify_webhook`]. |

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::HyperswitchConfig;
pub use rail::HyperswitchRail;
pub use webhook::verify_webhook_signature;
