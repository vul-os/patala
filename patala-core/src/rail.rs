//! §3 THE SEAM: the [`PaymentRail`] trait and its request/response types.
//!
//! Money is always integer minor units (a `u64`) plus a currency string —
//! **never a float**, anywhere in this crate. See `PATALA.md` §3, §8.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::capabilities::{RailCapabilities, Settlement};
use crate::destination::DestinationVerdict;
use crate::error::{Error, Result};
use crate::webhook::{WebhookDelivery, WebhookEvent};

/// A request to move money on some rail.
///
/// `patala-core` never parses `destination` — it is a wallet address to a
/// crypto rail and an opaque processor-side token to a fiat rail, and only the
/// rail that receives it knows which. A caller that wants to check one *before*
/// committing to a charge asks that rail, through
/// [`PaymentRail::validate_destination`].
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
    ///
    /// [`Self::validate`] only rejects an empty one; whether the string is a
    /// well-formed address is a question for the rail that will receive it —
    /// see [`PaymentRail::validate_destination`], which answers it offline,
    /// before any money moves.
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

    /// Check a destination address as far as this rail can, **offline**, before
    /// any money moves.
    ///
    /// This exists so a caller can tell a person "that is not a valid Solana
    /// address" at the moment they type it, rather than discovering it at charge
    /// time — or, worse, not at all. It is the one call in the two-party payout
    /// flow described in [`crate::destination`]: the customer supplies an
    /// address they control, this says what is decidably wrong with it, and a
    /// human confirms what no code here can know.
    ///
    /// # Contract
    ///
    /// - **Pure and offline.** No network, no clock, no filesystem, no global
    ///   state; the same `dest` must always give the same verdict. It has to run
    ///   in a browser, on a gate device with no uplink, and in a test with no
    ///   RPC. A check that needs to ask a chain a question (does this account
    ///   exist, does it have a trustline for this asset) is a *different*
    ///   method, and not this one.
    /// - **Never `Result`.** "I could not check" is a verdict
    ///   ([`crate::DestinationStatus::Unknown`]), not an error, because a caller
    ///   must handle it exactly as carefully as a refusal.
    /// - **Fails closed.** A malformed address is
    ///   [`crate::DestinationStatus::Malformed`] — a refusal, never a shrug.
    ///   Conversely a rail must not claim
    ///   [`crate::DestinationStatus::StructurallyValid`] for a check it did not
    ///   actually perform.
    /// - **No verdict means "safe to send to".** patala does not and will not
    ///   detect whether an address is an exchange deposit address; every
    ///   verdict carries [`crate::EXCHANGE_DEPOSIT_CAVEAT`] and
    ///   `human_must_confirm`. See [`DestinationVerdict`].
    ///
    /// # Default
    ///
    /// The default answers [`crate::DestinationStatus::Unknown`] — "a human must
    /// confirm" — for anything non-empty, and
    /// [`crate::DestinationStatus::Malformed`] for a blank string, which is
    /// undeliverable on every rail there is. It is deliberately **not**
    /// `StructurallyValid`: that would silently bless every fiat
    /// processor-side token and every rail that has not written a parser yet.
    /// A rail overrides this only when it genuinely parses its own addresses.
    ///
    /// Like [`Self::verify_webhook`], this is a trait method and not a free
    /// function beside each rail, because a free function is unreachable from
    /// every non-Rust consumer: the UniFFI surface and the sidecar both
    /// dispatch through `dyn PaymentRail`.
    ///
    /// ```
    /// use patala_core::{DestinationStatus, MockRail, PaymentRail, RailClass};
    ///
    /// let rail = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
    ///
    /// // MockRail's synthetic grammar: `<network>:<kind>:<label>`.
    /// let v = rail.validate_destination("mock:wallet:alice");
    /// assert_eq!(v.status, DestinationStatus::StructurallyValid);
    ///
    /// // ...and even then, a human confirms. There is no verdict that skips this.
    /// assert!(v.human_must_confirm);
    /// assert!(!v.exchange_deposit_caveat.is_empty());
    ///
    /// // A refusal is a refusal: do not charge to it.
    /// assert!(rail.validate_destination("not-an-address").is_refusal());
    /// ```
    fn validate_destination(&self, dest: &str) -> DestinationVerdict {
        if dest.trim().is_empty() {
            return DestinationVerdict::malformed(
                self.id(),
                "an empty destination is not an address on any rail",
            );
        }
        DestinationVerdict::unknown(
            self.id(),
            format!(
                "the {} rail does not check destinations offline, so it can neither accept nor \
                 reject this one; a human who controls the receiving wallet must confirm it",
                self.id()
            ),
        )
    }

    /// Reverse a settled payment.
    ///
    /// Rails that cannot reverse a payment — any `NonCustodialFinal` rail, by
    /// definition, since finality is the whole point — MUST return
    /// [`Error::Unsupported`] rather than a stub that appears to work. This is
    /// the default, so a rail only needs to override it if it genuinely can.
    ///
    /// # `Unsupported` does not mean "the customer cannot be paid back"
    ///
    /// It means *this rail cannot undo that transaction*. On a
    /// `NonCustodialFinal` rail, giving the money back is a **compensating
    /// payment**: a second, independent payment in the opposite direction, with
    /// its own transaction, its own fee, its own confirmation, and its own
    /// ability to fail. Nothing about the original payment changes. Conflating
    /// the two would flatten exactly the distinction [`crate::RailClass`] exists
    /// to preserve, which is why it is `charge` that does this and not `refund`:
    ///
    /// 1. Ask the **customer** for a destination and never reuse the address the
    ///    payment came from — that is very often an exchange withdrawal address,
    ///    and an exchange does not credit funds arriving there to the customer
    ///    who withdrew from it. This is what BitPay, Coinbase Commerce and
    ///    OpenNode all do, and it is the correct design rather than a fallback.
    /// 2. Run it through [`Self::validate_destination`] and show a human the
    ///    verdict, its reason and its caveat. A refusal stops there.
    /// 3. Call [`Self::charge`] with that destination, a fresh
    ///    [`PayRequest::reference`] of your own (the payout is its own payment,
    ///    so it needs its own idempotency key), and keep the [`Receipt`] it
    ///    returns — that receipt, not the original one, is the proof the payout
    ///    happened.
    ///
    /// See [`crate::destination`] for why step 1 is not automatable.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::RailClass;
    use crate::destination::DestinationStatus;

    /// A rail that implements *only* the required methods, so what these tests
    /// exercise is the trait's own defaults rather than [`crate::MockRail`]'s
    /// overrides. This is the shape of every rail written against this trait
    /// that has not implemented an address parser — and the point is that such
    /// a rail cannot accidentally claim it validated anything.
    struct BareRail {
        capabilities: RailCapabilities,
    }

    impl BareRail {
        fn new() -> Self {
            Self {
                capabilities: RailCapabilities {
                    class: RailClass::CustodialReversible,
                    reversible: true,
                    requires_kyc: true,
                    holds_funds: true,
                    currencies: vec!["USD".into()],
                    settlement: Settlement::Days(2),
                },
            }
        }
    }

    #[async_trait]
    impl PaymentRail for BareRail {
        fn id(&self) -> &str {
            "bare"
        }
        fn capabilities(&self) -> &RailCapabilities {
            &self.capabilities
        }
        async fn quote(&self, _req: &PayRequest) -> Result<Quote> {
            Err(Error::Unsupported("quote"))
        }
        async fn charge(&self, _req: &PayRequest) -> Result<Receipt> {
            Err(Error::Unsupported("charge"))
        }
        async fn verify(&self, _receipt: &Receipt) -> Result<bool> {
            Ok(false)
        }
    }

    #[test]
    fn the_default_verdict_is_unknown_and_never_structurally_valid() {
        // A default that answered `StructurallyValid` would silently bless every
        // fiat processor-side token and every rail with no parser yet. The
        // honest default is "a human must confirm".
        let rail = BareRail::new();
        let v = rail.validate_destination("cus_XYZ_opaque_processor_token");

        assert_eq!(v.status, DestinationStatus::Unknown);
        assert!(!v.is_refusal(), "unknown is not an established defect");
        assert!(
            v.human_must_confirm,
            "the whole value of Unknown is that it hands the decision to a person"
        );
        assert_eq!(v.rail_id, "bare", "a verdict names whose opinion it is");
        assert!(
            v.reason.contains("bare"),
            "the reason has to be showable to a person: {}",
            v.reason
        );
    }

    #[test]
    fn the_default_still_refuses_a_blank_destination_rather_than_shrugging() {
        // The one thing decidable with no rail knowledge at all. Guards fail
        // closed, so this is Malformed and not Unknown.
        let rail = BareRail::new();
        for blank in ["", " ", "\t\n"] {
            let v = rail.validate_destination(blank);
            assert_eq!(v.status, DestinationStatus::Malformed, "{blank:?}");
            assert!(v.is_refusal(), "{blank:?} must be a refusal");
        }
    }

    #[tokio::test]
    async fn refund_stays_unsupported_on_the_default_and_says_so_by_name() {
        // Audited, not changed: finality is the whole point of a
        // NonCustodialFinal rail. Paying a customer back there is a
        // compensating `charge` to a customer-supplied, validated destination —
        // see this method's docs.
        let rail = BareRail::new();
        let receipt = Receipt {
            rail_id: "bare".into(),
            amount_minor: 100,
            currency: "USD".into(),
            reference: "order-1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        let err = rail
            .refund(&receipt)
            .await
            .expect_err("the default must never fake a reversal");
        assert!(matches!(err, Error::Unsupported("refund")));
    }
}
