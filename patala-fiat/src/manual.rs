//! [`ManualRail`] — ported from cackle's `internal/payments/manual.go`
//! (`ManualProvider`). Cackle's DEFAULT, always-available payment provider:
//! it makes NO network calls, anywhere. This is the fiat-side equivalent of
//! `patala-core::MockRail` being the offline default for the crypto side —
//! see `PATALA.md` §3, §8 and this module's own registry ([`crate::registry`],
//! which never lets `manual` be disabled, mirroring `Registry.IsEnabled`).
//!
//! ## `Provider` -> `PaymentRail` mapping (see `PORTING.md` for the general rules)
//!
//! - cackle's `Begin` (records the order, returns buyer-facing instructions,
//!   moves NO money) maps to [`PaymentRail::charge`]: it returns a `Receipt`
//!   with `amount_minor: 0` (nothing has settled) and the generated
//!   instructions text embedded in `proof` — the same "a `Receipt` is
//!   returned, but `amount_minor` only ever reports what ACTUALLY moved"
//!   honesty pattern `patala-hyperswitch`'s `charge()` already uses for a
//!   pending/redirect card payment (see that crate's `proof.rs`). Retrying
//!   `charge()` for a reference that already began is idempotent, exactly
//!   like cackle's `Begin`: it returns the SAME recorded instructions rather
//!   than re-deriving or resetting state.
//! - cackle's `Verify` (reports the currently recorded state, contacting
//!   nothing) maps to [`PaymentRail::verify`]. Cackle's `Verify` itself does
//!   not compare against a caller-supplied amount/currency (that
//!   reconciliation is a separate, later step — `provider.go`'s `Reconcile`,
//!   called by `HandleWebhook`/`HandleVerify` against a stored `OrderRef`
//!   looked up separately). `patala-core`'s seam has no equivalent external
//!   "stored order" concept — the caller passes back the very `Receipt`
//!   `charge()` handed it — so this port folds that same fail-closed
//!   amount/currency check directly into `verify()`: it fails unless the
//!   rail's OWN recorded amount is at least what the receipt claims and the
//!   currency matches exactly. This is the identical anti-fraud guarantee
//!   cackle's `Reconcile`/`ErrAmountMismatch` gives one layer up, just
//!   enforced at this layer because that is the only layer patala-core's
//!   trait has.
//! - cackle's `MarkPaid`/`MarkFailed` (the ONLY way a manual order ever
//!   settles) have no `PaymentRail` trait equivalent — the trait only has
//!   quote/charge/verify/refund, no "an external operator asserts this
//!   happened" primitive. They are exposed here as extra inherent methods
//!   beyond the trait, exactly as they are `*ManualProvider`-specific
//!   methods in cackle too (not part of the `Provider` interface there
//!   either).
//! - cackle's `Webhook` (unconditionally `ErrManualNoWebhook`) has nothing to
//!   port: an operator marking an order paid by hand has no processor and so
//!   no push delivery to verify. This rail therefore leaves
//!   [`patala_core::PaymentRail::verify_webhook`] at its trait default,
//!   `Err(Error::Unsupported("verify_webhook"))` — the same
//!   "unconditionally unsupported" fact cackle spells with a sentinel
//!   error, spelled here with the trait's own one.
//! - `refund()` uses the trait's default `Err(Error::Unsupported(...))` —
//!   cackle's manual provider has no processor to call a refund against
//!   (`Capabilities.Refunds` defaults to `false`, unset in
//!   `ManualProvider.Capabilities()`), and cackle's `Provider` interface
//!   never had a `Refund` method to begin with.
//!
//! ## Gaps (flagged per this task's brief; see `PORTING.md`)
//!
//! - **`RailClass`**: manual is neither classically `CustodialReversible`
//!   (no processor ever touches the money — it's a direct bank
//!   transfer/cash arrangement between buyer and operator) nor
//!   `NonCustodialFinal` (settlement is an operator's own attestation, fully
//!   reversible via `mark_failed`, with no wallet or finality guarantee at
//!   all). `RailClass` only has these two variants; `CustodialReversible` is
//!   the closer fit (reversible, no wallet, operator-paced settlement) —
//!   `holds_funds: false` below still correctly says no processor, not even
//!   patala itself, ever custodies anything.
//! - **`Settlement`**: has no "whenever a human says so" variant.
//!   `Settlement::Days(0)` is the least-wrong stand-in available, not a real
//!   timing guarantee.
//! - **`requires_kyc`**: cackle's `Capabilities` struct has NO KYC field at
//!   all (`Currencies`/`Countries`/`Flow`/`Refunds`/`Payouts`/`Webhooks`/
//!   `ZeroDecimalOK` only) — `requires_kyc: false` here is a reasonable,
//!   disclosed default (manual has no processor to impose KYC), not a
//!   ported value.
//! - **Not ported**: cackle's `NewManualWithStore`/`RecordStore`-backed
//!   durable persistence (`recordstore.go`). This rail is in-memory-only,
//!   mirroring cackle's simpler `NewManual` constructor — a process restart
//!   loses state, exactly as cackle's own in-memory mode does.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result, Settlement,
};

