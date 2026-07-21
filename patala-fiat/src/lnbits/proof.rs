//! What goes in an LNbits [`patala_core::Receipt::proof`].
//!
//! **Where this crate's `Receipt`-based seam structurally resolves a
//! limitation cackle's own file doc comment names explicitly, without
//! porting any of LNbits' actual payment logic differently** (see
//! `rail.rs`'s module docs for the full explanation): cackle's
//! `LNbitsProvider` cannot ask LNbits for the ORIGINAL fiat amount/currency
//! an invoice was priced in (LNbits' own status endpoint only reports a sat
//! amount), so cackle remembers that association itself — an in-memory map
//! keyed by `payment_hash`, populated in `Begin`, read in `Verify`/`Webhook`,
//! which cackle's own doc comment admits "does NOT survive a process
//! restart... a restart between Begin and Verify/Webhook will make this
//! adapter report 'unknown reference'" unless a `RecordStore` is wired in
//! via `NewLNbitsWithStore`. `patala_core::PaymentRail::verify` takes the
//! WHOLE [`patala_core::Receipt`] (not a bare reference string the way
//! cackle's `Verify(reference string)` does) — so this port embeds the
//! fiat amount/currency/creation-time directly in `proof`, exactly the same
//! "provider's own record locator, re-checked" idiom every other adapter's
//! `proof.rs` in this crate uses, and the caller's own storage of the
//! returned `Receipt` (which every caller must already do to reconcile
//! later) durably carries the association with NO separate `RecordStore`
//! seam needed. This is a consequence of the different trait shape, not a
//! change to LNbits' invoice-creation/status-polling logic, which is
//! byte-for-byte the same as cackle's.

use serde::{Deserialize, Serialize};

/// Serialized (as JSON bytes) into [`patala_core::Receipt::proof`] by
/// [`crate::lnbits::LNbitsRail::charge`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChargeProof {
    /// The BOLT11 invoice's payment hash — the stable key `verify()`
    /// re-fetches by.
    pub payment_hash: String,
    /// The BOLT11 payment request string, carried here purely for caller UI
    /// convenience (`patala_core::Receipt` has no invoice/redirect field of
    /// its own), same reasoning as every other `proof.rs` in this crate.
    pub payment_request: String,
    /// The ORIGINAL fiat amount this invoice was priced in, in minor units —
    /// see module docs. LNbits' own status endpoint cannot report this back.
    pub amount_minor: u64,
    /// The ORIGINAL fiat currency this invoice was priced in.
    pub currency: String,
    /// Unix seconds when this invoice was created — `verify()` enforces
    /// [`crate::lnbits::config::LNbitsConfig::quote_ttl_secs`] against this,
    /// mirroring cackle's `record.createdAt`/`p.quoteTTL` check.
    pub created_at_unix: u64,
}

impl ChargeProof {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ChargeProof always serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}
