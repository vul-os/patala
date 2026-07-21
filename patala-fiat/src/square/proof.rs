//! What goes in a Square [`patala_core::Receipt::proof`].
//!
//! **A genuine structural gap, not an inconsistency** (see `PORTING.md`'s
//! gap list, `rail.rs`'s module docs, and cackle's own file-header HONESTY
//! note 3): Square's Payment Links API creates an underlying Order, but the
//! PAYMENT id -- the only thing Square's Payments API can be queried by --
//! is not known until the buyer actually pays, delivered via a
//! `payment.updated` webhook. Cackle's own comment states there is no
//! confirmed "look up a payment by our own reference_id" endpoint
//! independent of the Order. So unlike Stripe (whose session id IS known at
//! charge time and gets embedded here immediately) or Paystack (whose own
//! reference IS the lookup key), Square's `payment_id` genuinely does not
//! exist yet when [`crate::square::SquareRail::charge`] returns.
//!
//! [`ChargeProof::payment_id`] therefore starts `None` and can only become
//! `Some` once a caller has received and verified a `payment.updated`
//! webhook (via [`crate::square::webhook::verify_and_parse`], which resolves
//! the real id) and re-embedded it via
//! [`ChargeProof::with_resolved_payment_id`] before calling
//! [`crate::square::SquareRail::verify`] again. Until that happens,
//! `verify()` honestly returns `Ok(false)` -- not because a check failed,
//! but because there is nothing yet to check (see `rail.rs`'s module docs
//! and its `verify()` implementation).

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::square::SquareRail::charge`]. See module docs for the
/// `payment_id` gap.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The Square Order id created alongside the payment link.
    pub order_id: String,
    /// The Square Payment Link id `charge()` created.
    pub payment_link_id: String,
    /// The real Square Payment id -- `None` until resolved from a
    /// `payment.updated` webhook. See module docs.
    #[serde(default)]
    pub payment_id: Option<String>,
}

impl ChargeProof {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ChargeProof always serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }

    /// **NOT a cackle port** -- new plumbing, absent from cackle's Go
    /// entirely, that exists purely so a caller can round-trip the gap
    /// documented above: after receiving a verified `payment.updated`
    /// webhook, clone this proof with the now-known `payment_id` filled in,
    /// re-embed it into a `Receipt` carrying the SAME `rail_id`/
    /// `amount_minor`/`currency`/`reference` the original `charge()`
    /// issued, and call [`crate::square::SquareRail::verify`] again to have
    /// this rail re-confirm directly against Square.
    pub fn with_resolved_payment_id(&self, payment_id: String) -> Self {
        Self {
            order_id: self.order_id.clone(),
            payment_link_id: self.payment_link_id.clone(),
            payment_id: Some(payment_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_bytes() {
        let proof = ChargeProof {
            order_id: "ORDER1".into(),
            payment_link_id: "PLINK1".into(),
            payment_id: None,
        };
        let back = ChargeProof::from_bytes(&proof.to_bytes()).unwrap();
        assert_eq!(back.order_id, "ORDER1");
        assert_eq!(back.payment_id, None);
    }

    #[test]
    fn with_resolved_payment_id_sets_it_without_disturbing_the_rest() {
        let proof = ChargeProof {
            order_id: "ORDER1".into(),
            payment_link_id: "PLINK1".into(),
            payment_id: None,
        };
        let resolved = proof.with_resolved_payment_id("pay_1".into());
        assert_eq!(resolved.payment_id.as_deref(), Some("pay_1"));
        assert_eq!(resolved.order_id, "ORDER1");
        assert_eq!(resolved.payment_link_id, "PLINK1");
    }
}
