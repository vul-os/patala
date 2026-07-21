//! Wire shapes and decimal-string amount handling for the Mollie adapter --
//! ported from cackle's `internal/payments/mollie.go`.
//!
//! Doc sources (cited verbatim from mollie.go, verified live against
//! docs.mollie.com by cackle's own author; not re-verified from this
//! environment -- see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE"
//! note):
//! - Create payment: <https://docs.mollie.com/reference/create-payment>
//! - Get payment: <https://docs.mollie.com/reference/get-payment>
//! - Create refund: <https://docs.mollie.com/reference/create-refund>
//! - Webhooks: <https://docs.mollie.com/reference/webhooks>
//!
//! ## HONESTY note, ported from cackle's own file doc comment
//!
//! Mollie's `amount.value` field is a decimal string (e.g. `"10.00"`), and
//! cackle's own author was fully confident about the ordinary 2-decimal
//! case but could NOT confirm, word-for-word against Mollie's own reference
//! page, whether a zero-decimal currency like JPY must be sent as `"1000"`
//! or `"1000.00"`. Rather than guess and risk a 100x amount error, cackle's
//! adapter REFUSES every non-2-decimal ISO-4217 currency outright.
//!
//! **A disclosed, deliberate WIDENING of cackle's own refusal list, not a
//! narrowing** (flagged per `PORTING.md`'s own instruction to surface
//! divergences): cackle's `mollieNonTwoDecimalCurrencies` hardcodes only
//! ten currencies (`JPY, KRW, VND, CLP, ISK, KWD, BHD, JOD, OMR, TND`) --
//! this is NOT the full ISO-4217 non-2-decimal set (`crate::currency`'s own
//! zero-decimal table alone has sixteen entries, three-decimal has seven);
//! cackle's own list is missing, among others, BIF/DJF/GNF/KMF/PYG/RWF/
//! UGX/VUV/XAF/XOF/XPF (zero-decimal) and IQD/LYD (three-decimal) -- so
//! cackle's own Mollie adapter would silently treat e.g. a BIF order as an
//! ordinary 2-decimal amount, the exact 100x-error risk this file's own
//! HONESTY note warns against. Per `PORTING.md` §8's binding rule ("always
//! route currency exponent lookups through `crate::currency`, never a
//! hardcoded list"), this port instead refuses on
//! `crate::currency::exponent(code) != 2` -- a strict SUPERSET of cackle's
//! refusal list (refuses everything cackle refuses, plus the currencies
//! cackle's own list omits). This can only make the adapter MORE
//! conservative than cackle (never accepts a conversion cackle would have
//! silently gotten wrong), so it is the safer direction for money-critical
//! code, consistent with this crate's fail-closed philosophy -- and cackle's
//! OWN two test cases (JPY, KWD refused) still pass unchanged.
#![allow(dead_code)]

use patala_core::Error;

/// Mirrors cackle's `ErrMollieUnsupportedCurrency`.
pub fn unsupported_currency(cur: &str) -> Error {
    Error::InvalidRequest(format!(
        "mollie: {cur} is not a 2-decimal ISO-4217 currency; its amount.value decimal-string format is not verified against Mollie's documented semantics, refusing rather than guessing (see models.rs HONESTY note)"
    ))
}

pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("mollie: malformed API response: {detail}"))
}

/// Mirrors cackle's `mollieAmountValue`, but see the module doc comment's
/// HONESTY note: this routes the non-2-decimal refusal through
/// `crate::currency::exponent` (a superset of cackle's own hardcoded list)
/// and the actual formatting through `crate::currency::minor_to_major_string`
/// (never a hand-rolled `%d.%02d`), per `PORTING.md` §8's binding rule.
pub fn mollie_amount_value(amount_minor: u64, currency: &str) -> Result<String, Error> {
    let cur = currency.trim().to_ascii_uppercase();
    let exp = crate::currency::exponent(&cur)
        .map_err(|e| Error::InvalidRequest(format!("mollie: {e}")))?;
    if exp != 2 {
        return Err(unsupported_currency(&cur));
    }
    crate::currency::minor_to_major_string(amount_minor, &cur)
        .map_err(|e| Error::InvalidRequest(format!("mollie: {e}")))
}

