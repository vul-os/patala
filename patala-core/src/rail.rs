//! §3 THE SEAM: the [`PaymentRail`] trait and its request/response types.
//!
//! Money is always integer minor units (a `u64`) plus a currency string —
//! **never a float**, anywhere in this crate. See `PATALA.md` §3, §8.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::capabilities::{RailCapabilities, Settlement};
use crate::error::{Error, Result};
use crate::webhook::{WebhookDelivery, WebhookEvent};

/// A request to move money on some rail.
///
/// `patala-core` never parses `destination` — it is a wallet address to a
/// crypto rail and an opaque processor-side token to a fiat rail, and only the
/// rail that receives it knows which.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayRequest {
    /// Amount in the currency's smallest unit (cents, USDC base units, ...).
    /// Never a float — see `PATALA.md` §8.
    pub amount_minor: u64,
    /// ISO-4217 code for fiat (`"USD"`), or the asset ticker for crypto
    /// (`"USDC"`). What a given rail accepts is declared on
    /// [`RailCapabilities::currencies`].
    pub currency: String,
    /// Where the money should go. A wallet address for a crypto rail, or an
    /// opaque processor-side destination token for a fiat rail.
    pub destination: String,
    /// Caller-supplied idempotency / correlation key. Rails should make
    /// [`PaymentRail::charge`] idempotent on this where their backend permits
    /// it, and it is always echoed back on the resulting [`Receipt`].
    pub reference: String,
}

impl PayRequest {
    /// Reject the obviously-invalid — zero amount, or any empty string field
    /// — before any rail does real work. Never touches the network.
    pub fn validate(&self) -> Result<()> {
        if self.amount_minor == 0 {
            return Err(Error::InvalidRequest("amount_minor must be nonzero".into()));
        }
        if self.currency.trim().is_empty() {
            return Err(Error::InvalidRequest("currency must not be empty".into()));
        }
        if self.destination.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "destination must not be empty".into(),
            ));
        }
        if self.reference.trim().is_empty() {
            return Err(Error::InvalidRequest("reference must not be empty".into()));
        }
        Ok(())
    }
}

/// Fees, fx and expiry for a prospective payment. Produced without moving any
/// money.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    /// The rail that produced this quote (matches [`PaymentRail::id`]).
    pub rail_id: String,
    /// The requested amount, echoed back.
    pub amount_minor: u64,
    /// The requested currency, echoed back.
    pub currency: String,
    /// Rail fee, in the same currency's minor units.
    pub fee_minor: u64,
    /// `amount_minor + fee_minor` (saturating; never negative, never a float).
    pub total_minor: u64,
    /// How long settlement is expected to take.
    pub settlement: Settlement,
    /// Unix seconds after which this quote is stale and must be re-fetched.
    pub expires_at_unix: u64,
}

/// Signed proof that a [`PayRequest`] was executed.
///
/// **This is the entitlement.** Whatever a caller gates on payment should gate
/// on [`PaymentRail::verify`] returning `Ok(true)`, never on `charge` merely
/// having returned `Ok` — a receipt can be handed to another party, stored,
/// and re-checked later, and only `verify` re-derives whether it still holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// Which rail produced this. Must match [`PaymentRail::id`] on whichever
    /// rail is later asked to verify or refund it.
    pub rail_id: String,
    /// The amount actually moved, in minor units.
    pub amount_minor: u64,
    /// The currency moved.
    pub currency: String,
    /// The [`PayRequest::reference`] this receipt fulfills.
    pub reference: String,
    /// Rail-specific proof/binding blob: a tx signature plus on-chain memo for
    /// a chain rail, a signed digest for [`crate::MockRail`], a processor
    /// charge id plus its own signature for a fiat rail. Opaque to
    /// `patala-core` — only the rail that issued a given receipt knows how to
    /// re-verify it.
    pub proof: Vec<u8>,
    /// Unix seconds when the rail considers this settled.
    pub settled_at_unix: u64,
}

/// §3 THE SEAM. Nothing outside a rail's own implementation may name a
/// provider-specific type — every consumer of `patala-core` programs against
/// this trait and [`RailCapabilities`] only.
#[async_trait]
pub trait PaymentRail: Send + Sync {
    /// Stable rail id, e.g. `"solana"`, `"hyperswitch"`, `"mock"`.
    fn id(&self) -> &str;

    /// What this rail is and is not able to do, and under what guarantees.
    fn capabilities(&self) -> &RailCapabilities;

    /// Fees, fx and expiry for a prospective payment. Must not move money.
    async fn quote(&self, req: &PayRequest) -> Result<Quote>;

    /// Initiate/settle a payment. Returns a [`Receipt`] — the entitlement.
    async fn charge(&self, req: &PayRequest) -> Result<Receipt>;

    /// Verify a receipt was actually issued by this rail and is internally
    /// consistent.
    ///
    /// **Must fail closed**: any doubt returns `Ok(false)`. Return `Err` only
    /// for an operational failure to even perform the check (RPC down, and
    /// the like) — never to imply the receipt is valid, and never `Ok(true)`
    /// on a receipt this rail cannot actually re-derive.
    async fn verify(&self, receipt: &Receipt) -> Result<bool>;

    /// Reverse a settled payment.
    ///
    /// Rails that cannot reverse a payment — any `NonCustodialFinal` rail, by
    /// definition, since finality is the whole point — MUST return
    /// [`Error::Unsupported`] rather than a stub that appears to work. This is
    /// the default, so a rail only needs to override it if it genuinely can.
    async fn refund(&self, _receipt: &Receipt) -> Result<Receipt> {
        Err(Error::Unsupported("refund"))
    }

    /// Authenticate an inbound webhook delivery from this rail's processor
    /// and report what it says — the *push* counterpart to [`Self::verify`].
    ///
    /// **Must fail closed**: a missing, malformed, stale or mismatched
    /// signature is an `Err`, never an `Ok` with a negative status. Reaching
    /// `Ok` means this rail is satisfied the delivery genuinely came from its
    /// own processor; what that delivery then *claims* is
    /// [`WebhookEvent::status`], and a scheme that authenticates a
    /// notification without asserting anything about money must report
    /// [`crate::WebhookStatus::Unconfirmed`] rather than pretending to know.
    ///
    /// Rails whose processor has no push delivery at all — and the offline
    /// [`crate::MockRail`] — return [`Error::Unsupported`] rather than a stub
    /// that appears to work. That is the default, so a rail only overrides
    /// this if it genuinely verifies something.
    ///
    /// This lives on the trait, and not beside each rail as a free function,
    /// because a free function is unreachable from every non-Rust consumer:
    /// the UniFFI surface and the sidecar both dispatch through
    /// `dyn PaymentRail`, so anything not on the trait cannot be exposed to
    /// Python, Go, Swift or an HTTP client at all.
    async fn verify_webhook(&self, _delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        Err(Error::Unsupported("verify_webhook"))
    }
}
