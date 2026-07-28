//! §3 THE SEAM, push side: the inbound-webhook types every rail that can
//! receive a processor callback verifies through.
//!
//! A rail's *pull* path is [`crate::PaymentRail::verify`]: the caller holds a
//! [`crate::Receipt`] and asks the rail to re-derive whether it still holds.
//! Its *push* path is this module: the processor makes an HTTP request to the
//! caller, and the only useful question is "did this delivery really come
//! from the rail, and what does it say?". Both answers must come from the
//! same trait, or a consumer that reaches patala through a binding (the
//! UniFFI surface, the sidecar) can only ever poll.
//!
//! **Verification is over the bytes as received.** Every scheme in use
//! (HMAC over the raw body, HMAC over `"{timestamp}.{body}"`, a hash of one
//! field, a bare shared secret echoed in a header) signs what was actually
//! sent. So [`WebhookDelivery`] carries `Vec<u8>`, not a parsed value, and
//! deliberately does **not** derive `Serialize`/`Deserialize`: a delivery
//! that has been through a JSON round-trip is no longer the thing the
//! processor signed, and offering that round-trip would invite exactly the
//! bug it cannot detect. [`WebhookEvent`] — the *output* — does derive both,
//! because it is a result, not a signed input.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One inbound webhook delivery, exactly as it arrived on the wire.
///
/// Header names are stored lower-cased and looked up case-insensitively
/// (HTTP header names are case-insensitive; a caller forwarding them from
/// axum, `net/http`, WSGI or a raw socket must not have to care which case
/// its own stack produced).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebhookDelivery {
    /// The literal request body bytes, before any JSON decode. Never a
    /// re-serialized value — see the module docs.
    pub raw_body: Vec<u8>,
    /// Request headers, keyed by lower-cased name.
    pub headers: BTreeMap<String, String>,
    /// The request's decoded query-string parameters.
    ///
    /// Not every scheme puts its secret in a header: LNbits has no signing
    /// scheme at all, so the compensating control is an operator-chosen
    /// secret embedded in the webhook URL the rail registers
    /// (`?secret=...`). A rail that has no such parameter simply never
    /// reads this map. Keys are used verbatim — query parameters, unlike
    /// header names, are case-sensitive.
    pub query: BTreeMap<String, String>,
    /// Unix seconds to check replay windows against. A rail whose scheme has
    /// a timestamp tolerance (Stripe's 5 minutes, Yoco/Svix's 5 minutes)
    /// reads `now` from here rather than the system clock, so the same
    /// delivery is reproducible in a test and across a process boundary.
    pub now_unix: u64,
}

impl WebhookDelivery {
    /// A delivery carrying `raw_body`, no headers, at `now_unix`.
    pub fn new(raw_body: impl Into<Vec<u8>>, now_unix: u64) -> Self {
        Self {
            raw_body: raw_body.into(),
            headers: BTreeMap::new(),
            query: BTreeMap::new(),
            now_unix,
        }
    }

    /// Add one header. Chainable; the name is lower-cased on the way in.
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.insert(name.to_ascii_lowercase(), value.into());
        self
    }

    /// Add every `(name, value)` pair, lower-casing each name. This is what
    /// a binding or an HTTP server uses to forward a whole header map.
    pub fn with_headers<K, V>(mut self, headers: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: AsRef<str>,
        V: Into<String>,
    {
        for (k, v) in headers {
            self.headers
                .insert(k.as_ref().to_ascii_lowercase(), v.into());
        }
        self
    }

    /// Add one query-string parameter. Chainable; the name is used verbatim.
    pub fn with_query_param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.insert(name.into(), value.into());
        self
    }

    /// Add every `(name, value)` query-string pair.
    pub fn with_query<K, V>(mut self, params: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in params {
            self.query.insert(k.into(), v.into());
        }
        self
    }

    /// The value of `name`, case-insensitively, if present.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(|s| s.as_str())
    }

    /// The query-string parameter `name`, if present.
    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query.get(name).map(|s| s.as_str())
    }

    /// The value of `name`, or `""` when absent.
    ///
    /// Every verifier in this workspace treats a missing signature header as
    /// a rejection, never as "skip the check" — so collapsing absent and
    /// empty is safe here and keeps each rail's call site free of an
    /// `unwrap_or_default()` that could later be mistaken for a default
    /// *value* rather than a fail-closed one.
    pub fn header_or_empty(&self, name: &str) -> &str {
        self.header(name).unwrap_or("")
    }
}

