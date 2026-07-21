//! What goes in an Adyen [`patala_core::Receipt::proof`].
//!
//! **A genuine structural gap, not just plumbing** (see `rail.rs`'s module
//! docs and `PORTING.md`'s gap list): Adyen's Pay by Link create response
//! (`{id, url}`) never carries a `pspReference` — that identifier only shows
//! up later, on the AUTHORISATION webhook notification, once a buyer has
//! actually paid. So at `charge()` time this proof can only carry the
//! payment LINK's own id (`payment_link_id`), never a `psp_reference` — the
//! field starts `None` and stays that way unless a caller, having received
//! and validated an AUTHORISATION webhook via
//! [`crate::adyen::webhook::verify_and_parse`], constructs a fresh
//! [`ChargeProof`] with `psp_reference` populated from that event and
//! updates its own stored `Receipt` before ever calling
//! [`crate::adyen::rail::AdyenRail::refund`] — see that method's own doc
//! comment for why it requires this.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::adyen::rail::AdyenRail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// Adyen's payment link id (from the `/paymentLinks` create response).
    pub payment_link_id: String,
    /// Adyen's own settlement-record id (`pspReference`), if and only if it
    /// has been threaded back in from a verified AUTHORISATION webhook — see
    /// module docs. `None` on every `Receipt` `charge()` itself produces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psp_reference: Option<String>,
    /// The Pay by Link hosted redirect URL, kept for caller UI convenience —
    /// `patala_core::Receipt` has no redirect-URL field of its own, same
    /// reason `stripe::proof::ChargeProof` and `paystack::proof::ChargeProof`
    /// each carry one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// Serialized into the `proof` of the *new* `Receipt` returned by
/// [`crate::adyen::rail::AdyenRail::refund`].
///
/// **Not ported from cackle** (cackle's `Provider` interface has no `Refund`
/// method at all — see `rail.rs`'s module docs on `refund`): new code
/// grounded in Adyen's own public Refunds API
/// (<https://docs.adyen.com/online-payments/refund/>).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefundProof {
    /// The refund modification's OWN `pspReference` (distinct from the
    /// original payment's `pspReference`) — Adyen returns this synchronously
    /// but the refund's actual completion is only confirmed asynchronously,
    /// by a later `REFUND` webhook notification this port does not build
    /// (cackle's own Webhook only ever handles `AUTHORISATION` — see
    /// `rail.rs`'s module docs).
    pub refund_psp_reference: String,
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