/// Mirrors cackle's `ProviderNameManual`.
pub const RAIL_ID_MANUAL: &str = "manual";

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Mirrors cackle's `Status`, restricted to what a manual record can be in
/// (there is no separate "settled by the rail but not yet reconciled" state
/// here — manual has no rail-level settlement to be ahead of the operator's
/// own attestation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManualStatus {
    Pending,
    Paid,
    Failed,
}

/// Mirrors cackle's `ManualRecord` — the full recorded state of one manual
/// order, including the audit fields ([`Self::marked_by`]/
/// [`Self::marked_at_unix`]) that a bare `Receipt`/`bool` from
/// [`PaymentRail::verify`] cannot carry. Use [`ManualRail::record`] to read
/// this.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManualRecord {
    pub reference: String,
    pub amount_minor: u64,
    pub currency: String,
    pub status: ManualStatus,
    pub instructions: String,
    pub created_at_unix: u64,
    /// Empty while `status` is still `Pending`. THIS is the auditable "who
    /// marked it paid" field the payments contract requires (mirrors
    /// cackle's `MarkedBy`).
    pub marked_by: String,
    /// Zero while `status` is still `Pending`.
    pub marked_at_unix: u64,
}

/// Mirrors cackle's `ManualInstructions`. Cackle's version takes the full
/// `Order` (which also carries buyer/event/org fields `PayRequest` does
/// not); this takes `&PayRequest` since that is the whole of what is
/// available at this seam — see the module gap notes.
pub type ManualInstructions = Box<dyn Fn(&PayRequest) -> Result<String> + Send + Sync>;

/// Mirrors cackle's `DefaultManualInstructions`, routed through this
/// crate's own currency table ([`crate::currency`]) so a zero- or
/// three-decimal currency is never mis-rendered — exactly the bug class
/// cackle's version exists to avoid.
pub fn default_manual_instructions(req: &PayRequest) -> Result<String> {
    let major = crate::currency::minor_to_major_string(req.amount_minor, &req.currency)
        .map_err(|e| Error::InvalidRequest(format!("manual: {e}")))?;
    let currency = crate::currency::normalize(&req.currency)
        .map_err(|e| Error::InvalidRequest(format!("manual: {e}")))?;
    Ok(format!(
        "Pay {major} {currency}, quoting order reference {}. Ask the operator to confirm and mark this order paid once they've received it.",
        req.reference
    ))
}

/// Embedded in a manual `Receipt::proof` by [`ManualRail::charge`]. Not a
/// cryptographic signature — there is nothing to sign; the rail's own
/// in-memory record is the source of truth `verify()` re-checks against,
/// the same "processor/operator is the source of truth, not a client-side
/// signature" shape `patala-hyperswitch`'s `ChargeProof` uses for a
/// custodial rail.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ManualProof {
    instructions: String,
}

impl ManualProof {
    fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ManualProof always serializes")
    }
}

struct State {
    records: HashMap<String, ManualRecord>,
}

/// Mirrors cackle's `ManualProvider`. See the module docs for the full
/// `Provider` -> `PaymentRail` mapping.
pub struct ManualRail {
    instructions: ManualInstructions,
    state: Mutex<State>,
    capabilities: RailCapabilities,
}

