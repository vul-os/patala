//! What goes in a Midtrans [`patala_core::Receipt::proof`].
//!
//! Like `paystack::proof`/`flutterwave::proof` (and unlike Stripe/iyzico):
//! cackle's `Begin` returns `Charge{Reference: o.Reference, ...}` — the
//! caller's own reference IS what Midtrans's Core API status endpoint is
//! keyed on (`GET /{order_id}/status`) — so `Receipt::reference` stays the
//! caller's own reference and this `proof` only carries the hosted
//! `redirect_url` for caller UI convenience.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::midtrans::MidtransRail::charge`]. Not load-bearing for
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
