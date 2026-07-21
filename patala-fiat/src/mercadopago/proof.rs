//! What goes in a Mercado Pago [`patala_core::Receipt::proof`].
//!
//! **The simplest case in this crate, like `paystack::proof`**: Mercado
//! Pago's `external_reference` (which this rail always sets to
//! `PayRequest::reference`) IS the value `verify()` searches by
//! (`GET /v1/payments/search?external_reference=...`) -- there is no
//! separate provider-assigned settlement id the way a Stripe Checkout
//! Session id is. So `verify()`/`refund()` operate on `Receipt::reference`
//! directly; this `proof` only carries the preference id and hosted
//! `init_point` for caller UI convenience, never as a verification key.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::mercadopago::rail::MercadoPagoRail::charge`]. Not load-bearing
/// for `verify()` -- see module docs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    pub preference_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_point: Option<String>,
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
