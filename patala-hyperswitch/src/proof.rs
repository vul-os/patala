//! What goes in a Hyperswitch [`patala_core::Receipt::proof`].
//!
//! `patala-core` documents `proof` as "opaque to `patala-core` -- only the
//! rail that issued a given receipt knows how to re-verify it" (see
//! `patala-core/src/rail.rs`). For a custodial rail there is no signature to
//! construct client-side -- the *processor* is the source of truth. So this
//! rail's "proof" is simply enough of the processor's own record locator
//! (its `payment_id`, and the status snapshot at the time `charge()` or
//! `refund()` returned) for [`crate::HyperswitchRail::verify`] to go back and
//! ask Hyperswitch "is this still true?" -- exactly how a real Stripe/
//! Paystack/etc. integration works today, just fronted by Hyperswitch.
//!
//! **This is the honest expression of the pending/redirect lifecycle.** A
//! card payment that comes back `requires_customer_action` is not settled --
//! this crate still returns `Ok(Receipt)` from `charge()` (the trait gives it
//! no other option; see `patala-core/src/rail.rs`'s own doc on `Receipt`:
//! "gate on `verify` ... never on `charge` merely having returned `Ok`"), but
//! the embedded status says `requires_customer_action`, and
//! [`crate::HyperswitchRail::verify`] re-fetches fresh from Hyperswitch and
//! returns `Ok(false)` for anything other than `succeeded` -- so a caller
//! that (correctly) gates on `verify` per the seam's own contract can never
//! be fooled into treating a pending payment as settled.

use serde::{Deserialize, Serialize};

use crate::models::IntentStatus;

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::HyperswitchRail::charge`]. Never treated as a cryptographic
/// signature -- see the module docs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ChargeProof {
    pub payment_id: String,
    /// Status snapshot *at charge time* -- may already be stale by the time
    /// anyone reads it back; `verify()` never trusts this field alone and
    /// always re-fetches.
    pub status_at_charge: IntentStatus,
    /// Present only when Hyperswitch returned a 3DS/redirect next-action.
    /// Carried through so a UI-facing caller that deserializes this proof
    /// itself (this crate does not police that; `proof` is `Vec<u8>` to
    /// `patala-core`) can find the redirect URL without a second API call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_to_url: Option<String>,
}

/// Serialized into the `proof` of the *new* `Receipt` returned by
/// [`crate::HyperswitchRail::refund`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RefundProof {
    pub refund_id: String,
    pub payment_id: String,
    pub status_at_refund: crate::models::RefundStatus,
}

impl ChargeProof {
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        // Infallible: every field is a plain String/enum, never a float, map
        // key, or anything else `serde_json` can fail to encode.
        serde_json::to_vec(self).expect("ChargeProof always serializes")
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

impl RefundProof {
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("RefundProof always serializes")
    }

    /// Mirrors [`ChargeProof::from_bytes`]. Not called by this crate's own
    /// `PaymentRail` impl today (a refund `Receipt` is terminal -- nothing
    /// re-verifies a refund the way `verify()` re-derives a charge), but kept
    /// symmetric so a consumer that stores a refund receipt can round-trip
    /// it the same way. Exercised by this crate's own tests.
    #[allow(dead_code)]
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}