/// What a verified delivery established about settlement.
///
/// Three states, not a bool, because several real schemes authenticate a
/// delivery without asserting anything about money: BTCPay, Coinbase
/// Commerce, OpenNode, LNbits and Mollie all sign (or, for Mollie, send) a
/// notification that names an object and nothing else. Flattening those onto
/// `settled: false` would report "this payment did not settle" when the truth
/// is "this delivery is genuine and says nothing about settlement" — the
/// exact class of confusion [`crate::PaymentRail::verify`]'s own fail-closed
/// contract exists to prevent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WebhookStatus {
    /// The rail established that the payment named by this delivery has
    /// settled. [`WebhookEvent::amount_minor`]/[`WebhookEvent::currency`]
    /// carry what the rail reported — processor-reported data, to be
    /// reconciled against the caller's own stored order before it is
    /// trusted for anything.
    Settled,
    /// The delivery is authentic and the rail established that the payment
    /// has **not** settled — still pending, failed, cancelled, expired.
    NotSettled,
    /// The delivery is authentic, but carries no settlement claim this rail
    /// can act on by itself. [`WebhookEvent::object_id`] names what the
    /// delivery was about; the caller must find its own stored
    /// [`crate::Receipt`] for that object and call
    /// [`crate::PaymentRail::verify`]. **Never treat this as settlement.**
    Unconfirmed,
}

/// The outcome of [`crate::PaymentRail::verify_webhook`] — a delivery that
/// has been authenticated, and whatever the rail could honestly say about it.
///
/// Reaching this type at all means the signature check passed; an
/// unauthenticated delivery is an `Err`, never a `WebhookEvent` with a
/// negative status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookEvent {
    /// The rail that verified this (matches [`crate::PaymentRail::id`]).
    pub rail_id: String,
    /// A stable identifier for **this delivery**, for replay/dedup. Never
    /// empty: a caller cannot suppress a duplicate it cannot name. Repeated
    /// delivery of the same event must produce the same value.
    pub event_id: String,
    /// The [`crate::PayRequest::reference`] this delivery is about, when the
    /// delivery itself carries it. Empty when the rail's callback names only
    /// a processor-side object — see [`Self::object_id`].
    pub reference: String,
    /// The processor-side object this delivery names (invoice id, charge id,
    /// payment id, checkout token). Empty when the rail's callback carries
    /// no such id. This is the lookup key for a
    /// [`WebhookStatus::Unconfirmed`] delivery.
    pub object_id: String,
    /// What this delivery established. See [`WebhookStatus`].
    pub status: WebhookStatus,
    /// Amount the rail reported settled, in minor units — `0` unless
    /// [`Self::status`] is [`WebhookStatus::Settled`] and the rail reported
    /// one. Never a float (`PATALA.md` §3, §8).
    pub amount_minor: u64,
    /// Currency the rail reported settling in — empty unless the rail
    /// reported one.
    pub currency: String,
}

impl WebhookEvent {
    /// A [`WebhookStatus::Unconfirmed`] event: authentic, names `object_id`,
    /// asserts nothing about money. The shape every signature-only callback
    /// produces.
    pub fn unconfirmed(
        rail_id: impl Into<String>,
        event_id: impl Into<String>,
        object_id: impl Into<String>,
    ) -> Self {
        Self {
            rail_id: rail_id.into(),
            event_id: event_id.into(),
            reference: String::new(),
            object_id: object_id.into(),
            status: WebhookStatus::Unconfirmed,
            amount_minor: 0,
            currency: String::new(),
        }
    }

