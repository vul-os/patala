//! [`MollieRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/mollie.go` (`MollieProvider`).
//!
//! Built against Mollie's DOCUMENTED public API (Payments) — see this
//! crate's `PORTING.md` "UNVERIFIED AGAINST LIVE" disclosure.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a Mollie payment, returns its hosted checkout
//!   URL) maps to [`PaymentRail::charge`]. **Gap vs cackle**: Mollie's
//!   Create Payment requires a `redirectUrl`, and `PayRequest` has no
//!   callback-url field — this port reinterprets `PayRequest::destination`
//!   AS that redirect URL, exactly the same reinterpretation
//!   `stripe::rail::StripeRail` applies to the same field. Callers of
//!   `MollieRail::charge` must pass the desired post-payment return URL as
//!   `destination`.
//! - `webhookUrl` is NOT taken from `PayRequest` at all — it is fixed
//!   per-deployment config (`MollieConfig::webhook_url`), exactly as
//!   cackle's own `Begin` uses `p.webhookURL` (a field on the provider, not
//!   a per-`Order` value) unconditionally.
//! - **Flagged divergence, resolving an apparent inconsistency in cackle's
//!   own source — see `proof.rs`'s module docs for the full reasoning**:
//!   `Receipt::reference` echoes `PayRequest::reference` (the caller's own
//!   key, as `patala_core::Receipt`'s own contract requires and every other
//!   rail in this crate does), while Mollie's own payment id lives in
//!   `proof` — NOT `Receipt::reference`, unlike what cackle's `Verify` doc
//!   comment (incorrectly) claims cackle's own `Begin` does.
//! - cackle's `Verify(reference)` maps to [`PaymentRail::verify`], but reads
//!   the payment id from `proof` (see above) rather than
//!   `receipt.reference`.
//! - cackle's `Webhook` is ported as
//!   [`crate::mollie::webhook::extract_payment_id`] (the pure half) plus
//!   [`MollieRail::handle_webhook`] (this method — the network half) — see
//!   `webhook.rs`'s module docs on why this is a genuine structural
//!   divergence from every other adapter's pure-function `webhook.rs`, not
//!   an inconsistency: Mollie's whole webhook contract IS an authenticated
//!   re-fetch, which needs `&self`.
//! - `refund()`: **NOT a cackle port** (cackle's `Provider` interface has no
//!   `Refund` method at all; `Capabilities.Refunds: true` is descriptive
//!   metadata only). New code grounded in Mollie's own public Create Refund
//!   API (<https://docs.mollie.com/reference/create-refund>, `POST
//!   /v2/payments/{id}/refunds`), which DOES document a `status` field
//!   (`pending`/`processing`/`refunded`/`failed`) this port can honestly map
//!   — only `"refunded"` is ever treated as money having actually moved
//!   back, same fail-closed convention as every ported `verify()` in this
//!   crate.

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::mollie::config::{MollieConfig, MOLLIE_API_BASE};
use crate::mollie::models::{self, PaymentPayload};
use crate::mollie::proof::{ChargeProof, RefundProof};
use crate::mollie::webhook::{self, MollieWebhookEvent};

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Mirrors `stripe::rail`/`paystack::rail`/`checkoutcom::rail`'s identical
/// `safe_path_segment` helper.
fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for a mollie id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to Mollie's Payments API. See module docs for
/// the full `Provider` -> `PaymentRail` mapping.
pub struct MollieRail {
    id: String,
    config: MollieConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl MollieRail {
    /// Build a rail from configuration. Fails if `api_key` or
    /// `webhook_url` are empty — mirrors cackle's `NewMollie` requiring
    /// both.
    pub fn new(config: MollieConfig) -> Result<Self> {
        if config.api_key.trim().is_empty() {
            return Err(Error::InvalidRequest("api_key must not be empty".into()));
        }
        if config.webhook_url.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_url must not be empty".into(),
            ));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building mollie http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Mollie (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "mollie".to_string(),
            config,
            http,
            capabilities,
            base_url: MOLLIE_API_BASE.to_string(),
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
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("mollie: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("mollie: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("mollie: {e}")))?;
        Ok((bytes.to_vec(), status))
    }

    /// Shared by [`PaymentRail::verify`] and [`Self::handle_webhook`]: fetch
    /// `GET /v2/payments/{id}` and evaluate it — mirrors cackle's `Verify`
    /// (and, since cackle's `Webhook` simply calls `Verify`, its `Webhook`
    /// too).
    async fn fetch_outcome(&self, payment_id: &str) -> Result<models::PaymentOutcome> {
        let payment_id = safe_path_segment(payment_id)?;
        let path = format!("/payments/{payment_id}");
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let payload: PaymentPayload =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        models::evaluate_payment(&payload)
    }

    /// Handle Mollie's classic webhook delivery — see module docs and
    /// `webhook.rs`'s module docs for why this needs `&self` (an
    /// authenticated re-fetch), unlike every other adapter's pure-function
    /// webhook handling.
    pub async fn handle_webhook(&self, raw_body: &[u8]) -> Result<MollieWebhookEvent> {
        let payment_id = webhook::extract_payment_id(raw_body)
            .map_err(|e| Error::InvalidRequest(e.to_string()))?;
        let outcome = self.fetch_outcome(&payment_id).await?;
        Ok(MollieWebhookEvent {
            event_id: payment_id,
            reference: outcome.reference,
            settled: outcome.settled,
            amount_minor: outcome.amount_minor,
            currency: outcome.currency,
        })
    }
}

