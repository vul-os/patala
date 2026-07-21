//! Wire shapes and the zero/three-decimal amount conversion for the Stripe
//! adapter — ported from cackle's `internal/payments/stripe.go`.
//!
//! Doc sources (verified live against docs.stripe.com by cackle's own
//! author; not re-verified from this environment — see this crate's
//! `PORTING.md` "UNVERIFIED AGAINST LIVE" note):
//! - Checkout Session create: <https://docs.stripe.com/api/checkout/sessions/create>
//! - Checkout Session retrieve: <https://docs.stripe.com/api/checkout/sessions/retrieve>
//! - Currencies (zero-decimal): <https://docs.stripe.com/currencies>
//!
//! [`STRIPE_ZERO_DECIMAL`] documents Stripe's zero-decimal list for
//! reference (and mirrors cackle's own `stripeZeroDecimalCurrencies`
//! verbatim) but, exactly as in cackle's `stripeAmount`, is never actually
//! consulted by the conversion logic below: an ordinary 2-decimal currency
//! and a true zero-decimal one both take the same pass-through path, so
//! only [`STRIPE_THREE_DECIMAL`] (refuse) and
//! [`STRIPE_FORCED_TWO_DECIMAL`] (×100) need checking. Cackle's own
//! `stripeZeroDecimalCurrencies` var is equally unreferenced by its
//! `stripeAmount` function — Go's compiler just doesn't flag an unused
//! package-level var the way Rust's `dead_code` lint flags an unused
//! `const`. Some response fields below (`StripeErrorBody::r#type`) are part
//! of Stripe's documented response shape and kept for the same
//! honest/complete-wire-model reason `patala-hyperswitch::models` gives.
#![allow(dead_code)]

use patala_core::Error;

/// Mirrors cackle's `stripeZeroDecimalCurrencies`: ISO-4217 codes Stripe's
/// API treats as zero-decimal (the `amount` field is the whole-unit count,
/// not multiplied by 100). This is STRIPE's documented list, not the
/// general ISO-4217 zero-exponent list in [`crate::currency`] — they
/// overlap almost entirely but not completely (MGA is Stripe-zero-decimal
/// but exponent 2 in the general ISO-4217 table; see
/// [`STRIPE_FORCED_TWO_DECIMAL`] for the documented exceptions the other
/// direction, ISK/UGX).
const STRIPE_ZERO_DECIMAL: &[&str] = &[
    "BIF", "CLP", "DJF", "GNF", "JPY", "KMF", "KRW", "MGA", "PYG", "RWF", "VND", "VUV", "XAF",
    "XOF", "XPF",
];

/// Mirrors cackle's `stripeForcedTwoDecimalCurrencies`. Verbatim from
/// Stripe's own docs (quoted in cackle's `stripe.go`): *"ISK transitioned to
/// a zero-decimal currency, but backward compatibility requires you to
/// represent it as a two-decimal value, where the decimal amount is always
/// 00. For example, to charge 5 ISK, provide an amount value of 500."* —
/// and the identical sentence for UGX. This is exactly the "get
/// zero-decimal handling right" trap this task's brief warns about: naively
/// trusting the ISO-4217 exponent table (which says ISK/UGX are 0-decimal)
/// would UNDERCHARGE by 100x on these two currencies specifically.
const STRIPE_FORCED_TWO_DECIMAL: &[&str] = &["ISK", "UGX"];

/// Mirrors cackle's `stripeThreeDecimalCurrencies`: the ISO-4217
/// three-decimal currencies. Stripe's currency docs do not mention
/// three-decimal handling at all, so this adapter refuses them rather than
/// guessing — see [`Error`]'s message on [`stripe_amount`].
const STRIPE_THREE_DECIMAL: &[&str] = &["KWD", "BHD", "JOD", "OMR", "TND"];

fn contains(list: &[&str], code: &str) -> bool {
    list.iter().any(|c| c.eq_ignore_ascii_case(code))
}