/// Mirrors cackle's `mollieAmountValueToMinor`: the inverse of
/// [`mollie_amount_value`], used to convert a SETTLED amount Mollie reports
/// back into `amount_minor`. See module doc comment's HONESTY note on why
/// this refuses via `crate::currency::exponent` rather than cackle's own
/// hardcoded list.
pub fn mollie_amount_value_to_minor(value: &str, currency: &str) -> Result<u64, Error> {
    let cur = currency.trim().to_ascii_uppercase();
    let exp = crate::currency::exponent(&cur).map_err(|e| malformed(&e.to_string()))?;
    if exp != 2 {
        return Err(unsupported_currency(&cur));
    }
    crate::currency::major_string_to_minor(value, &cur).map_err(|e| malformed(&e.to_string()))
}

/// `POST /payments` and `GET /payments/{id}` response's `amount` object --
/// mirrors cackle's inline anonymous `amount` struct.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct AmountObj {
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub value: String,
}

/// The subset of a Mollie Payment object this adapter reads -- mirrors
/// cackle's `molliePaymentPayload`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct PaymentPayload {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub amount: AmountObj,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// Mirrors cackle's `mollieMetadataReference`: extracts the
/// `patala_reference` field this adapter's `charge()` stores in the
/// payment's metadata (this crate's own convention -- see `PORTING.md` §3 --
/// in place of cackle's own `cackle_reference` key), tolerating metadata
/// being absent or not an object.
pub fn metadata_reference(raw: &Option<serde_json::Value>) -> Option<String> {
    raw.as_ref()?
        .as_object()?
        .get("patala_reference")?
        .as_str()
        .map(|s| s.to_string())
}

/// The result of evaluating a [`PaymentPayload`] -- mirrors the shape
/// cackle's `parseMolliePayment` builds into a `Result`.
pub struct PaymentOutcome {
    pub reference: String,
    /// `true` only for Mollie's `status == "paid"` -- `"open"`, `"pending"`,
    /// `"authorized"`, `"canceled"`, `"expired"`, `"failed"`, or anything
    /// unrecognised is `false`, mirroring cackle's fail-closed default.
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `parseMolliePayment`: turns a Payment payload into a
/// settlement outcome, failing closed on anything malformed or ambiguous.
pub fn evaluate_payment(p: &PaymentPayload) -> Result<PaymentOutcome, Error> {
    if p.id.is_empty() {
        return Err(malformed("missing payment id"));
    }
    let reference = metadata_reference(&p.metadata).ok_or_else(|| {
        malformed("payment has no patala_reference metadata to reconcile against")
    })?;
    let currency = p.amount.currency.trim().to_ascii_uppercase();

    if p.status == "paid" {
        let amount_minor = mollie_amount_value_to_minor(&p.amount.value, &currency)?;
        if amount_minor == 0 {
            return Err(malformed("status=paid with non-positive amount"));
        }
        Ok(PaymentOutcome {
            reference,
            settled: true,
            amount_minor,
            currency,
        })
    } else {
        Ok(PaymentOutcome {
            reference,
            settled: false,
            amount_minor: 0,
            currency,
        })
    }
}

/// Mirrors cackle's `mollieErrorEnvelope`: `{"status":..., "title":"...",
/// "detail":"..."}`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct ErrorEnvelope {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub detail: String,
}

/// Mirrors cackle's `classifyMollieError`.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    let env: ErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if !env.detail.is_empty() {
        env.detail
    } else if !env.title.is_empty() {
        env.title
    } else {
        "no message".to_string()
    };
    Error::Rail(format!(
        "mollie: unexpected API response status: http {status}: {msg}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/mollie_test.go.

    #[test]
    fn ordinary_currency_formats_as_decimal_string() {
        assert_eq!(mollie_amount_value(5000, "EUR").unwrap(), "50.00");
    }

    #[test]
    fn zero_decimal_currency_refused() {
        assert!(mollie_amount_value(1000, "JPY").is_err());
    }

    #[test]
    fn three_decimal_currency_refused() {
        assert!(mollie_amount_value(1000, "KWD").is_err());
    }

    #[test]
    fn amount_value_to_minor_round_trips() {
        assert_eq!(mollie_amount_value_to_minor("50.00", "EUR").unwrap(), 5000);
    }

    #[test]
    fn widened_refusal_covers_currencies_cackles_own_list_omits() {
        // See module doc comment's HONESTY note: BIF is zero-decimal (per
        // crate::currency) but is NOT in cackle's own hardcoded
        // mollieNonTwoDecimalCurrencies list -- this port refuses it anyway.
        assert!(mollie_amount_value(1000, "BIF").is_err());
        assert!(mollie_amount_value(1000, "IQD").is_err());
    }
}