#[async_trait]
impl PaymentRail for MollieRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::redirect_url`], because on the `mollie` rail
    /// `destination` is not a payout address: it is the post-checkout return
    /// URL, sent as Mollie's `redirectUrl` (see this module's docs above).
    ///
    /// So the honest ceiling here is
    /// [`patala_core::DestinationStatus::Unknown`], never
    /// `StructurallyValid` — that status means "a well-formed address for the
    /// network this rail pays on", and claiming it would tell a caller a
    /// redirect URL had been vetted as somewhere to send a customer's money.
    /// What *is* decided offline: a string that is not an absolute http(s)
    /// URL is refused (the processor documents this field as one), and a
    /// blockchain address or a private key pasted here is refused **by name**.
    ///
    /// Giving a customer their money back on this `CustodialReversible` rail
    /// is [`PaymentRail::refund`] — back the way it came, no destination
    /// involved — not a charge to a customer-supplied address.
    fn validate_destination(&self, dest: &str) -> patala_core::DestinationVerdict {
        crate::destination::redirect_url(self.id(), dest)
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        models::mollie_amount_value(req.amount_minor, &req.currency)?;

        // NEEDS-CONFIRMATION (mirrors stripe/paystack's identical note):
        // Mollie's documented API has no pre-charge fee-quote endpoint, and
        // cackle's own adapter has no Quote-equivalent method.
        Ok(Quote {
            rail_id: self.id.clone(),
            amount_minor: req.amount_minor,
            currency: req.currency.clone(),
            fee_minor: 0,
            total_minor: req.amount_minor,
            settlement: self.capabilities.settlement,
            expires_at_unix: now_unix().saturating_add(300),
        })
    }

    async fn charge(&self, req: &PayRequest) -> Result<Receipt> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        let currency = req.currency.trim().to_ascii_uppercase();
        let value = models::mollie_amount_value(req.amount_minor, &currency)?;

        // See module docs: `destination` is reinterpreted as the
        // redirectUrl Mollie requires (cackle's `Order.CallbackURL`).
        let body = serde_json::json!({
            "amount": {"currency": currency, "value": value},
            "description": format!("patala order {}", req.reference),
            "redirectUrl": req.destination,
            "webhookUrl": self.config.webhook_url,
            "metadata": {"patala_reference": req.reference},
        });

        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/payments", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(serde::Deserialize)]
        struct Links {
            checkout: Checkout,
        }
        #[derive(serde::Deserialize)]
        struct Checkout {
            href: String,
        }
        #[derive(serde::Deserialize)]
        struct CreateResponse {
            id: String,
            #[serde(rename = "_links")]
            links: Links,
        }
        let parsed: CreateResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.id.is_empty() || parsed.links.checkout.href.is_empty() {
            return Err(models::malformed(
                "empty payment id or _links.checkout.href",
            ));
        }

        let proof = ChargeProof {
            payment_id: parsed.id,
            checkout_url: Some(parsed.links.checkout.href),
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see module docs
            currency,
            reference: req.reference.clone(), // see proof.rs module docs
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
        let Ok(outcome) = self.fetch_outcome(&proof.payment_id).await else {
            // fetch_outcome only returns Err for a genuine operational
            // failure (HTTP/parse) -- but a malformed/tampered proof
            // reaching here (e.g. an empty payment_id) is caught by
            // safe_path_segment inside fetch_outcome as InvalidRequest,
            // which we still fail closed on rather than propagate.
            return Ok(false);
        };

        if !outcome.settled {
            return Ok(false);
        }
        if !outcome.currency.eq_ignore_ascii_case(&receipt.currency) {
            return Ok(false);
        }
        if outcome.amount_minor < receipt.amount_minor {
            return Ok(false);
        }
        Ok(true)
    }

    async fn refund(&self, receipt: &Receipt) -> Result<Receipt> {
        // See module docs: new code grounded in Mollie's public Create
        // Refund API, not a cackle port.
        if receipt.rail_id != self.id {
            return Err(Error::InvalidRequest(format!(
                "receipt names rail {:?}, not {:?}",
                receipt.rail_id, self.id
            )));
        }
        let proof = ChargeProof::from_bytes(&receipt.proof).ok_or_else(|| {
            Error::InvalidRequest("receipt proof is not a mollie charge proof".into())
        })?;
        let payment_id = safe_path_segment(&proof.payment_id)?;

        let currency = receipt.currency.trim().to_ascii_uppercase();
        let value = models::mollie_amount_value(receipt.amount_minor, &currency)?;
        let body = serde_json::json!({
            "amount": {"currency": currency, "value": value},
        });

        let path = format!("/payments/{payment_id}/refunds");
        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, &path, Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(serde::Deserialize, Default)]
        struct RefundResponse {
            #[serde(default)]
            id: String,
            #[serde(default)]
            status: String,
            #[serde(default)]
            amount: models::AmountObj,
        }
        let parsed: RefundResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;

        // Mollie's documented refund status values: "pending", "processing",
        // "refunded", "failed" -- only "refunded" is ever reported as money
        // having actually moved back, same fail-closed convention as every
        // other rail in this crate.
        let succeeded = parsed.status == "refunded";
        let amount_minor = if succeeded {
            models::mollie_amount_value_to_minor(&parsed.amount.value, &currency)?
        } else {
            0
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor,
            currency,
            reference: receipt.reference.clone(),
            proof: RefundProof {
                refund_id: parsed.id,
                status_at_refund: parsed.status,
            }
            .to_bytes(),
            settled_at_unix: now_unix(),
        })
    }

    /// Handle a Mollie webhook delivery — delegates to
    /// [`Self::handle_webhook`], which performs the authenticated re-fetch
    /// Mollie's design requires.
    ///
    /// Mollie's callback carries no signature and no payload beyond a
    /// payment id (by design — Mollie's own docs say so), so "verification"
    /// here IS the re-fetch: nothing in the delivery is trusted except which
    /// payment to ask Mollie about, and the settlement reported is Mollie's
    /// own answer to that authenticated query.
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event = self.handle_webhook(&delivery.raw_body).await?;
        let payment_id = event.event_id.clone();
        Ok(WebhookEvent::settlement(
            &self.id,
            event.event_id,
            event.reference,
            event.settled,
            event.amount_minor,
            event.currency,
        )
        .with_object_id(payment_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(amount: u64, currency: &str, destination: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: destination.into(),
            reference: reference.into(),
        }
    }

    fn config() -> MollieConfig {
        MollieConfig {
            api_key: "test_apikey".to_string(),
            webhook_url: "https://example.com/webhooks/mollie".to_string(),
            requires_kyc: true,
            currencies: Vec::new(),
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> MollieRail {
        let mut rail = MollieRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/mollie_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(rail.id(), "mollie");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.api_key.clear();
        assert!(MollieRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.webhook_url.clear();
        assert!(MollieRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "tr_test1",
                "_links": {"checkout": {"href": "https://www.mollie.com/checkout/tr_test1"}}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(5000, "EUR", "https://example.com/return", "ord_1"))
            .await
            .unwrap();
        assert_eq!(receipt.reference, "ord_1");
        assert_eq!(
            receipt.amount_minor, 0,
            "nothing has settled yet at charge time"
        );
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.payment_id, "tr_test1");
    }

    #[tokio::test]
    async fn charge_refuses_zero_decimal_currency_without_calling_server() {
        let server = MockServer::start().await;
        // No Mock registered -- if the adapter called the server anyway
        // wiremock would panic on an unexpected request.
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(1000, "JPY", "https://example.com", "ord_1"))
            .await
            .expect_err("zero-decimal currency must be refused before any network call");
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn charge_http_500_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payments"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                json!({"status": 500, "title": "Internal Server Error", "detail": "boom"}),
            ))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(5000, "EUR", "https://example.com/return", "ord_1"))
            .await
            .unwrap_err();
        match err {
            Error::Rail(msg) => assert!(msg.contains("boom")),
            other => panic!("expected Error::Rail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_paid_payment_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/payments/tr_test1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "tr_test1",
                "status": "paid",
                "amount": {"currency": "EUR", "value": "50.00"},
                "metadata": {"patala_reference": "ord_1"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "mollie".into(),
            amount_minor: 0,
            currency: "EUR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_id: "tr_test1".into(),
                checkout_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_open_payment_is_not_settled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/payments/tr_test1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "tr_test1",
                "status": "open",
                "amount": {"currency": "EUR", "value": "50.00"},
                "metadata": {"patala_reference": "ord_1"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "mollie".into(),
            amount_minor: 0,
            currency: "EUR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_id: "tr_test1".into(),
                checkout_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_fails_closed_on_amount_or_currency_mismatch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/payments/tr_test1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "tr_test1",
                "status": "paid",
                "amount": {"currency": "EUR", "value": "5.00"},
                "metadata": {"patala_reference": "ord_1"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let genuine = Receipt {
            rail_id: "mollie".into(),
            amount_minor: 500,
            currency: "EUR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_id: "tr_test1".into(),
                checkout_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&genuine).await.unwrap());

        let mut inflated = genuine.clone();
        inflated.amount_minor = 999_999;
        assert!(!rail.verify(&inflated).await.unwrap());

        let mut wrong_currency = genuine.clone();
        wrong_currency.currency = "USD".into();
        assert!(!rail.verify(&wrong_currency).await.unwrap());
    }

    #[tokio::test]
    async fn handle_webhook_forged_id_never_accepts_without_real_payment() {
        // A "forged" webhook call (attacker-supplied id) can, at most, make
        // this adapter go check that id's real status -- it can never
        // fabricate a paid result, because handle_webhook defers entirely to
        // the authenticated API lookup.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/payments/tr_forged"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "tr_forged",
                "status": "open",
                "amount": {"currency": "EUR", "value": "999999.00"},
                "metadata": {"patala_reference": "ord_1"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let event = rail.handle_webhook(b"id=tr_forged").await.unwrap();
        assert!(
            !event.settled,
            "a payment the API reports as open must not be settled"
        );
    }

    #[tokio::test]
    async fn handle_webhook_missing_id_fails_closed() {
        let rail = rail_for("http://127.0.0.1:1".into());
        assert!(rail.handle_webhook(b"").await.is_err());
    }

    #[tokio::test]
    async fn handle_webhook_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/payments/tr_test1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "tr_test1",
                "status": "paid",
                "amount": {"currency": "EUR", "value": "50.00"},
                "metadata": {"patala_reference": "ord_1"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let event = rail.handle_webhook(b"id=tr_test1").await.unwrap();
        assert!(event.settled);
        assert_eq!(event.amount_minor, 5000);
        assert_eq!(event.currency, "EUR");
        assert_eq!(event.reference, "ord_1");
    }

    #[tokio::test]
    async fn refund_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payments/tr_test1/refunds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "re_1",
                "status": "refunded",
                "amount": {"currency": "EUR", "value": "50.00"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let original = Receipt {
            rail_id: "mollie".into(),
            amount_minor: 5000,
            currency: "EUR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_id: "tr_test1".into(),
                checkout_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let refund_receipt = rail.refund(&original).await.unwrap();
        assert_eq!(refund_receipt.amount_minor, 5000);
    }

    #[tokio::test]
    async fn refund_pending_is_not_yet_moved() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payments/tr_test1/refunds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "re_1",
                "status": "pending",
                "amount": {"currency": "EUR", "value": "50.00"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let original = Receipt {
            rail_id: "mollie".into(),
            amount_minor: 5000,
            currency: "EUR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_id: "tr_test1".into(),
                checkout_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let refund_receipt = rail.refund(&original).await.unwrap();
        assert_eq!(
            refund_receipt.amount_minor, 0,
            "a pending refund has not moved money back yet"
        );
    }

    #[tokio::test]
    async fn refund_rejects_a_receipt_from_a_different_rail() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let foreign = Receipt {
            rail_id: "stripe".into(),
            amount_minor: 100,
            currency: "EUR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.refund(&foreign).await.is_err());
    }
}