/// Mirrors cackle's `stripeAmount`: converts `amount_minor` (patala-core's
/// own ISO-4217 minor-unit representation) into the integer Stripe's
/// `amount` field expects.
///
/// - Ordinary 2-decimal currencies: passed straight through (both
///   conventions are "major unit * 100").
/// - Stripe's documented zero-decimal currencies (excluding ISK/UGX):
///   passed straight through.
/// - ISK/UGX: multiplied by 100 (the one real conversion this function
///   performs — see [`STRIPE_FORCED_TWO_DECIMAL`]).
/// - ISO-4217 three-decimal currencies (KWD, ...): refused.
pub fn stripe_amount(amount_minor: u64, currency: &str) -> Result<u64, Error> {
    let cur = currency.trim().to_ascii_uppercase();
    if contains(STRIPE_THREE_DECIMAL, &cur) {
        return Err(unsupported_currency(&cur));
    }
    if contains(STRIPE_FORCED_TWO_DECIMAL, &cur) {
        return amount_minor.checked_mul(100).ok_or_else(|| {
            Error::InvalidRequest(format!("stripe: amount {amount_minor} overflows for {cur}"))
        });
    }
    Ok(amount_minor)
}

/// Mirrors cackle's `stripeAmountToMinor`: the inverse of
/// [`stripe_amount`], used to convert a SETTLED amount Stripe reports back
/// into patala's `amount_minor`.
pub fn stripe_amount_to_minor(stripe_amt: u64, currency: &str) -> Result<u64, Error> {
    let cur = currency.trim().to_ascii_uppercase();
    if contains(STRIPE_THREE_DECIMAL, &cur) {
        return Err(unsupported_currency(&cur));
    }
    if contains(STRIPE_FORCED_TWO_DECIMAL, &cur) {
        if !stripe_amt.is_multiple_of(100) {
            return Err(malformed(&format!(
                "{cur} amount {stripe_amt} is not a whole multiple of 100 as Stripe documents"
            )));
        }
        return Ok(stripe_amt / 100);
    }
    Ok(stripe_amt)
}

/// Mirrors cackle's `ErrStripeUnsupportedCurrency`.
pub fn unsupported_currency(cur: &str) -> Error {
    Error::InvalidRequest(format!(
        "stripe: {cur} is a three-decimal ISO-4217 currency, not verified against Stripe's documented amount semantics; refusing rather than guessing"
    ))
}

/// Mirrors cackle's `ErrStripeMalformedResponse`.
pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("stripe: malformed API response: {detail}"))
}

/// The subset of a Stripe Checkout Session object
/// (<https://docs.stripe.com/api/checkout/sessions/object>) this adapter
/// reads — mirrors cackle's `stripeSessionPayload`. Used both for the
/// `Verify`-path API response and the `Webhook`-path event's nested
/// `data.object`.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct StripeSessionPayload {
    pub id: String,
    pub payment_status: String,
    #[serde(default)]
    pub amount_total: u64,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub client_reference_id: String,
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
    /// Present once a payment method has been attached/confirmed. Mirrors
    /// cackle's own `stripeSessionPayload.PaymentIntent` field, which
    /// cackle's `Begin`/`Verify`/`Webhook` never actually read (a dead field
    /// there) — this port DOES use it, in `refund()` only, to look up the
    /// PaymentIntent id Stripe's own Refunds API needs. See `rail.rs`'s
    /// module docs: `refund()` itself is new code, not a cackle port.
    #[serde(default)]
    pub payment_intent: Option<String>,
}

/// Stripe's documented error response shape
/// (<https://docs.stripe.com/api/errors>):
/// `{"error":{"message":"...","type":"...","code":"..."}}`. Mirrors
/// cackle's `stripeErrorEnvelope`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct StripeErrorEnvelope {
    #[serde(default)]
    pub error: StripeErrorBody,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct StripeErrorBody {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub r#type: String,
}

/// Mirrors cackle's `classifyStripeError`: builds an error for a non-2xx
/// Stripe response, best-effort including Stripe's own message.
pub fn classify_error(status: u16, body: &[u8]) -> Error {
    let env: StripeErrorEnvelope = serde_json::from_slice(body).unwrap_or_default();
    let msg = if env.error.message.is_empty() {
        "no message".to_string()
    } else {
        env.error.message
    };
    Error::Rail(format!(
        "stripe: unexpected API response status: http {status}: {msg}"
    ))
}

