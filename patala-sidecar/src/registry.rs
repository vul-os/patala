//! The set of rails this sidecar process knows how to talk to.
//!
//! Everything here is `Arc<dyn patala_core::PaymentRail>` — the trait object,
//! never a provider-specific type (`PATALA.md` §3) — so the HTTP handlers in
//! `api.rs` never need to know or care which concrete rail answered a given
//! `rail_id`.
//!
//! ## THIS REGISTRY IS MOCK-ONLY. It reaches no real rail.
//!
//! Stated plainly because the rest of the sidecar is real and it would be
//! easy to assume this part is too: [`default_registry`] registers **exactly
//! one** rail, a [`patala_core::MockRail`] under the id `"mock"`. Nothing
//! else. A request to `/v1/rails/solana/...`, `/v1/rails/stripe/...` or any
//! other id gets a `404` — not because the rail failed, but because this
//! process has never heard of it. Per-rail registration is **unwritten**.
//!
//! What *is* real is everything around it: the loopback bind, the fail-closed
//! token gate, the error mapping, all six endpoints, and their round-trips
//! over a real socket in `tests/`. Those are exercised against `MockRail`, and
//! `MockRail` is a genuine [`patala_core::PaymentRail`] with a genuine
//! fail-closed `verify` — so the HTTP surface is proven, and the rail set
//! behind it is a stub. This module's own `tests::registry_is_mock_only`
//! pins that (a `#[cfg(test)]` item, hence not a doc link), so this paragraph
//! cannot quietly go stale.
//!
//! ### Why it is unwritten (the honest reason, updated)
//!
//! An earlier version of this note said the rail crates "don't exist in this
//! tree". That has not been true since `patala-solana`, `patala-stellar`,
//! `patala-hyperswitch` and `patala-fiat` landed — all four are workspace
//! members today. The remaining work is real but ordinary, and simply has not
//! been done:
//!
//! 1. Declare each as an **optional** dependency in this crate's
//!    `Cargo.toml`, behind its own feature, so the default build stays as
//!    offline as `patala-core` (the same trick `patala-py` already uses).
//! 2. Decide how each rail gets its configuration. Every real rail needs
//!    credentials, and the sidecar's whole premise is that those credentials
//!    live in *this* process and nowhere else. `from_env()` is the obvious
//!    answer and is not obviously the right one: a sidecar serving several
//!    rails needs a per-rail config source and a fail-closed story for a rail
//!    whose config is absent or malformed (skip it and 404, or refuse to
//!    start?). That decision is not made, and guessing it in code would be
//!    worse than leaving this stub honest.
//! 3. Extend `make lint`/`make test` to cover the feature-on build, or the
//!    new code would be unlinted and untested — this workspace has already
//!    been bitten by exactly that (see the root `Makefile`'s `test-features`).
//!
//! No handler in `api.rs` changes when that work happens: every handler looks
//! a rail up by `rail_id` in this map and calls trait methods, exactly as it
//! does for `"mock"` today. The shape each registration takes:
//!
//! ```ignore
//! #[cfg(feature = "solana")]
//! registry.insert(
//!     "solana".to_string(),
//!     Arc::new(patala_solana::SolanaRail::from_env()?) as Arc<dyn PaymentRail>,
//! );
//! ```

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the module docs' central claim so it cannot rot into a false
    /// statement in either direction: this registry is mock-only, and README
    /// / `site/docs/status.md` say so out loud.
    ///
    /// If you are here because this test failed, you added a rail — good.
    /// Update, in the same change: this test, the "MOCK-ONLY" heading above,
    /// `patala-sidecar/README.md`'s "Adding a real rail later" section, the
    /// caveat under the crate table in the root `README.md`, and the matching
    /// one in `site/docs/status.md`. All five say the same thing today, and
    /// the whole point of pinning it here is that they keep saying the same
    /// thing.
    #[test]
    fn registry_is_mock_only() {
        let registry = default_registry();

        assert_eq!(
            registry.len(),
            1,
            "default_registry() has {} rails: {:?}. The docs in this module, in \
             patala-sidecar/README.md, in README.md and in site/docs/status.md all state \
             it is mock-only — update every one of them together with this assertion.",
            registry.len(),
            registry.keys().collect::<Vec<_>>()
        );

        let mock = registry
            .get("mock")
            .expect("the offline \"mock\" rail must always be registered (PATALA.md §8)");
        assert_eq!(mock.id(), "mock");
        assert_eq!(
            mock.capabilities().class,
            RailClass::NonCustodialFinal,
            "the sidecar's stand-in rail must not describe itself as custodial"
        );
        assert!(
            !mock.capabilities().holds_funds,
            "nothing this sidecar registers holds funds"
        );
    }
}
