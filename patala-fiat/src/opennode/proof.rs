//! What goes in an OpenNode [`patala_core::Receipt::proof`].
//!
//! Same structural gap as `btcpay::proof`/`stripe::proof` (see
//! `PORTING.md` §3, §5): cackle's `Begin` returns `Charge{Reference:
//! envelope.Data.ID}` — OpenNode's own charge id — but
//! `patala_core::Receipt::reference` is always the CALLER's own
//! `PayRequest::reference`, so the charge id lives in `proof` instead.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::opennode::OpenNodeRail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The OpenNode-assigned charge id — the stable key `verify()`
    /// re-fetches by.
    pub charge_id: String,
    /// OpenNode's hosted checkout page, carried here for caller UI
    /// convenience (`patala_core::Receipt` has no redirect-URL field).
    pub hosted_checkout_url: String,
}

impl ChargeProof {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ChargeProof always serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}
