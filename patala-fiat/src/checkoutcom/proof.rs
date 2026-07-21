//! What goes in a Checkout.com [`patala_core::Receipt::proof`].
//!
//! Same seam-plumbing pattern as `stripe::proof` (see that module's own doc
//! comment): Checkout.com's own payment id (`pay_...`) is the value
//! `verify()`/`refund()` must look up by — Checkout.com's API has no
//! documented "look up a payment by an arbitrary merchant reference" GET
//! endpoint (only a payment-request search API with different semantics),
//! mirroring cackle's own `checkoutcom.go` `Verify` doc comment exactly. So
//! this proof embeds the payment id, and `verify()`/`refund()` always look
//! it up from here, never from a caller-supplied bare string.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::checkoutcom::rail::CheckoutComRail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The Checkout.com Hosted Payments Page session id (see module docs).
    pub payment_id: String,
    /// Best-effort snapshot at charge time; `verify()` never trusts this
    /// alone and always re-fetches from Checkout.com.
    pub status_at_charge: String,
    /// The hosted payment page URL, kept for caller UI convenience --
    /// `patala_core::Receipt` has no redirect-URL field of its own, same
    /// reason `stripe::proof::ChargeProof` carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
/// [`crate::checkoutcom::rail::CheckoutComRail::refund`].
///
/// **Not ported from cackle** (cackle's `Provider` interface has no
/// `Refund` method at all — see `rail.rs`'s module docs): new code grounded
/// in Checkout.com's own public Refund API
/// (<https://checkout.com/docs/payments/manage-payments/refund-a-payment>).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefundProof {
    /// The refund's own `action_id`, per Checkout.com's documented 202
    /// Accepted response — see `rail.rs`'s module docs on why this port
    /// always reports the resulting `Receipt` as still-pending.
    pub action_id: String,
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
