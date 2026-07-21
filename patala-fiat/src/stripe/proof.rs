//! What goes in a Stripe [`patala_core::Receipt::proof`].
//!
//! **Where this crate's Receipt-based seam sidesteps a gap cackle's own
//! `Verify` openly admits to** (see `PORTING.md`'s gap list and
//! `stripe/rail.rs`'s module docs): cackle's `stripe.go` has a long comment
//! above `Verify` admitting an unresolved ambiguity — its `Verify(reference
//! string)` cannot tell whether `reference` is Cackle's own order reference
//! or Stripe's session id (`cs_...`), because cackle's v1 `Provider`
//! interface only ever hands `Verify` a bare string. `patala_core::Receipt`
//! carries BOTH the caller's own `reference` field AND an opaque
//! rail-specific `proof` blob — so this port embeds Stripe's real session id
//! in `proof` (exactly the pattern `patala-hyperswitch::ChargeProof`
//! already uses for its `payment_id`), and `verify()` always looks up by
//! THAT embedded id, never by a caller-supplied bare string. This is a
//! difference in the SEAM/plumbing forced by the different trait shapes,
//! not a change to the Stripe API calls, JSON parsing, or settlement-status
//! mapping, which are byte-for-byte the same as cackle's.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::stripe::StripeRail::charge`]. Never treated as a cryptographic
/// signature — Stripe's own session record is the source of truth `verify()`
/// re-fetches, exactly the pattern `patala-hyperswitch::proof::ChargeProof`
/// documents for a custodial rail.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The Stripe Checkout Session id (`cs_...`) — see module docs.
    pub session_id: String,
    /// Stripe's `payment_status` at charge time (`"paid"` /
    /// `"unpaid"` / `"no_payment_required"`). May already be stale by the
    /// time anyone reads it back; `verify()` never trusts this alone and
    /// always re-fetches from Stripe.
    pub status_at_charge: String,
    /// Present only when the session is a hosted-page redirect (Stripe
    /// Checkout always is, so this is always populated by `charge()`, but
    /// kept `Option` for symmetry with `patala-hyperswitch::ChargeProof` and
    /// in case a future non-redirect Stripe flow is added). `patala_core`'s
    /// `Receipt` has no redirect-URL field of its own — carried here so a
    /// caller that deserializes this proof itself can find the checkout URL
    /// without a second API call. `patala_core` treats `proof` as fully
    /// opaque either way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_url: Option<String>,
}

impl ChargeProof {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ChargeProof always serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Serialized into the `proof` of the *new* `Receipt` returned by
/// [`crate::stripe::StripeRail::refund`].
///
/// **Not ported from cackle** (see `stripe/rail.rs`'s module docs on
/// `refund`): cackle's `Provider` interface never had a `Refund` method, so
/// there is no Go logic to mirror here — this is new code grounded directly
/// in Stripe's own public Refunds API
/// (<https://docs.stripe.com/api/refunds/create>,
/// <https://docs.stripe.com/api/refunds/object>), following the same
/// "processor's own record locator, re-checked, never a client-side
/// signature" shape as [`ChargeProof`] and
/// `patala-hyperswitch::proof::RefundProof`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefundProof {
    pub refund_id: String,
    pub status_at_refund: String,
}

impl RefundProof {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("RefundProof always serializes")
    }

    #[allow(dead_code)]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}
