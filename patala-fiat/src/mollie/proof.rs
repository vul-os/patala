//! What goes in a Mollie [`patala_core::Receipt::proof`].
//!
//! **Flagged divergence from cackle, resolving an apparent inconsistency in
//! cackle's own source** (see `rail.rs`'s module docs for the full
//! reasoning): cackle's `mollie.go` `Begin` sets `Charge.Reference =
//! o.Reference` (confirmed by cackle's own `TestMollieBegin_Success`, which
//! asserts `charge.Reference == "ord_1"`, the CALLER's reference) -- yet the
//! doc comment on cackle's `Verify` claims "`reference` is a Mollie payment
//! id (`tr_...`), which is exactly what `Charge.Reference` is set to by
//! `Begin` above". These two statements contradict each other in cackle's
//! own file: `parsed.ID` (the real Mollie payment id) is validated
//! non-empty in `Begin` but then never actually propagated anywhere the
//! caller can retrieve it.
//!
//! `patala_core::Receipt::reference` is documented as echoing
//! `PayRequest::reference` (the caller's own key) -- this port follows that
//! contract, exactly as every other rail in this crate does -- and carries
//! Mollie's OWN payment id in `proof` instead, the same "processor's own
//! record locator, in proof" pattern `stripe::proof::ChargeProof` already
//! uses for its `session_id`. This is not "fixing a bug" so much as
//! correctly applying `patala_core`'s own seam (which, unlike cackle's
//! `Charge`, has a `proof` field for exactly this) -- and it is what makes
//! `verify()` actually able to look the right payment up, which cackle's own
//! field choice structurally could not do.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::mollie::rail::MollieRail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// Mollie's own payment id (`tr_...`) -- see module docs.
    pub payment_id: String,
    /// The Mollie-hosted checkout URL, kept for caller UI convenience --
    /// `patala_core::Receipt` has no redirect-URL field of its own, same
    /// reason `stripe::proof::ChargeProof` carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_url: Option<String>,
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
/// [`crate::mollie::rail::MollieRail::refund`].
///
/// **Not ported from cackle** (cackle's `Provider` interface has no
/// `Refund` method at all -- see `rail.rs`'s module docs): new code grounded
/// in Mollie's own public Create Refund API
/// (<https://docs.mollie.com/reference/create-refund>).
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
