//! Wire shapes, signing, and query-string codec for the PayFast adapter —
//! ported from cackle's `internal/payments/payfast.go`.
//!
//! Reference: <https://developers.payfast.co.za/docs> (signature
//! generation, ITN). Not re-verified live — see `mod.rs`'s "UNVERIFIED
//! AGAINST LIVE" note.
#![allow(dead_code)]

use patala_core::Error;
use std::collections::HashMap;

/// One `key=value` pair in PayFast's signed field set — mirrors cackle's
/// `payFastKV`. Order matters: PayFast signs the parameter string in the
/// order fields were assembled, NOT alphabetically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kv {
    pub key: String,
    pub value: String,
}

/// Mirrors Go's `url.QueryEscape`: unreserved characters (`A-Za-z0-9-_.~`)
/// pass through unescaped, space becomes `+`, everything else is
/// percent-encoded (uppercase hex).
pub fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Mirrors Go's `url.QueryUnescape`: the inverse of [`query_escape`].
/// Byte-safe (decodes into a byte buffer before the final UTF-8 check) so a
/// multi-byte percent-encoded sequence round-trips correctly rather than
/// being reassembled one Latin-1 code point at a time.
pub fn query_unescape(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                let byte = u8::from_str_radix(hex, 16).ok()?;
                out.push(byte);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Mirrors cackle's `payFastSignature`: MD5 over `fields` in the given
/// order (empty values skipped), each value url-escaped, plus the
/// passphrase appended if non-empty. The `"signature"` key itself must
/// never appear in `fields`.
pub fn compute_signature(fields: &[Kv], passphrase: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for kv in fields {
        if kv.value.is_empty() {
            continue;
        }
        parts.push(format!("{}={}", kv.key, query_escape(&kv.value)));
    }
    if !passphrase.is_empty() {
        parts.push(format!("passphrase={}", query_escape(passphrase)));
    }
    let joined = parts.join("&");
    format!("{:x}", md5::compute(joined.as_bytes()))
}

/// Mirrors cackle's `hmacEqualHex`: constant-time, length-checked
/// byte-for-byte comparison of two hex strings (not case-normalized —
/// exactly like cackle's own helper, since both sides here are always
/// produced by lowercase hex encoders).
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes().iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Mirrors cackle's `verifySignature`: re-parses `raw_body` preserving
/// wire ORDER, decoding each key/value (a pair that fails to decode is
/// silently dropped, matching cackle's own `continue` on error — not a
/// hard parse failure for the whole notification), skipping the
/// `"signature"` field itself, then recomputes and compares.
pub fn verify_signature(raw_body: &[u8], given: &str, passphrase: &str) -> bool {
    if given.is_empty() {
        return false;
    }
    let Ok(body_str) = std::str::from_utf8(raw_body) else {
        return false;
    };
    let mut fields = Vec::new();
    for pair in body_str.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "signature" {
            continue;
        }
        let (Some(decoded_key), Some(decoded_val)) = (query_unescape(k), query_unescape(v)) else {
            continue;
        };
        fields.push(Kv {
            key: decoded_key,
            value: decoded_val,
        });
    }
    let expected = compute_signature(&fields, passphrase);
    constant_time_eq(&expected, given)
}

/// Mirrors cackle's `parsePayFastNotification`'s use of Go's
/// `url.ParseQuery` (a map-based parse used for FIELD VALUE extraction,
/// separate from [`verify_signature`]'s own order-preserving parse used
/// only for signature recomputation — cackle genuinely parses the same raw
/// body twice, for two different purposes).
pub fn parse_query_map(raw_body: &[u8]) -> Result<HashMap<String, String>, Error> {
    let body_str = std::str::from_utf8(raw_body)
        .map_err(|_| malformed("notification body is not valid UTF-8"))?;
    let mut map = HashMap::new();
    for pair in body_str.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if let (Some(dk), Some(dv)) = (query_unescape(k), query_unescape(v)) {
            map.insert(dk, dv);
        }
    }
    Ok(map)
}

/// The settlement outcome of evaluating a parsed ITN's field map.
pub struct NotificationOutcome {
    pub reference: String,
    pub event_id: String,
    pub settled: bool,
    pub amount_minor: u64,
}

