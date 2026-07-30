//! # patala-core
//!
//! The core seam of `patala` (see `PATALA.md` §3): one trait, one capability
//! descriptor, class-respecting failover, and the offline default every
//! consumer and CI run gets for free before a single real rail exists.
//!
//! | Piece | What it is |
//! |---|---|
//! | [`PaymentRail`] | The trait — `id`, `capabilities`, `quote`, `charge`, `verify`, `validate_destination`, `refund`, `verify_webhook`. |
//! | [`RailClass`] | `CustodialReversible` \| `NonCustodialFinal` — the settlement class, in the type. |
//! | [`RailCapabilities`] | class, reversible, requires_kyc, holds_funds, currencies, settlement, atomic_multi_party. |
//! | [`FailoverRail`] | Tries wrapped rails in order; never crosses [`RailClass`] silently. |
//! | [`MockRail`] | The offline default — deterministic, dependency-free. |
//! | [`WebhookDelivery`] / [`WebhookEvent`] | The push side: raw bytes + headers in, an authenticated [`WebhookStatus`] out. |
//! | [`DestinationVerdict`] / [`DestinationStatus`] | The pre-flight side: what a rail can honestly say about an address *offline*, before money moves. |
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
//!
//! **Paying a customer back on a final rail is not a reversal.**
//! [`PaymentRail::refund`] stays [`Error::Unsupported`] on a
//! `NonCustodialFinal` rail because finality is the whole point of that class.
//! Giving the money back there is a *compensating payment*: a second,
//! independent [`PaymentRail::charge`] to an address **the customer supplies**
//! — never the address the payment came from, which is very often an exchange
//! withdrawal address where the funds cannot be credited back to them.
//! [`PaymentRail::validate_destination`] is the offline, pure check a caller
//! runs on that address before a human approves the payout; it can never
//! report that an address is *safe*, only that no defect it can decide was
//! found. See the [`destination`] module for the whole flow and
//! [`EXCHANGE_DEPOSIT_CAVEAT`] for what patala refuses to guess at.
//!
//! # Example — the offline default, end to end
//!
//! [`MockRail`] needs no network and no chain SDK, so this charge → verify
//! round-trip is what every consumer and CI run gets for free before a single
//! real rail exists:
//!
//! ```
//! use patala_core::{MockRail, PayRequest, PaymentRail, RailClass};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let rail = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
//!
//! let req = PayRequest {
//!     amount_minor: 500, // 5.00 USDC — integer minor units, never a float
//!     currency: "USDC".into(),
//!     destination: "wallet-or-processor-token".into(),
//!     reference: "order-1".into(),
//! };
//!
//! // `charge` returns the Receipt — the entitlement.
//! let receipt = rail.charge(&req).await.unwrap();
//! assert_eq!(receipt.reference, "order-1");
//!
//! // Gate on `verify` returning `Ok(true)`, never on `charge` merely having
//! // returned `Ok`: a receipt can be stored and re-checked later, and only
//! // `verify` re-derives whether it still holds.
//! assert!(rail.verify(&receipt).await.unwrap());
//! # });
//! ```

mod capabilities;
pub mod destination;
mod error;
mod failover;
mod mock;
mod rail;
mod webhook;

pub use capabilities::{RailCapabilities, RailClass, Settlement};
pub use destination::{DestinationStatus, DestinationVerdict, EXCHANGE_DEPOSIT_CAVEAT};
pub use error::{Error, Result};
pub use failover::{FailoverRail, InOrderPolicy, RoutingPolicy};
pub use mock::MockRail;
pub use rail::{PayRequest, PaymentRail, Quote, Receipt};
pub use webhook::{WebhookDelivery, WebhookEvent, WebhookStatus};

/// Current unix time in whole seconds. Used for quote expiry / receipt
/// timestamps in [`MockRail`]; real rails derive their own from chain/processor
/// time.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
