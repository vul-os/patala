//! What goes in a Yoco [`patala_core::Receipt::proof`].
//!
//! **Same genuine divergence as `stripe::proof`/`iyzico::proof`, not an
//! inconsistency**: cackle's `Begin` returns `Charge{Reference: parsed.ID,
//! ...}` — Yoco's OWN checkout id, not `o.Reference`. `Receipt::reference`
//! stays the CALLER's own reference (per `patala_core::Receipt`'s
//! contract); Yoco's real tracking id lives here in `proof`, and
//! `verify()` always looks it up from there.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::yoco::YocoRail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The Yoco-assigned checkout id — see module docs. This is the value
    /// `verify()` looks the payment up by, NOT `Receipt::reference`.
    pub checkout_id: String,
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
