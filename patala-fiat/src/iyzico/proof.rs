//! What goes in an iyzico [`patala_core::Receipt::proof`].
//!
//! **Where this crate's Receipt-based seam sidesteps a real cackle quirk**
//! (see `PORTING.md`'s gap list and `rail.rs`'s module docs): cackle's own
//! `Begin` returns `Charge{Reference: parsed.Token, ...}` — NOT
//! `o.Reference` — meaning the string cackle's whole downstream system
//! (Verify/Webhook/storage) tracks an iyzico payment by is the Checkout
//! Form TOKEN, not the caller's own order reference. `patala_core::Receipt`
//! is documented as echoing back the CALLER's own `PayRequest::reference`,
//! so this port does the same thing `stripe::proof::ChargeProof` already
//! does for an analogous ambiguity: `Receipt::reference` always stays the
//! caller's own reference, and iyzico's real tracking token lives here in
//! `proof`, with `verify()` always looking it up from there.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::iyzico::IyzicoRail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The iyzico Checkout Form token — see module docs. This is the value
    /// `verify()`/the webhook path look the payment up by, NOT
    /// `Receipt::reference`.
    pub token: String,
    /// The hosted `paymentPageUrl`, kept for caller UI convenience —
    /// `patala_core::Receipt` has no redirect-URL field of its own (same
    /// reasoning as every other redirect-flow rail in this crate).
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
