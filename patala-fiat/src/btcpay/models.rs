//! Wire shapes for the BTCPay adapter — ported from cackle's
//! `internal/payments/btcpay.go`.
//!
//! Reference: BTCPay Server's Greenfield API v1
//! (<https://docs.btcpayserver.org/API/Greenfield/v1/>, invoices:
//! <https://docs.btcpayserver.org/API/Greenfield/v1/#tag/Invoices>). Not
//! re-verified live from this environment — see this crate's `PORTING.md`
//! "UNVERIFIED AGAINST LIVE" note. Cackle's own file doc comment rates its
//! confidence HIGH for the invoice create/fetch shape and the webhook
//! signing scheme, and only MODERATE for the exact `additionalStatus` enum
//! values used below — ported verbatim regardless, with that same caveat
//! repeated here.
#![allow(dead_code)]

use patala_core::Error;
use serde::Deserialize;

/// Mirrors cackle's `btcpayInvoice` — the shape both the create-invoice and
/// get-invoice responses share.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct BTCPayInvoice {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "storeId", default)]
    pub store_id: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub currency: String,
    /// New | Processing | Settled | Expired | Invalid
    #[serde(default)]
    pub status: String,
    /// None | Marked | Invalid | PaidPartial | PaidOver | PaidLate
    #[serde(rename = "additionalStatus", default)]
    pub additional_status: String,
    #[serde(rename = "checkoutLink", default)]
    pub checkout_link: String,
    #[serde(rename = "expirationTime", default)]
    pub expiration_time: i64,
}

/// Mirrors cackle's `classifyBTCPayError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    #[derive(Deserialize, Default)]
    struct ErrorEnvelope {
        #[serde(default)]
        message: String,
    }
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.message.is_empty() {
        "no message".to_string()
    } else {
        env.message
    };
    Error::Rail(format!(
        "btcpay: unexpected API response status: http {status}: {msg}"
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("btcpay: malformed API response: {detail}"))
}

/// The settlement state a BTCPay invoice's `status`/`additionalStatus`
/// combination maps to. Mirrors the `switch` in cackle's
/// `btcpayResultFromInvoice` exactly, fail-closed default included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvoiceState {
    /// `Settled` with `""`/`None`/`Marked`/`PaidLate` — the only states ever
    /// reported as paid.
    Paid,
    /// `New`/`Processing` — not yet paid, may still complete.
    Pending,
    /// `Expired`/`Invalid` (without `PaidPartial`), or any unrecognised
    /// status — never settles, fails closed.
    Failed,
    /// `Settled` + `PaidOver` — cackle's `ErrBTCPayOverpaid`: flagged for a
    /// human, never silently accepted or silently rejected.
    Overpaid,
    /// Any other `Settled` + `additionalStatus` combination not covered
    /// above (e.g. `Settled`+`PaidPartial`) — cackle's
    /// `ErrBTCPayInconsistentStatus`: an ambiguous combination this adapter
    /// refuses to guess about.
    Inconsistent,
}

/// Mirrors cackle's `btcpayResultFromInvoice`'s status-mapping `switch`.
pub fn classify_invoice_state(status: &str, additional_status: &str) -> InvoiceState {
    match status {
        "Settled" => match additional_status {
            "" | "None" | "Marked" | "PaidLate" => InvoiceState::Paid,
            "PaidOver" => InvoiceState::Overpaid,
            _ => InvoiceState::Inconsistent,
        },
        "New" | "Processing" => InvoiceState::Pending,
        "Expired" | "Invalid" => InvoiceState::Failed,
        // Fail closed: an unrecognised status is never treated as paid.
        _ => InvoiceState::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_status_mapping() {
        assert_eq!(
            classify_invoice_state("Settled", "None"),
            InvoiceState::Paid
        );
        assert_eq!(classify_invoice_state("Settled", ""), InvoiceState::Paid);
        assert_eq!(
            classify_invoice_state("Settled", "Marked"),
            InvoiceState::Paid
        );
        assert_eq!(
            classify_invoice_state("Settled", "PaidLate"),
            InvoiceState::Paid
        );
        assert_eq!(
            classify_invoice_state("Settled", "PaidOver"),
            InvoiceState::Overpaid
        );
        assert_eq!(
            classify_invoice_state("Settled", "PaidPartial"),
            InvoiceState::Inconsistent
        );
        assert_eq!(classify_invoice_state("New", "None"), InvoiceState::Pending);
        assert_eq!(
            classify_invoice_state("Processing", "None"),
            InvoiceState::Pending
        );
        assert_eq!(
            classify_invoice_state("Expired", "None"),
            InvoiceState::Failed
        );
        assert_eq!(
            classify_invoice_state("Invalid", "PaidPartial"),
            InvoiceState::Failed
        );
        assert_eq!(
            classify_invoice_state("some-new-status", "None"),
            InvoiceState::Failed
        );
    }
}
