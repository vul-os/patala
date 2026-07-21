//! Wire shapes for the Xendit adapter — ported from cackle's
//! `internal/payments/xendit.go`.
//!
//! Reference: <https://developers.xendit.co/api-reference/#create-invoice>
//! (Invoices API — create + list-by-external_id). Not re-verified live from
//! this environment — see this crate's `PORTING.md` "UNVERIFIED AGAINST
//! LIVE" note. Cackle's own confidence note: MEDIUM — the Invoices API
//! shape is well documented and implemented from that documentation, but
//! never run against a real Xendit sandbox account.
//!
//! **Currency-exponent note** (mirrors cackle's own file-header warning
//! verbatim): ISO-4217 — and this crate's own `crate::currency` table —
//! treat IDR as a 2-decimal currency (sen, unused in practice) and VND as
//! genuinely 0-decimal. Xendit's own wire format, however, carries `amount`
//! as the plain MAJOR-unit face value for every currency it supports (e.g.
//! `"amount": 10000` means Rp10.000, not Rp100 — Xendit does not use IDR
//! sen at all). This module bridges that gap via
//! `crate::currency::minor_to_major_string`/`major_string_to_minor`, which
//! convert `amount_minor` to/from major units using each currency's REAL
//! exponent (2 for IDR, 0 for VND, etc) — it does NOT special-case IDR as
//! zero-decimal anywhere. The IDR/VND conversion (Xendit's primary,
//! best-documented markets) is implemented with confidence; the other
//! supported 2-decimal currencies (PHP, THB, MYR) are NOT independently
//! verified against a real invoice.

use patala_core::Error;
use serde::Deserialize;

/// Mirrors cackle's `xenditInvoice`. `amount`/`paid_amount` are modelled as
/// [`serde_json::Value`] (not `u64`/`String`) because Xendit's wire format
/// is ambiguously a JSON number OR string depending on endpoint — mirrors
/// cackle's own `Amount any`/`PaidAmount any` fields exactly; assuming a
/// fixed JSON type here would silently break on whichever shape wasn't
/// assumed. `paid_at` is kept for shape-completeness (mirrors cackle's own
/// field) even though `patala_core::PaymentRail::verify` returns only
/// `bool` and has nowhere to surface a precise settlement timestamp -- see
/// `PORTING.md`'s gap list on `Result::PaidAt` having no home in `bool`.
#[derive(Clone, Debug, Deserialize, Default)]
pub struct XenditInvoice {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub external_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub amount: serde_json::Value,
    #[serde(default)]
    pub paid_amount: serde_json::Value,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub invoice_url: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub paid_at: String,
}

/// Mirrors cackle's `xenditNumberToString`: converts Xendit's amount field,
/// which may be encoded as either a JSON number or a JSON string depending
/// on endpoint, into a decimal string suitable for
/// `crate::currency::major_string_to_minor`. Returns `None` (mirrors
/// cackle's `""` sentinel, `default: return ""`) for anything else (null,
/// bool, array, object).
pub fn xendit_number_to_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => n.as_f64().map(xendit_trim_trailing_zeros),
        _ => None,
    }
}

/// Mirrors cackle's `xenditTrimTrailingZeros`: Xendit amounts are whole
/// numbers for its zero-decimal markets; formatting via a bare `{}` on an
/// `f64` risks scientific notation for very large values, so this uses a
/// fixed-point format instead, then trims the trailing zeros/dot.
fn xendit_trim_trailing_zeros(f: f64) -> String {
    let s = format!("{f:.6}");
    let s = s.trim_end_matches('0');
    s.trim_end_matches('.').to_string()
}

/// Mirrors cackle's `classifyXenditError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    #[derive(Deserialize, Default)]
    struct ErrorEnvelope {
        #[serde(default)]
        message: String,
        #[serde(default)]
        error_code: String,
    }
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if !env.message.is_empty() {
        env.message
    } else if !env.error_code.is_empty() {
        env.error_code
    } else {
        "no message".to_string()
    };
    Error::Rail(format!(
        "xendit: unexpected API response status: http {status}: {msg}"
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("xendit: malformed API response: {detail}"))
}

