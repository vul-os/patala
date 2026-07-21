//! # patala-core
//!
//! The core seam of `patala` (see `PATALA.md` §3): one trait, one capability
//! descriptor, class-respecting failover, and the offline default every
//! consumer and CI run gets for free before a single real rail exists.
//!
//! | Piece | What it is |
//! |---|---|
//! | [`PaymentRail`] | The trait — `id`, `capabilities`, `quote`, `charge`, `verify`, `refund`. |
//! | [`RailClass`] | `CustodialReversible` \| `NonCustodialFinal` — the settlement class, in the type. |
//! | [`RailCapabilities`] | class, reversible, requires_kyc, holds_funds, currencies, settlement. |
//! | [`FailoverRail`] | Tries wrapped rails in order; never crosses [`RailClass`] silently. |
//! | [`MockRail`] | The offline default — deterministic, dependency-free. |
//!
//! Nothing outside a rail's own implementation names a provider-specific
//! type. Every consumer of this crate programs against [`PaymentRail`] and
//! [`RailCapabilities`] only — real rails (Solana, Stellar, a self-hosted
//! Hyperswitch, ...) live in their own crates/features and depend on this
//! one, never the reverse (`PATALA.md` §4, §9).
//!
//! **The settlement class is in the type.** [`RailClass`] is not a bool
//! because it changes the trust contract the payer is shown: a refundable
//! "pending" state with a card form (`CustodialReversible`) is not the same
//! promise as a wallet address and a signed final receipt
//! (`NonCustodialFinal`). [`FailoverRail`] enforces that this boundary is
//! never crossed by accident — see its docs.
//!
//! **The default build is offline.** [`MockRail`] is deterministic and adds no
//! external crypto dependency; `patala-core`'s own dependency list (see
//! `Cargo.toml`) is `async-trait` + `serde` + `thiserror` — no network client,
//! no chain SDK. Every real rail is feature-gated in its own crate and is
//! never a mandatory dependency of this one (`PATALA.md` §8).
//!
//! **Non-custodial invariant.** No type or method in this crate can make
//! patala itself hold funds. [`RailCapabilities::holds_funds`] describes a
//! rail's *own processor* — never the substrate (`PATALA.md` §1, §8).
//!
//! **Money is integer minor units plus a currency string, never a float** —
//! see [`PayRequest`], [`Quote`], [`Receipt`].

mod capabilities;
mod error;
mod failover;
mod mock;
mod rail;

pub use capabilities::{RailCapabilities, RailClass, Settlement};
pub use error::{Error, Result};
pub use failover::{FailoverRail, InOrderPolicy, RoutingPolicy};
pub use mock::MockRail;
pub use rail::{PayRequest, PaymentRail, Quote, Receipt};

/// Current unix time in whole seconds. Used for quote expiry / receipt
/// timestamps in [`MockRail`]; real rails derive their own from chain/processor
/// time.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