/// Mirrors cackle's `Webhook`'s field-extraction + `payment_status` switch:
/// only `"COMPLETE"` (with a positive amount and a `pf_payment_id`) ever
/// settles; `"FAILED"`/`"CANCELLED"`/anything unrecognised fail closed.
pub fn evaluate_notification(
    values: &HashMap<String, String>,
) -> Result<NotificationOutcome, Error> {
    let reference = values.get("m_payment_id").cloned().unwrap_or_default();
    let amount_str = values.get("amount_gross").cloned().unwrap_or_default();
    let pf_payment_id = values.get("pf_payment_id").cloned().unwrap_or_default();
    let payment_status = values.get("payment_status").cloned().unwrap_or_default();

    if reference.is_empty() || amount_str.is_empty() {
        return Err(malformed("missing m_payment_id or amount_gross"));
    }
    let amount_minor = crate::currency::major_string_to_minor(&amount_str, "ZAR")
        .map_err(|e| malformed(&format!("amount_gross {amount_str:?}: {e}")))?;

    match payment_status.as_str() {
        "COMPLETE" => {
            if amount_minor == 0 || pf_payment_id.is_empty() {
                return Err(malformed(
                    "COMPLETE status with non-positive amount or no pf_payment_id",
                ));
            }
            Ok(NotificationOutcome {
                reference,
                event_id: pf_payment_id,
                settled: true,
                amount_minor,
            })
        }
        // "FAILED" | "CANCELLED" | anything else: fail closed.
        _ => Ok(NotificationOutcome {
            reference,
            event_id: pf_payment_id,
            settled: false,
            amount_minor: 0,
        }),
    }
}

/// Mirrors cackle's `ErrPayFastUnsupportedCurrency`.
pub fn unsupported_currency(got: &str) -> Error {
    Error::InvalidRequest(format!("payfast: only ZAR is supported, got {got:?}"))
}

/// Mirrors cackle's `ErrPayFastUnexpectedStatus`.
pub fn unexpected_status(msg: &str) -> Error {
    Error::Rail(format!("payfast: unexpected API response status: {msg}"))
}

/// Mirrors cackle's `ErrPayFastMalformedNotification`.
pub fn malformed(detail: &str) -> Error {
    Error::Rail(format!("payfast: malformed ITN payload: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_escape_matches_go_url_query_escape_shape() {
        assert_eq!(query_escape("hello world"), "hello+world");
        assert_eq!(query_escape("R100.00"), "R100.00");
        assert_eq!(query_escape("a/b"), "a%2Fb");
    }

    #[test]
    fn query_unescape_round_trips() {
        for s in ["hello world", "R100.00", "a/b&c=d"] {
            assert_eq!(query_unescape(&query_escape(s)).unwrap(), s);
        }
    }

    #[test]
    fn compute_signature_skips_empty_values_and_appends_passphrase() {
        let fields = vec![
            Kv {
                key: "m_payment_id".into(),
                value: "ord_1".into(),
            },
            Kv {
                key: "notify_url".into(),
                value: String::new(),
            },
            Kv {
                key: "amount".into(),
                value: "100.00".into(),
            },
        ];
        let with_pass = compute_signature(&fields, "secret");
        let without_pass = compute_signature(&fields, "");
        assert_ne!(with_pass, without_pass);
        assert_eq!(with_pass.len(), 32, "MD5 hex digest is 32 chars");
    }

    #[test]
    fn verify_signature_matches_freshly_computed() {
        let fields = vec![
            Kv {
                key: "m_payment_id".into(),
                value: "ord_1".into(),
            },
            Kv {
                key: "amount_gross".into(),
                value: "100.00".into(),
            },
        ];
        let sig = compute_signature(&fields, "test-passphrase");
        let body = format!("m_payment_id=ord_1&amount_gross=100.00&signature={sig}");
        assert!(verify_signature(body.as_bytes(), &sig, "test-passphrase"));
        assert!(!verify_signature(body.as_bytes(), &sig, "wrong-passphrase"));
    }

    #[test]
    fn evaluate_notification_complete_settles() {
        let mut values = HashMap::new();
        values.insert("m_payment_id".to_string(), "ord_1".to_string());
        values.insert("payment_status".to_string(), "COMPLETE".to_string());
        values.insert("amount_gross".to_string(), "100.00".to_string());
        values.insert("pf_payment_id".to_string(), "pf_1".to_string());
        let outcome = evaluate_notification(&values).unwrap();
        assert!(outcome.settled);
        assert_eq!(outcome.amount_minor, 10000);
        assert_eq!(outcome.reference, "ord_1");
    }

    #[test]
    fn evaluate_notification_failed_is_not_settled() {
        let mut values = HashMap::new();
        values.insert("m_payment_id".to_string(), "ord_1".to_string());
        values.insert("payment_status".to_string(), "FAILED".to_string());
        values.insert("amount_gross".to_string(), "100.00".to_string());
        let outcome = evaluate_notification(&values).unwrap();
        assert!(!outcome.settled);
    }

    #[test]
    fn evaluate_notification_missing_fields_is_malformed() {
        let values = HashMap::new();
        assert!(evaluate_notification(&values).is_err());
    }
}
