//! [`LNbitsRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/lnbits.go` (`LNbitsProvider`): a thin wallet/accounts
//! layer in front of a Lightning node, self-hosted (or self-operated) by
//! the organiser against their OWN node/channels.
//!
//! Reference: LNbits' core Payments API
//! (<https://legend.lnbits.com/guide/api.html>). Not re-verified live from
//! this environment — see this crate's `PORTING.md` "UNVERIFIED AGAINST
//! LIVE" note.
//!
//! ## `RailClass`/`holds_funds` — what was chosen and why
//!
//! `RailClass::NonCustodialFinal`, `holds_funds: false` — identical
//! reasoning to `btcpay::rail`'s: LNbits is software the organiser runs
//! against their OWN node/channels; there is no third party ever in custody
//! of the funds, and Lightning settlement is genuinely, protocol-level
//! instant and final (an HTLC either resolves or it doesn't — no
//! chargeback). Unlike `btcpay`'s documented ambiguity between on-chain
//! (minutes) and Lightning (instant) settlement, LNbits is Lightning-only,
//! so `settlement: Settlement::Instant` is hardcoded, not configurable —
//! cackle's own file doc comment states this outright: *"Lightning payments
//! settle via HTLC the instant the recipient's node releases the preimage —
//! there is no block-confirmation wait."*
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a FIXED-AMOUNT BOLT11 invoice, returns
//!   payment instructions) maps to [`PaymentRail::charge`]. Cackle's
//!   `Order.CallbackURL`/`EventID`/`OrgID` have no role in LNbits' invoice
//!   creation at all — same as `btcpay`, this port does NOT reinterpret
//!   `PayRequest::destination` as anything LNbits-specific (mirrors
//!   `manual.rs`'s precedent; callers pass a placeholder to satisfy
//!   `PayRequest::validate()`).
//! - cackle's `Verify(reference)` maps to [`PaymentRail::verify`]. See
//!   `proof::ChargeProof`'s module docs for how this port structurally
//!   resolves cackle's own documented "does not survive a process restart"
//!   limitation (embedding the fiat amount/currency/creation-time in
//!   `proof` rather than an in-memory map) WITHOUT changing any of the
//!   actual payment semantics: a FIXED-AMOUNT BOLT11 invoice still only
//!   ever settles for exactly its own amount or not at all (the protocol
//!   invariant cackle's own doc comment cites as the reason underpayment is
//!   structurally impossible here — no extra logic in this file makes that
//!   true, same as cackle), and the quote-TTL expiry check
//!   (`config::LNbitsConfig::quote_ttl_secs`) is ported byte-for-byte,
//!   INCLUDING cackle's own explicitly-documented priority rule: **a
//!   `paid: true` response from LNbits always wins over the expiry check**
//!   (cackle's own test, `TestLNbitsVerify_LatePaymentAfterExpiryStillFailsClosed`,
//!   is misleadingly named — its own comment clarifies the behaviour is
//!   deliberate: *"paid=true always wins... so any future change is
//!   deliberate, not accidental"* — ported here as
//!   `verify_paid_wins_over_expiry_even_though_test_name_says_otherwise`).
//!   Cackle's `ErrLNbitsUnknownReference` (a `payment_hash` this process
//!   never created) has no equivalent here: since the fiat association now
//!   lives in the caller-supplied `Receipt` itself rather than server-side
//!   state, there is nothing that can be "unknown" — a garbage/foreign
//!   `proof` simply fails `Ok(false)` the same way every other adapter's
//!   `verify()` does for an undecodable proof.
//! - cackle's `Webhook` maps to [`PaymentRail::verify_webhook`], which
//!   delegates to the free function
//!   [`crate::lnbits::webhook::verify_and_extract`]. The function keeps the
//!   pure, directly-testable shape; the trait method is what a consumer
//!   dispatching through `dyn PaymentRail` — the UniFFI binding, the
//!   sidecar — can actually reach.
//! - `refund()`: **left as the trait default (`Error::Unsupported`)** — same
//!   reasoning as `btcpay::rail`'s: cackle's `Capabilities.Refunds: false`
//!   has no "supports it, not implemented" comment for LNbits (unlike
//!   Paystack's), and `patala_core::PaymentRail::refund`'s own doc comment
//!   requires `NonCustodialFinal` rails to return `Unsupported` since
//!   finality is the whole point. A Lightning "refund" would be a brand new
//!   payment FROM the organiser's wallet back to the buyer, requiring a
//!   buyer-supplied invoice this trait has no field for — not fabricated
//!   here.
//! - **Not ported**: `NewLNbitsWithStore`/`RecordStore` durability — made
//!   structurally unnecessary by the `proof`-embedding design above, not
//!   dropped as an oversight.

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::lnbits::config::LNbitsConfig;
use crate::lnbits::models::{self, CreatePaymentResponse, PaymentStatus};
use crate::lnbits::proof::ChargeProof;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for an lnbits payment hash"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to a self-hosted (or self-operated) LNbits
/// wallet. See module docs for the full `Provider` -> `PaymentRail` mapping
/// and the `RailClass`/`holds_funds` reasoning.
pub struct LNbitsRail {
    id: String,
    config: LNbitsConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl LNbitsRail {
    /// Build a rail from configuration. Fails if any required field is
    /// empty.
    pub fn new(config: LNbitsConfig) -> Result<Self> {
        if config.base_url.trim().is_empty() {
            return Err(Error::InvalidRequest("base_url must not be empty".into()));
        }
        if config.api_key.trim().is_empty() {
            return Err(Error::InvalidRequest("api_key must not be empty".into()));
        }
        if config.webhook_secret.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_secret must not be empty".into(),
            ));
        }
        if config.quote_ttl_secs == 0 {
            return Err(Error::InvalidRequest(
                "quote_ttl_secs must be a positive integer".into(),
            ));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building lnbits http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::NonCustodialFinal,
            reversible: false,
            requires_kyc: config.requires_kyc,
            holds_funds: false, // self-hosted: the organiser's own node custodies -- never a third party, never patala.
            currencies: config.currencies.clone(),
            settlement: Settlement::Instant, // Lightning HTLC -- see module docs.
            atomic_multi_party: false, // always false: N payouts here are N independent API calls, never one atomic event (B3)
        };

        let base_url = config.base_url.clone();
        Ok(Self {
            id: "lnbits".to_string(),
            config,
            http,
            capabilities,
            base_url,
        })
    }

    fn check_currency(&self, currency: &str) -> Result<()> {
        if self.capabilities.currencies.is_empty()
            || self
                .capabilities
                .currencies
                .iter()
                .any(|c| c.eq_ignore_ascii_case(currency))
        {
            Ok(())
        } else {
            Err(Error::InvalidRequest(format!(
                "rail {} does not support currency {currency}",
                self.id
            )))
        }
    }

    async fn do_json(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(Vec<u8>, u16)> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .http
            .request(method, &url)
            .header("X-Api-Key", &self.config.api_key)
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("lnbits: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("lnbits: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("lnbits: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for LNbitsRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::ignored`], because the `lnbits` rail never reads
    /// `destination` at all (see this module's docs above). The field exists
    /// on the request only because `PayRequest::validate()` requires a
    /// non-empty one on every rail.
    ///
    /// The verdict is therefore always
    /// [`patala_core::DestinationStatus::Unknown`] for a non-empty string:
    /// there is no format to be right or wrong about, so no format check is
    /// invented — including no refusal of a wallet address, which is
    /// genuinely harmless in a field nothing reads. What the reason says
    /// instead is the thing a caller needs to know: setting this steers
    /// nothing, and giving a customer their money back on this rail is the
    /// processor's refund path, never a charge to a destination.
    fn validate_destination(&self, dest: &str) -> patala_core::DestinationVerdict {
        crate::destination::ignored(self.id(), dest)
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        // NEEDS-CONFIRMATION (mirrors stripe/paystack's identical note): no
        // pre-charge fee-quote endpoint in LNbits' documented API or
        // cackle's own adapter.
        Ok(Quote {
            rail_id: self.id.clone(),
            amount_minor: req.amount_minor,
            currency: req.currency.clone(),
            fee_minor: 0,
            total_minor: req.amount_minor,
            settlement: self.capabilities.settlement,
            expires_at_unix: now_unix().saturating_add(self.config.quote_ttl_secs),
        })
    }

    async fn charge(&self, req: &PayRequest) -> Result<Receipt> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        // See module docs: `destination` is unused by this rail.
        let currency = req.currency.trim().to_ascii_uppercase();
        let amount_str = crate::currency::minor_to_major_string(req.amount_minor, &currency)
            .map_err(|e| Error::InvalidRequest(format!("lnbits: {e}")))?;
        let amount_float: f64 = amount_str
            .parse()
            .map_err(|_| Error::Rail("lnbits: could not render amount as a number".to_string()))?;

        let memo = format!("patala order {}", req.reference);
        let mut body = serde_json::json!({
            "out": false,
            "amount": amount_float,
            "unit": currency.to_ascii_lowercase(),
            "memo": memo,
            "expiry": self.config.quote_ttl_secs,
        });
        if let Some(webhook_url) = &self.config.webhook_url {
            let sep = if webhook_url.contains('?') { "&" } else { "?" };
            let full = format!(
                "{webhook_url}{sep}secret={}",
                urlencoding_minimal(&self.config.webhook_secret)
            );
            body["webhook"] = serde_json::Value::String(full);
        }

        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/api/v1/payments", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }
        let parsed: CreatePaymentResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.payment_hash.is_empty() || parsed.payment_request.is_empty() {
            return Err(models::malformed("empty payment_hash or payment_request"));
        }

        let now = now_unix();
        let proof = ChargeProof {
            payment_hash: parsed.payment_hash,
            payment_request: parsed.payment_request,
            amount_minor: req.amount_minor,
            currency: currency.clone(),
            created_at_unix: now,
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see PORTING.md §5
            currency,
            reference: req.reference.clone(),
            proof: proof.to_bytes(),
            settled_at_unix: 0,
        })
    }

    async fn verify(&self, receipt: &Receipt) -> Result<bool> {
        if receipt.rail_id != self.id {
            return Ok(false);
        }
        let Some(proof) = ChargeProof::from_bytes(&receipt.proof) else {
            return Ok(false);
        };
        if !proof.currency.eq_ignore_ascii_case(&receipt.currency) {
            return Ok(false);
        }
        if proof.amount_minor < receipt.amount_minor {
            return Ok(false);
        }
        let payment_hash = safe_path_segment(&proof.payment_hash)?;

        let path = format!("/api/v1/payments/{payment_hash}");
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let parsed: PaymentStatus =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;

        Ok(
            verify_paid_wins_over_expiry_even_though_test_name_says_otherwise(
                parsed.paid,
                proof.created_at_unix,
                self.config.quote_ttl_secs,
            ),
        )
    }

    // refund(): left as the trait default (Error::Unsupported). See module
    // docs for why this is honest rather than a shortcut.

    /// Verify an LNbits webhook delivery — see
    /// [`crate::lnbits::webhook::verify_and_extract`].
    ///
    /// LNbits has no signing scheme at all, so the compensating control (as
    /// designed upstream, not invented here) is an operator-chosen secret
    /// embedded in the registered webhook URL. It is therefore read from
    /// [`WebhookDelivery::query`]'s `secret` parameter, not a header — a
    /// caller forwarding a delivery MUST populate the query map or this
    /// fails closed with a missing-secret error.
    ///
    /// Reports [`patala_core::WebhookStatus::Unconfirmed`], never a
    /// settlement: take [`WebhookEvent::object_id`] (the payment hash), find
    /// your stored [`Receipt`], and call [`Self::verify`].
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event = crate::lnbits::webhook::verify_and_extract(
            &self.config.webhook_secret,
            delivery.query_param("secret"),
            &delivery.raw_body,
        )
        .map_err(|e| Error::InvalidRequest(e.to_string()))?;
        Ok(WebhookEvent::unconfirmed(
            &self.id,
            event.event_id,
            event.payment_hash,
        ))
    }
}

