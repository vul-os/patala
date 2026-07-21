//! What goes in a PayU [`patala_core::Receipt::proof`].
//!
//! PayU's `Begin` makes NO network call (see `rail.rs`'s module docs) — it
//! only computes a request hash and builds a field set for a
//! hidden-auto-submitting HTML form the caller renders client-side
//! (mirrors cackle's `Charge.RedirectURL` + `Charge.Instructions`). This
//! `proof` carries exactly that: the URL-encoded field set (including the
//! hash) and the constant checkout form-action URL, purely for caller
//! convenience (`patala_core::Receipt` has no redirect-URL or
//! form-instructions field of its own — same reasoning as every other
//! `ChargeProof` in this crate). Verification never trusts this proof as a
//! signature; PayU's Verify Payment API (looked up by `Receipt::reference`
//! directly, exactly like `paystack::proof::ChargeProof`'s identical
//! reasoning) is the source of truth `verify()` re-fetches from — this
//! `proof` is NOT load-bearing for `verify()`/`refund()`.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::payu::PayURail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The full PayU hosted-checkout form field set, URL-encoded (`key`,
    /// `txnid`, `amount`, `productinfo`, `firstname`, `email`, `hash`) —
    /// mirrors cackle's `Charge.Instructions`. The caller renders this as a
    /// hidden auto-submitting form POSTing to [`Self::checkout_url`].
    pub fields: String,
    /// The constant PayU hosted-checkout form-action URL — mirrors cackle's
    /// `Charge.RedirectURL` (`payUCheckoutURL`). Always the same value;
    /// carried here rather than as a `Receipt` field only because
    /// `patala_core::Receipt` has no redirect-URL field of its own.
    pub checkout_url: String,
}

impl ChargeProof {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ChargeProof always serializes")
    }

    #[allow(dead_code)]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}
