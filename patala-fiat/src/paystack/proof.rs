//! What goes in a Paystack [`patala_core::Receipt::proof`].
//!
//! **A genuine divergence from Stripe/Hyperswitch, not an inconsistency**
//! (see `PORTING.md`'s gap list and `rail.rs`'s module docs): Paystack's own
//! transaction `reference` is the SAME string the caller supplies at
//! `Begin`/`charge` time — Paystack has no separate provider-assigned
//! settlement id the way a Stripe Checkout Session id or a Hyperswitch
//! `payment_id` is. So [`crate::paystack::PaystackRail::verify`] and
//! `::refund` operate on `Receipt::reference` directly; this `proof` only
//! carries the authorization URL for caller UI convenience (`patala_core`'s
//! `Receipt` has no redirect-URL field of its own — same reason
//! `patala-hyperswitch::ChargeProof` and this crate's
//! `stripe::proof::ChargeProof` carry one), never as the verification key.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::paystack::PaystackRail::charge`]. Not load-bearing for
/// `verify()`/`refund()` — see module docs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    pub authorization_url: String,
    #[serde(default)]
    pub access_code: String,
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
