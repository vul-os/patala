//! What goes in a Xendit [`patala_core::Receipt::proof`].
//!
//! **Same shape as Paystack, not an inconsistency** (see `PORTING.md`'s gap
//! list and `paystack::proof`'s module docs): Xendit's own `external_id` IS
//! the SAME string the caller supplies at `Begin`/`charge` time -- Xendit
//! has no separate provider-assigned settlement id that
//! [`crate::xendit::rail::XenditRail::verify`] needs to look up by. So
//! `verify()` operates on `Receipt::reference` directly; this `proof` only
//! carries the invoice URL (and Xendit's own invoice id, for
//! shape-completeness/future use) for caller UI convenience --
//! `patala_core`'s `Receipt` has no redirect-URL field of its own, same
//! reason `stripe::proof::ChargeProof`/`paystack::proof::ChargeProof` carry
//! one -- never as the verification key.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::xendit::rail::XenditRail::charge`]. Not load-bearing for
/// `verify()` -- see module docs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    pub invoice_url: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub invoice_id: String,
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
