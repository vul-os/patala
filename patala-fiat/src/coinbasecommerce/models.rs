//! Wire shapes for the Coinbase Commerce adapter — ported from cackle's
//! `internal/payments/coinbasecommerce.go`.
//!
//! Reference: <https://docs.cloud.coinbase.com/commerce/reference>
//! (create/fetch a charge),
//! <https://docs.cloud.coinbase.com/commerce/docs/webhooks-security>. Not
//! re-verified live from this environment — see this crate's `PORTING.md`
//! "UNVERIFIED AGAINST LIVE" note. Cackle's own file doc comment rates
//! confidence HIGH for auth/create/fetch/webhook-signing, MODERATE for the
//! exact timeline status enum beyond NEW/PENDING/COMPLETED/EXPIRED.
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;

/// One entry in a charge's status timeline. Mirrors cackle's
/// `coinbaseCommerceTimelineEntry`. `context` is only populated for
/// `UNRESOLVED` entries and is only moderately confident (cackle's own doc
/// comment) — surfaced in error messages for a human to read, never parsed
/// into an automated decision.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TimelineEntry {
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub context: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct LocalPrice {
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub currency: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Pricing {
    #[serde(default)]
    pub local: LocalPrice,
}

/// Mirrors cackle's `coinbaseCommerceCharge`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Charge {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub hosted_url: String,
    #[serde(default)]
    pub timeline: Vec<TimelineEntry>,
    #[serde(default)]
    pub pricing: Pricing,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Envelope {
    #[serde(default)]
    pub data: Charge,
}

/// Mirrors cackle's `classifyCoinbaseCommerceError` (nested
/// `{"error":{"message":...}}` shape).
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    #[derive(Deserialize, Default)]
    struct ErrorBody {
        #[serde(default)]
        message: String,
    }
    #[derive(Deserialize, Default)]
    struct ErrorEnvelope {
        #[serde(default)]
        error: ErrorBody,
    }
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.error.message.is_empty() {
        "no message".to_string()
    } else {
        env.error.message
    };
    Error::Rail(format!(
        "coinbasecommerce: unexpected API response status: http {status}: {msg}"
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!(
        "coinbasecommerce: malformed API response: {detail}"
    ))
}

/// The settlement state a charge's LATEST timeline entry maps to. Mirrors
/// cackle's `coinbaseCommerceResultFromCharge` switch exactly, including
/// its choice to ERROR (not just report unpaid) on `UNRESOLVED`/`RESOLVED`
/// and on a truly unrecognised status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChargeState {
    /// `COMPLETED` — the only status ever reported as paid.
    Paid,
    /// `NEW`/`PENDING` — not yet paid, may still complete.
    Pending,
    /// `EXPIRED`/`CANCELED` — never settles.
    Failed,
    /// `UNRESOLVED`/`RESOLVED` — cackle's
    /// `ErrCoinbaseCommerceRequiresManualReview`: this is where
    /// under/overpayment surfaces. Carries the timeline entry's `context`
    /// when non-empty (e.g. `"OVERPAID"`).
    RequiresManualReview(Option<String>),
    /// Any other, truly unrecognised status — cackle errors here too
    /// (rather than just reporting unpaid), since an enum this crate/cackle
    /// doesn't know about might mean anything.
    Unrecognised(String),
}

/// Mirrors cackle's `coinbaseCommerceResultFromCharge`'s status-mapping
/// `switch` on the LATEST timeline entry.
pub fn classify_charge_state(latest: &TimelineEntry) -> ChargeState {
    match latest.status.as_str() {
        "COMPLETED" => ChargeState::Paid,
        "NEW" | "PENDING" => ChargeState::Pending,
        "EXPIRED" | "CANCELED" => ChargeState::Failed,
        "UNRESOLVED" | "RESOLVED" => {
            if latest.context.is_empty() {
                ChargeState::RequiresManualReview(None)
            } else {
                ChargeState::RequiresManualReview(Some(latest.context.clone()))
            }
        }
        other => ChargeState::Unrecognised(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(status: &str, context: &str) -> TimelineEntry {
        TimelineEntry {
            time: String::new(),
            status: status.to_string(),
            context: context.to_string(),
        }
    }

    #[test]
    fn status_mapping() {
        assert_eq!(
            classify_charge_state(&entry("COMPLETED", "")),
            ChargeState::Paid
        );
        assert_eq!(
            classify_charge_state(&entry("NEW", "")),
            ChargeState::Pending
        );
        assert_eq!(
            classify_charge_state(&entry("PENDING", "")),
            ChargeState::Pending
        );
        assert_eq!(
            classify_charge_state(&entry("EXPIRED", "")),
            ChargeState::Failed
        );
        assert_eq!(
            classify_charge_state(&entry("CANCELED", "")),
            ChargeState::Failed
        );
        assert_eq!(
            classify_charge_state(&entry("UNRESOLVED", "OVERPAID")),
            ChargeState::RequiresManualReview(Some("OVERPAID".to_string()))
        );
        assert_eq!(
            classify_charge_state(&entry("RESOLVED", "")),
            ChargeState::RequiresManualReview(None)
        );
        assert_eq!(
            classify_charge_state(&entry("SOME_FUTURE_STATUS", "")),
            ChargeState::Unrecognised("SOME_FUTURE_STATUS".to_string())
        );
    }
}