impl ManualRail {
    /// `instructions_fn` generates the buyer-facing text for a given
    /// request; pass `None` to use [`default_manual_instructions`] (mirrors
    /// cackle's `NewManual(nil)`).
    pub fn new(instructions_fn: Option<ManualInstructions>) -> Self {
        Self {
            instructions: instructions_fn.unwrap_or_else(|| Box::new(default_manual_instructions)),
            state: Mutex::new(State {
                records: HashMap::new(),
            }),
            capabilities: RailCapabilities {
                class: RailClass::CustodialReversible, // see module "Gaps" note
                reversible: false,   // mirrors cackle's unset Capabilities.Refunds
                requires_kyc: false, // no cackle equivalent field -- see "Gaps"
                holds_funds: false,  // no processor at all; see PATALA.md §1, §8
                currencies: Vec::new(), // empty == every currency, mirrors cackle's `Currencies: nil`
                settlement: Settlement::Days(0), // see module "Gaps" note
                atomic_multi_party: false, // always false: N payouts here are N independent API calls, never one atomic event (B3)
            },
        }
    }

    /// Mirrors cackle's `MarkPaid`. `marked_by` must be non-empty —
    /// recording WHO marked an order paid is the entire point of this
    /// rail's audit trail. Idempotent: re-marking an already-paid reference
    /// does not overwrite the original `marked_by`/`marked_at_unix` — the
    /// FIRST mark is what the audit trail reflects, exactly as in cackle.
    pub fn mark_paid(&self, reference: &str, marked_by: &str) -> Result<ManualRecord> {
        self.mark(reference, marked_by, ManualStatus::Paid)
    }

    /// Mirrors cackle's `MarkFailed`: lets an operator explicitly record
    /// that a manual order will never be paid. Same rules as
    /// [`Self::mark_paid`].
    pub fn mark_failed(&self, reference: &str, marked_by: &str) -> Result<ManualRecord> {
        self.mark(reference, marked_by, ManualStatus::Failed)
    }

    fn mark(&self, reference: &str, marked_by: &str, status: ManualStatus) -> Result<ManualRecord> {
        let reference = reference.trim();
        let marked_by = marked_by.trim();
        if reference.is_empty() {
            return Err(Error::InvalidRequest(
                "manual: reference is required".into(),
            ));
        }
        if marked_by.is_empty() {
            return Err(Error::InvalidRequest(
                "manual: marked_by is required (this is an auditable action and needs an actor)"
                    .into(),
            ));
        }
        let mut state = self.state.lock().expect("manual rail lock poisoned");
        let rec = state.records.get_mut(reference).ok_or_else(|| {
            Error::InvalidRequest(format!("manual: unknown reference {reference:?}"))
        })?;

        if status == ManualStatus::Paid && rec.status == ManualStatus::Paid {
            // Idempotent: don't clobber the original marker/timestamp.
            return Ok(rec.clone());
        }
        rec.status = status;
        rec.marked_by = marked_by.to_string();
        rec.marked_at_unix = now_unix();
        Ok(rec.clone())
    }

    /// Mirrors cackle's `Record` — the full recorded state, including the
    /// audit fields `verify()` cannot surface through a bare bool.
    pub fn record(&self, reference: &str) -> Option<ManualRecord> {
        self.state
            .lock()
            .expect("manual rail lock poisoned")
            .records
            .get(reference.trim())
            .cloned()
    }
}

impl Default for ManualRail {
    fn default() -> Self {
        Self::new(None)
    }
}

