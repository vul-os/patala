//! What goes in a BTCPay [`patala_core::Receipt::proof`].
//!
//! **Structural gap vs cackle, same shape as `stripe::proof`'s** (see
//! `PORTING.md` §3 and §5, and `rail.rs`'s module docs): cackle's `Begin`
//! returns `Charge{Reference: inv.ID}` — the BTCPay-assigned invoice id
//! becomes cackle's own `Charge.Reference`, echoed back to `Verify`.
//! `patala_core::Receipt::reference` is documented as always "the
//! `PayRequest::reference` this receipt fulfills" (the CALLER's own
//! correlation key) — so the BTCPay invoice id has nowhere to live except
//! `proof`, exactly the same seam adaptation `stripe::proof::ChargeProof`
//! documents for a Stripe Checkout Session id.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::btcpay::BTCPayRail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The BTCPay-assigned invoice id — the stable key `verify()`/`refund()`
    /// re-fetch by. See module docs.
    pub invoice_id: String,
    /// BTCPay's hosted checkout page. `patala_core::Receipt` has no
    /// redirect-URL field of its own — carried here purely for caller UI
    /// convenience, same as `stripe::proof::ChargeProof::redirect_url`.
    pub checkout_link: String,
}

impl ChargeProof {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ChargeProof always serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}
