//! The set of rails this sidecar process knows how to talk to.
//!
//! Everything here is `Arc<dyn patala_core::PaymentRail>` — the trait object,
//! never a provider-specific type (`PATALA.md` §3) — so the HTTP handlers in
//! `api.rs` never need to know or care which concrete rail answered a given
//! `rail_id`.
//!
//! ## Adding a real rail later
//!
//! Today [`default_registry`] registers exactly one rail: a
//! [`patala_core::MockRail`] under the id `"mock"`, which is enough to prove
//! the whole HTTP surface end-to-end offline. When `patala-solana` /
//! `patala-stellar` / `patala-hyperswitch` exist, wire them in the same
//! function, each behind its own Cargo feature, e.g.:
//!
//! ```ignore
//! #[cfg(feature = "solana")]
//! registry.insert(
//!     "solana".to_string(),
//!     Arc::new(patala_solana::SolanaRail::from_env()?) as Arc<dyn PaymentRail>,
//! );
//! ```
//!
//! No handler in `api.rs` changes: they all look a rail up by `rail_id` in
//! this map and call the trait methods, exactly as they do for `"mock"`
//! today. This crate's `Cargo.toml` deliberately does not declare those
//! optional dependencies yet (they don't exist in this tree) — see the
//! comment there.

use std::collections::HashMap;
use std::sync::Arc;

use patala_core::{MockRail, PaymentRail, RailClass};

/// Rail id -> the rail that handles it.
pub type RailRegistry = HashMap<String, Arc<dyn PaymentRail>>;

/// The registry this sidecar starts with. Always includes the offline
/// `"mock"` rail so the sidecar is exercisable with no network and no real
/// rail present (`PATALA.md` §8) — real rails are additive, never a
/// replacement for this.
pub fn default_registry() -> RailRegistry {
    let mut registry: RailRegistry = HashMap::new();
    registry.insert(
        "mock".to_string(),
        Arc::new(MockRail::new(
            "mock",
            RailClass::NonCustodialFinal,
            vec!["USDC".to_string(), "USD".to_string()],
        )) as Arc<dyn PaymentRail>,
    );
    registry
}