#[async_trait]
impl PaymentRail for ManualRail {
    fn id(&self) -> &str {
        RAIL_ID_MANUAL
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::ignored`], because the `manual` rail never reads
    /// `destination` at all (see this module's docs above). The field exists
    /// on the request only because `PayRequest::validate()` requires a
    /// non-empty one on every rail.
    ///
    /// The verdict is therefore always
    /// [`patala_core::DestinationStatus::Unknown`] for a non-empty string:
    /// there is no format to be right or wrong about, so no format check is
    /// invented — including no refusal of a wallet address, which is
    /// genuinely harmless in a field nothing reads. What the reason says
    /// instead is the thing a caller needs to know: setting this steers
    /// nothing, and giving a customer their money back on this rail is the
    /// processor's refund path, never a charge to a destination.
    fn validate_destination(&self, dest: &str) -> patala_core::DestinationVerdict {
        crate::destination::ignored(self.id(), dest)
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        crate::currency::validate(&req.currency)
            .map_err(|e| Error::InvalidRequest(format!("manual: {e}")))?;
        // Cackle has no pre-charge fee-quote concept for manual either (an
        // operator's own bank/cash arrangement has no rail fee to report) --
        // honestly zero, mirroring patala-hyperswitch's same "never
        // fabricate a fee it cannot obtain" quote.
        Ok(Quote {
            rail_id: RAIL_ID_MANUAL.to_string(),
            amount_minor: req.amount_minor,
            currency: req.currency.clone(),
            fee_minor: 0,
            total_minor: req.amount_minor,
            settlement: self.capabilities.settlement,
            expires_at_unix: now_unix().saturating_add(300),
        })
    }

    async fn charge(&self, req: &PayRequest) -> Result<Receipt> {
        req.validate()?;
        let currency = crate::currency::normalize(&req.currency)
            .map_err(|e| Error::InvalidRequest(format!("manual: {e}")))?;

        let mut state = self.state.lock().expect("manual rail lock poisoned");

        // Idempotent on retry, mirroring cackle's `Begin`: return the SAME
        // recorded instructions/state rather than erroring or resetting it.
        if let Some(existing) = state.records.get(&req.reference) {
            let proof = ManualProof {
                instructions: existing.instructions.clone(),
            }
            .to_bytes();
            let amount_minor = if existing.status == ManualStatus::Paid {
                existing.amount_minor
            } else {
                0
            };
            return Ok(Receipt {
                rail_id: RAIL_ID_MANUAL.to_string(),
                amount_minor,
                currency: existing.currency.clone(),
                reference: existing.reference.clone(),
                proof,
                settled_at_unix: existing.marked_at_unix,
            });
        }

        let mut req_norm = req.clone();
        req_norm.currency = currency.clone();
        let text = (self.instructions)(&req_norm)?;
        if text.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "manual: instructions must not be empty".into(),
            ));
        }

        let rec = ManualRecord {
            reference: req.reference.clone(),
            amount_minor: req.amount_minor,
            currency: currency.clone(),
            status: ManualStatus::Pending,
            instructions: text.clone(),
            created_at_unix: now_unix(),
            marked_by: String::new(),
            marked_at_unix: 0,
        };
        state.records.insert(req.reference.clone(), rec);

        Ok(Receipt {
            rail_id: RAIL_ID_MANUAL.to_string(),
            amount_minor: 0, // nothing has settled yet -- see module docs
            currency,
            reference: req.reference.clone(),
            proof: ManualProof { instructions: text }.to_bytes(),
            settled_at_unix: 0,
        })
    }

    async fn verify(&self, receipt: &Receipt) -> Result<bool> {
        // Fail closed: a receipt naming a different rail, or an unknown
        // reference, is never assumed valid -- mirrors cackle's `Verify`
        // failing closed on an unrecognised reference.
        if receipt.rail_id != RAIL_ID_MANUAL {
            return Ok(false);
        }
        let state = self.state.lock().expect("manual rail lock poisoned");
        let Some(rec) = state.records.get(&receipt.reference) else {
            return Ok(false);
        };
        // Only StatusPaid (an explicit mark_paid) may ever verify -- exactly
        // as cackle's own comment on `Status::Paid` states: "This is the
        // ONLY status that should ever cause tickets to be issued."
        if rec.status != ManualStatus::Paid {
            return Ok(false);
        }
        if !rec.currency.eq_ignore_ascii_case(&receipt.currency) {
            return Ok(false);
        }
        // Fold in cackle's `Reconcile`/`ErrAmountMismatch` anti-fraud check
        // (see module docs): the rail's own recorded total must be at least
        // what this receipt claims -- `>=`, not `==`, because a genuine
        // pending receipt from `charge()` legitimately claims
        // `amount_minor: 0` and must still verify true once marked paid.
        if rec.amount_minor < receipt.amount_minor {
            return Ok(false);
        }
        Ok(true)
    }

    // refund(): trait default `Err(Error::Unsupported("refund"))` --
    // manual has no processor to call and cackle's own Provider interface
    // never had a Refund method either. See module docs.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(amount: u64, currency: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: "unused-for-manual".into(),
            reference: reference.into(),
        }
    }

    /// The `manual` rail is not one of the feature-gated adapter directories,
    /// so `tests/webhook_coverage.rs`'s destination sweep never reaches it.
    /// It gets the same assertions here instead — the always-on default rail
    /// is the last one that should be the exception.
    #[test]
    fn validate_destination_says_the_field_steers_nothing_and_never_blesses_it() {
        use patala_core::DestinationStatus;

        let rail = ManualRail::default();

        let v = rail.validate_destination("unused-for-manual");
        assert_eq!(v.status, DestinationStatus::Unknown);
        assert!(!v.is_refusal());
        assert_eq!(v.rail_id, RAIL_ID_MANUAL);
        assert!(v.reason.contains("never reads"), "{}", v.reason);

        // A blank one is still a refusal: `PayRequest::validate()` rejects it,
        // so accepting it here would make the two disagree.
        for blank in ["", " ", "\t\n"] {
            let v = rail.validate_destination(blank);
            assert_eq!(v.status, DestinationStatus::Malformed, "{blank:?}");
            assert!(v.is_refusal(), "{blank:?}");
        }

        // And no input ever reaches `StructurallyValid` — `manual` settles
        // cash in a hand, and has no address to call well-formed.
        for dest in [
            "unused-for-manual",
            "6dNVeXf5rQrTVAvpjTv2oyeHiWMCGSCUuUkxYCK6bZTs",
            "https://example.com/thanks",
            "",
        ] {
            let v = rail.validate_destination(dest);
            assert_ne!(v.status, DestinationStatus::StructurallyValid, "{dest:?}");
            assert!(v.human_must_confirm, "{dest:?}");
            assert_eq!(
                v.exchange_deposit_caveat,
                patala_core::EXCHANGE_DEPOSIT_CAVEAT,
                "{dest:?}"
            );
        }
    }

    // Ported from cackle's internal/payments/manual_test.go.

    #[tokio::test]
    async fn capabilities_are_unrestricted_and_honest() {
        let rail = ManualRail::default();
        let caps = rail.capabilities();
        assert!(
            !caps.holds_funds,
            "no processor ever custodies manual funds"
        );
        assert!(caps.currencies.is_empty(), "manual supports every currency");
        assert_eq!(rail.id(), RAIL_ID_MANUAL);
    }

    #[tokio::test]
    async fn charge_returns_pending_receipt_with_instructions() {
        let rail = ManualRail::default();
        let receipt = rail.charge(&req(12345, "USD", "ord_1")).await.unwrap();
        assert_eq!(receipt.reference, "ord_1");
        assert_eq!(receipt.amount_minor, 0, "nothing has settled yet");
        assert!(!receipt.proof.is_empty());
    }

    #[tokio::test]
    async fn charge_is_idempotent_on_retry() {
        let rail = ManualRail::default();
        let first = rail.charge(&req(500, "USD", "ord_1")).await.unwrap();
        // A different (buggy) amount/currency on retry must not change the
        // recorded state -- charge() returns the ALREADY recorded receipt.
        let second = rail.charge(&req(999_999, "EUR", "ord_1")).await.unwrap();
        assert_eq!(first.proof, second.proof);
        assert_eq!(second.currency, "USD");
    }

    #[tokio::test]
    async fn charge_rejects_invalid_currency() {
        let rail = ManualRail::default();
        assert!(rail.charge(&req(100, "ZZZ", "ord_1")).await.is_err());
    }

    #[tokio::test]
    async fn charge_zero_decimal_currency_formats_correctly() {
        let rail = ManualRail::default();
        let receipt = rail.charge(&req(1000, "JPY", "ord_jpy")).await.unwrap();
        let proof: serde_json::Value = serde_json::from_slice(&receipt.proof).unwrap();
        let instructions = proof["instructions"].as_str().unwrap();
        assert!(
            instructions.contains("1000 JPY"),
            "instructions = {instructions:?}, want it to contain '1000 JPY' (not '10.00')"
        );
    }

    #[tokio::test]
    async fn charge_with_custom_instructions() {
        let rail = ManualRail::new(Some(Box::new(|req| {
            Ok(format!("Bank transfer, ref {}", req.reference))
        })));
        let receipt = rail.charge(&req(100, "USD", "ord_1")).await.unwrap();
        let proof: serde_json::Value = serde_json::from_slice(&receipt.proof).unwrap();
        assert_eq!(proof["instructions"], "Bank transfer, ref ord_1");
    }

    #[tokio::test]
    async fn verify_is_false_before_mark_paid() {
        let rail = ManualRail::default();
        let receipt = rail.charge(&req(100, "USD", "ord_1")).await.unwrap();
        assert!(
            !rail.verify(&receipt).await.unwrap(),
            "pending order must not verify"
        );
    }

    #[tokio::test]
    async fn verify_unknown_reference_fails_closed() {
        let rail = ManualRail::default();
        let fake = Receipt {
            rail_id: RAIL_ID_MANUAL.to_string(),
            amount_minor: 0,
            currency: "USD".to_string(),
            reference: "never-began".to_string(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&fake).await.unwrap());
    }

    #[tokio::test]
    async fn mark_paid_records_who_and_when_then_verifies() {
        let rail = ManualRail::default();
        let receipt = rail.charge(&req(100, "USD", "ord_1")).await.unwrap();

        let rec = rail.mark_paid("ord_1", "operator@example.com").unwrap();
        assert_eq!(rec.status, ManualStatus::Paid);
        assert_eq!(rec.marked_by, "operator@example.com");
        assert_ne!(rec.marked_at_unix, 0);

        assert!(
            rail.verify(&receipt).await.unwrap(),
            "the original (amount_minor=0) pending receipt must verify true once marked paid"
        );
    }

    #[tokio::test]
    async fn mark_paid_requires_marked_by() {
        let rail = ManualRail::default();
        rail.charge(&req(100, "USD", "ord_1")).await.unwrap();
        assert!(rail.mark_paid("ord_1", "").is_err());
    }

    #[tokio::test]
    async fn mark_paid_unknown_reference_fails_closed() {
        let rail = ManualRail::default();
        assert!(rail
            .mark_paid("never-began", "operator@example.com")
            .is_err());
    }

    #[tokio::test]
    async fn mark_paid_is_idempotent_and_keeps_original_audit_trail() {
        let rail = ManualRail::default();
        rail.charge(&req(100, "USD", "ord_1")).await.unwrap();

        let first = rail.mark_paid("ord_1", "alice@example.com").unwrap();
        let second = rail.mark_paid("ord_1", "bob@example.com").unwrap();
        assert_eq!(second.marked_at_unix, first.marked_at_unix);
        assert_eq!(rail.record("ord_1").unwrap().marked_by, "alice@example.com");
    }

    #[tokio::test]
    async fn mark_failed_records_audit_and_never_verifies() {
        let rail = ManualRail::default();
        let receipt = rail.charge(&req(100, "USD", "ord_1")).await.unwrap();
        let rec = rail.mark_failed("ord_1", "operator@example.com").unwrap();
        assert_eq!(rec.status, ManualStatus::Failed);
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_fails_closed_on_inflated_receipt_amount() {
        // Anti-fraud check folded in from cackle's Reconcile/ErrAmountMismatch
        // (see module docs): a receipt claiming MORE than the rail's own
        // recorded total must never verify, even once marked paid.
        let rail = ManualRail::default();
        rail.charge(&req(100, "USD", "ord_1")).await.unwrap();
        rail.mark_paid("ord_1", "operator@example.com").unwrap();

        let existing = rail.record("ord_1").unwrap();
        let forged = Receipt {
            rail_id: RAIL_ID_MANUAL.to_string(),
            amount_minor: 999_999,
            currency: existing.currency.clone(),
            reference: "ord_1".to_string(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&forged).await.unwrap());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = ManualRail::default();
        let receipt = rail.charge(&req(100, "USD", "ord_1")).await.unwrap();
        let err = rail.refund(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }
}