    /// An event from a rail that *did* establish settlement one way or the
    /// other: [`WebhookStatus::Settled`] when `settled`,
    /// [`WebhookStatus::NotSettled`] otherwise.
    ///
    /// The money fields are cleared in the `NotSettled` case, here and only
    /// here. Several processors report an amount on a delivery that has not
    /// settled (a Stripe Checkout Session whose `payment_status` is still
    /// `unpaid` carries its `amount_total`), and a caller that read
    /// `amount_minor` without first checking `status` would be reading a
    /// number nobody paid. Every rail routes through this constructor so
    /// that invariant cannot drift per-rail.
    pub fn settlement(
        rail_id: impl Into<String>,
        event_id: impl Into<String>,
        reference: impl Into<String>,
        settled: bool,
        amount_minor: u64,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            rail_id: rail_id.into(),
            event_id: event_id.into(),
            reference: reference.into(),
            object_id: String::new(),
            status: if settled {
                WebhookStatus::Settled
            } else {
                WebhookStatus::NotSettled
            },
            amount_minor: if settled { amount_minor } else { 0 },
            currency: if settled {
                currency.into()
            } else {
                String::new()
            },
        }
    }

    /// Attach the processor-side object id this delivery named.
    pub fn with_object_id(mut self, object_id: impl Into<String>) -> Self {
        self.object_id = object_id.into();
        self
    }

    /// True only for [`WebhookStatus::Settled`]. The one predicate a caller
    /// should gate entitlement on — and even then only after reconciling
    /// [`Self::amount_minor`]/[`Self::currency`] against its own record.
    pub fn is_settled(&self) -> bool {
        self.status == WebhookStatus::Settled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lookup_is_case_insensitive_both_ways() {
        let d = WebhookDelivery::new(b"{}".to_vec(), 0)
            .with_header("Stripe-Signature", "t=1,v1=deadbeef")
            .with_headers([("X-Request-Id", "req_1")]);

        assert_eq!(d.header("stripe-signature"), Some("t=1,v1=deadbeef"));
        assert_eq!(d.header("STRIPE-SIGNATURE"), Some("t=1,v1=deadbeef"));
        assert_eq!(d.header("x-request-id"), Some("req_1"));
        assert_eq!(d.header("nope"), None);
        assert_eq!(d.header_or_empty("nope"), "");
    }

    #[test]
    fn query_params_are_case_sensitive_unlike_headers() {
        let d = WebhookDelivery::new(b"{}".to_vec(), 0)
            .with_query_param("secret", "s3cr3t")
            .with_query([("Other", "v")]);

        assert_eq!(d.query_param("secret"), Some("s3cr3t"));
        // A query parameter is NOT a header: no case folding, or an LNbits
        // `?secret=` check would accept `?SECRET=` that LNbits never sent.
        assert_eq!(d.query_param("Secret"), None);
        assert_eq!(d.query_param("Other"), Some("v"));
    }

    #[test]
    fn unconfirmed_asserts_no_money() {
        let e = WebhookEvent::unconfirmed("btcpay", "evt_1", "inv_1");
        assert_eq!(e.status, WebhookStatus::Unconfirmed);
        assert!(!e.is_settled());
        assert_eq!(e.amount_minor, 0);
        assert!(e.currency.is_empty());
        assert_eq!(e.object_id, "inv_1");
    }

    #[test]
    fn not_settled_is_distinct_from_unconfirmed() {
        // The whole reason `status` is not a bool: "the rail says this did
        // not settle" and "the rail cannot say" must never collapse.
        let unknown = WebhookEvent::unconfirmed("btcpay", "evt_1", "inv_1");
        let negative = WebhookEvent {
            status: WebhookStatus::NotSettled,
            ..WebhookEvent::unconfirmed("stripe", "evt_2", "cs_2")
        };
        assert_ne!(unknown.status, negative.status);
        assert!(!unknown.is_settled() && !negative.is_settled());
    }

    #[test]
    fn settlement_clears_money_when_not_settled() {
        // A Stripe Checkout Session can be `completed` and still `unpaid`,
        // carrying its full amount_total. Nobody paid it, so nothing may
        // read an amount off it.
        let unpaid = WebhookEvent::settlement("stripe", "evt_1", "ord_1", false, 5000, "USD");
        assert_eq!(unpaid.status, WebhookStatus::NotSettled);
        assert_eq!(unpaid.amount_minor, 0);
        assert!(unpaid.currency.is_empty());

        let paid = WebhookEvent::settlement("stripe", "evt_2", "ord_2", true, 5000, "USD");
        assert!(paid.is_settled());
        assert_eq!(paid.amount_minor, 5000);
        assert_eq!(paid.currency, "USD");
        assert_eq!(paid.with_object_id("cs_2").object_id, "cs_2");
    }
}