/// The settlement outcome of an [`XenditInvoice`] — shared by both
/// [`crate::xendit::rail::XenditRail::verify`] (a poll) and
/// [`crate::xendit::webhook::verify_and_parse`] (a push), mirroring
/// cackle's shared `xenditInvoiceToResult` helper exactly (both cackle
/// paths call the same function; this port factors the same logic once
/// here rather than duplicating the paid_amount-preference rule).
pub struct InvoiceOutcome {
    pub reference: String,
    pub event_id: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `xenditInvoiceToResult`: prefer `paid_amount` (what was
/// actually settled) over `amount` (the invoice face value) ONLY when
/// `status` is `"PAID"` and `paid_amount` is present (not JSON `null`);
/// otherwise fall back to `amount` for pending/expired invoices. Only
/// `"PAID"`/`"SETTLED"` (with a positive parsed amount and a non-empty
/// invoice id, else malformed) are ever settled -- `"EXPIRED"` and anything
/// unrecognised (including `"PENDING"`) fail closed as not-settled, exactly
/// as cackle's own `switch` does.
pub fn invoice_to_outcome(inv: &XenditInvoice) -> Result<InvoiceOutcome, Error> {
    if inv.external_id.is_empty() {
        return Err(malformed("missing external_id"));
    }
    let amount_field = if inv.status == "PAID" && !inv.paid_amount.is_null() {
        &inv.paid_amount
    } else {
        &inv.amount
    };
    let amount_str = xendit_number_to_string(amount_field)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| malformed("unparseable amount field"))?;
    let amount_minor = crate::currency::major_string_to_minor(&amount_str, &inv.currency)
        .map_err(|e| malformed(&format!("amount {amount_str:?}: {e}")))?;

    match inv.status.as_str() {
        "PAID" | "SETTLED" => {
            if amount_minor == 0 {
                return Err(malformed("paid status with non-positive amount"));
            }
            if inv.id.is_empty() {
                return Err(malformed(
                    "paid invoice with no id (cannot dedupe webhooks)",
                ));
            }
            Ok(InvoiceOutcome {
                reference: inv.external_id.clone(),
                event_id: inv.id.clone(),
                settled: true,
                amount_minor,
                currency: inv.currency.clone(),
            })
        }
        // "EXPIRED", "PENDING", and anything unrecognised fail closed as
        // not-paid -- mirrors cackle's `default` case.
        _ => Ok(InvoiceOutcome {
            reference: inv.external_id.clone(),
            event_id: inv.id.clone(),
            settled: false,
            amount_minor: 0,
            currency: inv.currency.clone(),
        }),
    }
}

/// A value is safe to interpolate directly into a URL query string or path
/// segment without a percent-encoding crate (the `xendit` feature
/// deliberately has no `url` dependency, unlike `payu`) only if it carries
/// none of the characters that would need real encoding. Mirrors the
/// spirit of `stripe::rail`/`paystack::rail`'s `safe_path_segment` helper:
/// guard-then-reject rather than encode.
pub fn safe_query_value(s: &str) -> Result<&str, Error> {
    if s.is_empty() || s.contains(['/', '?', '#', '&', '=', ' ', '\t', '\n', '\r']) || !s.is_ascii()
    {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe query value for a xendit external_id"
        )));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_to_string_passthrough_for_strings() {
        assert_eq!(
            xendit_number_to_string(&serde_json::json!("100.50")),
            Some("100.50".to_string())
        );
    }

    #[test]
    fn number_to_string_formats_whole_numbers() {
        assert_eq!(
            xendit_number_to_string(&serde_json::json!(10000)),
            Some("10000".to_string())
        );
    }

    #[test]
    fn number_to_string_trims_trailing_zeros_and_dot() {
        assert_eq!(xendit_trim_trailing_zeros(10000.0), "10000");
        assert_eq!(xendit_trim_trailing_zeros(100.5), "100.5");
        assert_eq!(xendit_trim_trailing_zeros(0.0), "0");
    }

    #[test]
    fn number_to_string_none_for_other_json_types() {
        assert_eq!(xendit_number_to_string(&serde_json::Value::Null), None);
        assert_eq!(xendit_number_to_string(&serde_json::json!(true)), None);
        assert_eq!(xendit_number_to_string(&serde_json::json!([1, 2])), None);
    }

    #[test]
    fn invoice_to_outcome_prefers_paid_amount_when_paid() {
        let inv = XenditInvoice {
            id: "inv_1".into(),
            external_id: "ord_1".into(),
            status: "PAID".into(),
            amount: serde_json::json!(10000),
            paid_amount: serde_json::json!(9500),
            currency: "IDR".into(),
            invoice_url: String::new(),
            paid_at: String::new(),
        };
        let outcome = invoice_to_outcome(&inv).unwrap();
        assert!(outcome.settled);
        assert_eq!(outcome.amount_minor, 950_000);
    }

    #[test]
    fn invoice_to_outcome_pending_uses_face_amount_and_is_not_settled() {
        let inv = XenditInvoice {
            id: "inv_1".into(),
            external_id: "ord_1".into(),
            status: "PENDING".into(),
            amount: serde_json::json!(10000),
            paid_amount: serde_json::Value::Null,
            currency: "IDR".into(),
            invoice_url: String::new(),
            paid_at: String::new(),
        };
        let outcome = invoice_to_outcome(&inv).unwrap();
        assert!(!outcome.settled);
        assert_eq!(outcome.amount_minor, 0);
    }
}
