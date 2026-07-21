//! What goes in a PayPal [`patala_core::Receipt::proof`].
//!
//! **A genuine, disclosed discrepancy in cackle's own code, resolved the
//! same way `stripe::proof` resolves its analogous one — not a redesign of
//! PayPal's payment logic.** cackle's `Begin` returns
//! `Charge{Reference: o.Reference}` (the CALLER's own order reference), but
//! cackle's `Verify(reference string)` doc comment says *"reference is a
//! PayPal order id (not Cackle's own order reference)"* and its own test
//! (`TestPayPalVerify_ApprovedThenCaptured`) calls `p.Verify(ctx, "ORDER1")`
//! using PayPal's own order id, never `o.Reference`. Whatever cackle's
//! higher-level `httpapi` orchestration does to reconcile this (persisting
//! BOTH values somewhere outside this package) is not visible in
//! `internal/payments` itself. `patala_core::Receipt` has an explicit,
//! documented separation `stripe.go`'s cackle code doesn't: `reference`
//! (always the caller's own `PayRequest::reference` — see
//! `patala_core::Receipt`'s own doc comment) and an opaque `proof` blob for
//! whatever a rail's OWN re-verification needs. This port uses that
//! separation exactly as `stripe::proof::ChargeProof` documents for its own
//! session-id-vs-reference gap: the PayPal order id lives in `proof`,
//! `Receipt::reference` is always `req.reference`, and `verify()`/`refund()`
//! always look the order up via `proof`, then check the CAPTURED purchase
//! unit's own `custom_id`/`reference_id` against `receipt.reference` before
//! ever reporting success — performing directly, inline, what cackle's
//! separate `Reconcile`/`HandleVerify` orchestration (which this crate has
//! no equivalent seam for) would otherwise do. The actual PayPal API calls
//! (create order, get order, capture order) and status-mapping logic are
//! byte-for-byte the same as cackle's.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::paypal::PayPalRail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The PayPal-assigned order id — the stable key `verify()`/`refund()`
    /// re-fetch by. See module docs.
    pub paypal_order_id: String,
    /// The buyer's PayPal approval redirect URL, carried here for caller UI
    /// convenience (`patala_core::Receipt` has no redirect-URL field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approve_url: Option<String>,
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
/// [`crate::paypal::PayPalRail::refund`].
///
/// **Not ported from cackle** (`internal/payments/provider.go`'s `Provider`
/// interface has no `Refund` method at all) — new code grounded directly in
/// PayPal's own public Captures Refund API
/// (<https://developer.paypal.com/docs/api/payments/v2/#captures_refund>),
/// following the same "processor's own record locator, re-checked" shape as
/// [`ChargeProof`] and `stripe::proof::RefundProof`.
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