/// The result of evaluating a [`StripeSessionPayload`] — mirrors the shape
/// cackle's `parseStripeSession` builds into a `Result`, minus the fields
/// (`Provider`, `EventID`, `PaidAt`, `Raw`) that this crate's callers
/// ([`crate::stripe::rail`]'s `verify()` and [`crate::stripe::webhook`]'s
/// event parser) attach themselves, since one is a poll and the other a
/// push and they have different values for those fields.
pub struct SessionOutcome {
    pub reference: String,
    /// `true` only for Stripe's `payment_status == "paid"` — every other
    /// value (`"unpaid"`, `"no_payment_required"`, or anything
    /// unrecognised) is `false`, mirroring cackle's fail-closed `default`
    /// case in `parseStripeSession`'s `switch`.
    pub settled: bool,
    /// `0` unless `settled` — mirrors this crate's honest
    /// "`amount_minor` reports what ACTUALLY moved" convention (see
    /// `stripe/proof.rs` and `patala-hyperswitch`'s own `charge()` docs).
    pub amount_minor: u64,
    pub currency: String,
}

/// Mirrors cackle's `parseStripeSession`: turns a Checkout Session payload
/// into a settlement outcome, failing closed on anything malformed or
/// ambiguous. `payment_status` values are `"paid"` / `"unpaid"` /
/// `"no_payment_required"` — only `"paid"` is ever treated as settled.
pub fn evaluate_session(s: &StripeSessionPayload) -> Result<SessionOutcome, Error> {
    if s.id.is_empty() {
        return Err(malformed("missing session id"));
    }
    let reference = if !s.client_reference_id.is_empty() {
        s.client_reference_id.clone()
    } else if let Some(r) = s.metadata.get("patala_reference") {
        r.clone()
    } else {
        return Err(malformed(
            "session has no client_reference_id or patala_reference metadata to reconcile against",
        ));
    };
    let currency = s.currency.trim().to_ascii_uppercase();

    match s.payment_status.as_str() {
        "paid" => {
            if s.amount_total == 0 {
                return Err(malformed(
                    "payment_status=paid with non-positive amount_total",
                ));
            }
            let amount_minor = stripe_amount_to_minor(s.amount_total, &currency)?;
            Ok(SessionOutcome {
                reference,
                settled: true,
                amount_minor,
                currency,
            })
        }
        // "unpaid" | "no_payment_required" | anything else: fail closed --
        // never treated as paid.
        _ => Ok(SessionOutcome {
            reference,
            settled: false,
            amount_minor: 0,
            currency,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from cackle's internal/payments/stripe_test.go.

    #[test]
    fn ordinary_currency_passes_through() {
        assert_eq!(stripe_amount(5000, "USD").unwrap(), 5000);
    }

    #[test]
    fn zero_decimal_currency_passes_through() {
        assert_eq!(stripe_amount(1000, "JPY").unwrap(), 1000);
    }

    #[test]
    fn isk_forced_two_decimal() {
        assert_eq!(stripe_amount(500, "ISK").unwrap(), 50000);
    }

    #[test]
    fn ugx_forced_two_decimal() {
        assert_eq!(stripe_amount(500, "UGX").unwrap(), 50000);
    }

    #[test]
    fn three_decimal_currency_refused() {
        assert!(stripe_amount(1000, "KWD").is_err());
    }

    #[test]
    fn amount_to_minor_round_trips() {
        for (amt, currency, want) in [
            (5000, "USD", 5000),
            (1000, "JPY", 1000),
            (50000, "ISK", 500),
            (50000, "UGX", 500),
        ] {
            assert_eq!(stripe_amount_to_minor(amt, currency).unwrap(), want);
        }
    }

    #[test]
    fn isk_not_multiple_of_100_rejected() {
        assert!(stripe_amount_to_minor(555, "ISK").is_err());
    }
}
