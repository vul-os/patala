//! What goes in a PayFast [`patala_core::Receipt::proof`].
//!
//! **`patala_core::Receipt` has no redirect-URL field, AND no form-data
//! field** — a bigger gap than every other redirect-flow rail in this
//! crate hits, so it's flagged loudly here too (mirroring cackle's own loud
//! comment on `Charge.RedirectURL`): PayFast's canonical integration is an
//! HTML form auto-POSTed to `process_url` (all the signed fields, plus
//! `signature`), NOT a bare clickable GET link. A caller that only
//! redirects the browser to `process_url` with no form data will NOT work.
//! [`ChargeProof::signed_fields_query`] carries the full signed field set
//! as a URL-encoded query string for the caller to render as a hidden
//! auto-submitting form — this has not been verified against a live
//! PayFast checkout (`mod.rs`'s "UNVERIFIED AGAINST LIVE" note).

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::payfast::PayFastRail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// PayFast's process endpoint — see module docs: a bare redirect here
    /// alone is NOT enough.
    pub process_url: String,
    /// The full signed field set (`merchant_id=...&...&signature=...`),
    /// URL-encoded, for the caller to render as a hidden auto-submitting
    /// HTML form. See module docs.
    pub signed_fields_query: String,
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
