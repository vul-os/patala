//! What goes in a Flutterwave [`patala_core::Receipt::proof`].
//!
//! **Same genuine divergence as `paystack::proof`, not an inconsistency**:
//! Flutterwave's `Begin` (cackle's `flutterwave.go`) returns
//! `Charge{Reference: o.Reference, ...}` — the CALLER's own reference,
//! never a separate provider-assigned id. So
//! [`crate::flutterwave::FlutterwaveRail::verify`] operates on
//! `Receipt::reference` directly (as the `tx_ref` Flutterwave's
//! `verify_by_reference` is keyed on); this `proof` only carries the hosted
//! checkout link for caller UI convenience (`patala_core`'s `Receipt` has no
//! redirect-URL field of its own).

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::flutterwave::FlutterwaveRail::charge`]. Not load-bearing for
/// `verify()` — see module docs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_url: Option<String>,
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
