//! What goes in a Razorpay [`patala_core::Receipt::proof`].
//!
//! **Sidesteps the same "provider's own id vs caller's reference" ambiguity
//! stripe/proof.rs already resolves** (see `PORTING.md`'s gap list and
//! `rail.rs`'s module docs): cackle's own `Begin` sets `Charge.Reference` to
//! Razorpay's OWN generated order id (`parsed.ID`), discarding the caller's
//! original `Order.Reference` entirely -- internally consistent within
//! cackle (its later `Verify(reference)` expects that same Razorpay order
//! id back), but a different "what does Reference mean" convention than
//! PayU/Paystack (where it's the caller's own reference). `patala_core`'s
//! own `Receipt::reference` doc comment is explicit: it is "The
//! `PayRequest::reference` this receipt fulfills" -- so this port keeps
//! `Receipt::reference` ALWAYS equal to the caller's own
//! `PayRequest::reference`, exactly like `stripe::StripeRail::charge`
//! chooses to do for the identical reason, and embeds Razorpay's real order
//! id here instead, for `verify()` to look up by.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::razorpay::RazorpayRail::charge`]. Never treated as a
/// cryptographic signature -- Razorpay's own order record is the source of
/// truth `verify()` re-fetches, exactly the pattern
/// `stripe::proof::ChargeProof` documents for a custodial rail.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The Razorpay order id (`order_...`) -- see module docs.
    pub order_id: String,
}

impl ChargeProof {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ChargeProof always serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}