/// Mirrors cackle's `verifyAgainstRecord` switch EXACTLY, including its own
/// explicitly-documented, deliberate priority rule: `paid` is checked FIRST
/// and wins regardless of whether the quote has expired. Only when NOT paid
/// does the expiry window matter (and even then, patala's `bool`-only
/// `verify()` collapses "pending" and "expired-unpaid" into the same
/// `false` — see `PORTING.md`, `verify()` can only ever report settled/not).
fn verify_paid_wins_over_expiry_even_though_test_name_says_otherwise(
    paid: bool,
    created_at_unix: u64,
    quote_ttl_secs: u64,
) -> bool {
    if paid {
        return true;
    }
    let _expired = now_unix().saturating_sub(created_at_unix) > quote_ttl_secs;
    false
}

/// A minimal query-value encoder for the one place this rail needs one (the
/// webhook secret embedded in a `?secret=` URL), avoiding a dependency on a
/// URL-encoding crate. Percent-encodes everything outside the RFC 3986
/// "unreserved" set, which is sufficient for an operator-chosen secret
/// string that is never attacker-controlled.
fn urlencoding_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(amount: u64, currency: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: "unused-for-lnbits".into(),
            reference: reference.into(),
        }
    }

    fn config() -> LNbitsConfig {
        LNbitsConfig {
            base_url: "http://unused".to_string(),
            api_key: "test-api-key".to_string(),
            webhook_secret: "test-webhook-secret".to_string(),
            webhook_url: None,
            quote_ttl_secs: 900,
            requires_kyc: false,
            currencies: Vec::new(),
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> LNbitsRail {
        let mut rail = LNbitsRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/lnbits_test.go.

    #[test]
    fn capabilities_are_honest_about_non_custodial_instant_settlement() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::NonCustodialFinal);
        assert!(!caps.holds_funds);
        assert_eq!(caps.settlement, Settlement::Instant);
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/payments"))
            .and(header("X-Api-Key", "test-api-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "payment_hash": "hash123", "payment_request": "lnbc1..."
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail.charge(&req(1234, "USD", "order_1")).await.unwrap();
        assert_eq!(receipt.reference, "order_1");
        assert_eq!(receipt.amount_minor, 0);
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.payment_hash, "hash123");
        assert_eq!(proof.payment_request, "lnbc1...");
        assert_eq!(proof.amount_minor, 1234);
        assert_eq!(proof.currency, "USD");
    }

    #[tokio::test]
    async fn charge_rejects_non_positive_amount() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let err = rail.charge(&req(0, "USD", "order_1")).await.unwrap_err();
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    fn receipt_with_proof(amount_minor: u64, currency: &str, proof: ChargeProof) -> Receipt {
        Receipt {
            rail_id: "lnbits".into(),
            amount_minor,
            currency: currency.into(),
            reference: "order_1".into(),
            proof: proof.to_bytes(),
            settled_at_unix: 0,
        }
    }

    #[tokio::test]
    async fn verify_paid_reports_original_fiat_amount() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/payments/hash123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"paid": true})))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let proof = ChargeProof {
            payment_hash: "hash123".into(),
            payment_request: "lnbc1...".into(),
            amount_minor: 1234,
            currency: "USD".into(),
            created_at_unix: now_unix(),
        };
        let receipt = receipt_with_proof(1234, "USD", proof);
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_unpaid_stays_false() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/payments/hash123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"paid": false})))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let proof = ChargeProof {
            payment_hash: "hash123".into(),
            payment_request: "lnbc1...".into(),
            amount_minor: 1234,
            currency: "USD".into(),
            created_at_unix: now_unix(),
        };
        let receipt = receipt_with_proof(1234, "USD", proof);
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_expired_quote_stays_false() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/payments/hash123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"paid": false})))
            .mount(&server)
            .await;

        let mut rail = rail_for(server.uri());
        rail.config.quote_ttl_secs = 1;
        let proof = ChargeProof {
            payment_hash: "hash123".into(),
            payment_request: "lnbc1...".into(),
            amount_minor: 1234,
            currency: "USD".into(),
            created_at_unix: now_unix().saturating_sub(3600),
        };
        let receipt = receipt_with_proof(1234, "USD", proof);
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_late_payment_after_expiry_still_reports_paid_true_wins() {
        // Mirrors cackle's TestLNbitsVerify_LatePaymentAfterExpiryStillFailsClosed:
        // despite the test's name, cackle's own comment says paid=true
        // always wins over the expiry check -- ported here identically.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/payments/hash123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"paid": true})))
            .mount(&server)
            .await;

        let mut rail = rail_for(server.uri());
        rail.config.quote_ttl_secs = 1;
        let proof = ChargeProof {
            payment_hash: "hash123".into(),
            payment_request: "lnbc1...".into(),
            amount_minor: 1234,
            currency: "USD".into(),
            created_at_unix: now_unix().saturating_sub(3600),
        };
        let receipt = receipt_with_proof(1234, "USD", proof);
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_malformed_json_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/payments/hash123"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let proof = ChargeProof {
            payment_hash: "hash123".into(),
            payment_request: "lnbc1...".into(),
            amount_minor: 1234,
            currency: "USD".into(),
            created_at_unix: now_unix(),
        };
        let receipt = receipt_with_proof(1234, "USD", proof);
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_server_error_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/payments/hash123"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let proof = ChargeProof {
            payment_hash: "hash123".into(),
            payment_request: "lnbc1...".into(),
            amount_minor: 1234,
            currency: "USD".into(),
            created_at_unix: now_unix(),
        };
        let receipt = receipt_with_proof(1234, "USD", proof);
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_garbage_proof_fails_closed_without_network() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let mut receipt = receipt_with_proof(
            1234,
            "USD",
            ChargeProof {
                payment_hash: "hash123".into(),
                payment_request: "lnbc1...".into(),
                amount_minor: 1234,
                currency: "USD".into(),
                created_at_unix: now_unix(),
            },
        );
        receipt.proof = b"not a valid proof".to_vec();
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = receipt_with_proof(
            1234,
            "USD",
            ChargeProof {
                payment_hash: "hash123".into(),
                payment_request: "lnbc1...".into(),
                amount_minor: 1234,
                currency: "USD".into(),
                created_at_unix: now_unix(),
            },
        );
        let err = rail.refund(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported("refund")));
    }
}
