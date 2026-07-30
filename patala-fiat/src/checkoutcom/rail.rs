//! [`CheckoutComRail`] — the `PaymentRail` implementation. Ported from
//! cackle's `internal/payments/checkoutcom.go` (`CheckoutComProvider`).
//!
//! Built against Checkout.com's DOCUMENTED public API (Hosted Payments
//! Page) — see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE"
//! disclosure. **HONESTY note, ported from cackle's own file doc comment**:
//! Checkout.com's interactive API reference for `POST /hosted-payments` is
//! JS-rendered and could not be fetched field-by-field by cackle's own
//! author. The request fields below (`amount`, `currency`, `reference`,
//! `success_url`, `failure_url`, `cancel_url`) are Checkout.com's standard,
//! well-documented naming convention used elsewhere in their Payments API,
//! but were not independently confirmed against the Hosted Payments Page
//! endpoint's own schema specifically. The response shape (`id`,
//! `_links.redirect.href`) WAS confirmed directly. `Verify` and `Webhook`
//! were confirmed against real documented examples and are higher
//! confidence than `Begin`'s exact request field names.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a Hosted Payments Page session, returns its
//!   redirect URL) maps to [`PaymentRail::charge`]. **Gap vs cackle**:
//!   `patala_core::PayRequest` has no callback-url field — this port
//!   reinterprets `PayRequest::destination` AS the `success_url`/
//!   `failure_url`/`cancel_url` Checkout.com requires, exactly the same
//!   reinterpretation `stripe::rail::StripeRail` applies to the same field.
//!   Callers of `CheckoutComRail::charge` must pass the desired post-payment
//!   return URL as `destination`.
//! - cackle's `Verify(reference)` has the SAME open ambiguity `stripe.go`'s
//!   own `Verify` admits to (see `proof.rs`'s module docs): `reference` here
//!   is treated as a Checkout.com payment id (`pay_...`), not
//!   `patala_core::PayRequest::reference`, because Checkout.com's API has no
//!   documented "look up a payment by an arbitrary merchant reference" GET
//!   endpoint. This port sidesteps the ambiguity the same way `stripe::rail`
//!   does: the real Checkout.com payment id lives in `proof`, and `verify()`
//!   always looks it up from there.
//! - cackle's `Webhook` maps to [`PaymentRail::verify_webhook`], which
//!   delegates to the free function
//!   [`crate::checkoutcom::webhook::verify_and_parse`]. The function keeps the
//!   pure, directly-testable shape; the trait method is what a consumer
//!   dispatching through `dyn PaymentRail` — the UniFFI binding, the
//!   sidecar — can actually reach.
//! - `refund()`: **NOT a cackle port** (cackle's `Provider` interface has no
//!   `Refund` method at all; `Capabilities.Refunds: true` is descriptive
//!   metadata only). New code grounded in Checkout.com's own public Refund
//!   API (<https://checkout.com/docs/payments/manage-payments/refund-a-payment>,
//!   endpoint `POST /payments/{id}/refunds`). Checkout.com's own documented
//!   response to that call is `202 Accepted` with only `{action_id,
//!   reference}` — no synchronous completion/status field this adapter
//!   could use to honestly report a settled amount without further
//!   confirmation (a `payment_refunded` webhook would confirm it, but
//!   cackle's own `Webhook` only ever handles `payment_captured` — see
//!   above — so this port does not build one either). Rather than fabricate
//!   a completion signal Checkout.com's own create-refund response does not
//!   provide, this method ALWAYS returns `Receipt { amount_minor: 0, .. }`,
//!   honestly reporting "refund initiated, not yet confirmed moved".

use async_trait::async_trait;
use serde::Deserialize;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::checkoutcom::config::CheckoutComConfig;
use crate::checkoutcom::models::{self, PaymentPayload};
use crate::checkoutcom::proof::{ChargeProof, RefundProof};

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Mirrors `stripe::rail`/`paystack::rail`'s identical `safe_path_segment`
/// helper.
fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for a checkoutcom id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to Checkout.com's Hosted Payments Page. See
/// module docs for the full `Provider` -> `PaymentRail` mapping.
pub struct CheckoutComRail {
    id: String,
    config: CheckoutComConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl CheckoutComRail {
    /// Build a rail from configuration. Fails if `secret_key`,
    /// `webhook_secret`, or `api_base_url` are empty — mirrors cackle's
    /// `NewCheckoutCom` refusing a half-configured adapter.
    pub fn new(config: CheckoutComConfig) -> Result<Self> {
        if config.secret_key.trim().is_empty() {
            return Err(Error::InvalidRequest("secret_key must not be empty".into()));
        }
        if config.webhook_secret.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_secret must not be empty".into(),
            ));
        }
        if config.api_base_url.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "api_base_url must not be empty".into(),
            ));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building checkoutcom http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Checkout.com (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
        };

        let base_url = config.api_base_url.trim_end_matches('/').to_string();
        Ok(Self {
            id: "checkoutcom".to_string(),
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
            .header(
                "Authorization",
                format!("Bearer {}", self.config.secret_key),
            )
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("checkoutcom: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("checkoutcom: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("checkoutcom: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for CheckoutComRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::redirect_url`], because on the `checkoutcom` rail
    /// `destination` is not a payout address: it is the post-checkout return
    /// URL, sent as Checkout.com's `success_url`/`failure_url`/`cancel_url` (see this module's docs above).
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
        models::checkout_com_amount(req.amount_minor, &req.currency)?;

        // NEEDS-CONFIRMATION (mirrors stripe/paystack's identical note):
        // Checkout.com's documented API has no pre-charge fee-quote
        // endpoint, and cackle's own adapter has no Quote-equivalent method.
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
        let amount = models::checkout_com_amount(req.amount_minor, &currency)?;

        // See module docs: `destination` is reinterpreted as the
        // success_url/failure_url/cancel_url Checkout.com requires.
        let body = serde_json::json!({
            "amount": amount,
            "currency": currency,
            "reference": req.reference,
            "success_url": req.destination,
            "failure_url": req.destination,
            "cancel_url": req.destination,
            "metadata": {"patala_reference": req.reference},
        });

        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/hosted-payments", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(Deserialize)]
        struct Links {
            redirect: Redirect,
        }
        #[derive(Deserialize)]
        struct Redirect {
            href: String,
        }
        #[derive(Deserialize)]
        struct CreateResponse {
            id: String,
            #[serde(rename = "_links")]
            links: Links,
        }
        let parsed: CreateResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.id.is_empty() || parsed.links.redirect.href.is_empty() {
            return Err(models::malformed(
                "empty session id or _links.redirect.href",
            ));
        }

        let proof = ChargeProof {
            payment_id: parsed.id,
            // Checkout.com's Hosted Payments Page create response (as
            // cackle's own Begin decodes it) has no `status` field --
            // "pending" is a neutral placeholder verify() never trusts,
            // always re-fetching the true status.
            status_at_charge: "pending".to_string(),
            redirect_url: Some(parsed.links.redirect.href),
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see module docs
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
        let Ok(payment_id) = safe_path_segment(&proof.payment_id) else {
            return Ok(false);
        };

        let path = format!("/payments/{payment_id}");
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let payload: PaymentPayload =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        let Ok(outcome) = models::evaluate_payment(&payload) else {
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
        // See module docs: new code grounded in Checkout.com's public
        // Refund API, not a cackle port.
        if receipt.rail_id != self.id {
            return Err(Error::InvalidRequest(format!(
                "receipt names rail {:?}, not {:?}",
                receipt.rail_id, self.id
            )));
        }
        let proof = ChargeProof::from_bytes(&receipt.proof).ok_or_else(|| {
            Error::InvalidRequest("receipt proof is not a checkoutcom charge proof".into())
        })?;
        let payment_id = safe_path_segment(&proof.payment_id)?;

        let currency = receipt.currency.trim().to_ascii_uppercase();
        let amount = models::checkout_com_amount(receipt.amount_minor, &currency)?;
        let body = serde_json::json!({
            "amount": amount,
            "reference": receipt.reference,
        });

        let path = format!("/payments/{payment_id}/refunds");
        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, &path, Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(Deserialize, Default)]
        struct RefundResponse {
            #[serde(default)]
            action_id: String,
        }
        let parsed: RefundResponse = serde_json::from_slice(&resp_body).unwrap_or_default();

        // Checkout.com's refund creation response never confirms completion
        // synchronously (202 Accepted, {action_id, reference} only) -- see
        // module docs. Always report as still-pending.
        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0,
            currency,
            reference: receipt.reference.clone(),
            proof: RefundProof {
                action_id: parsed.action_id,
            }
            .to_bytes(),
            settled_at_unix: 0,
        })
    }

    /// Verify a Checkout.com webhook delivery (HMAC-SHA256 over the raw
    /// body, header `Cko-Signature`) — see
    /// [`crate::checkoutcom::webhook::verify_and_parse`].
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event = crate::checkoutcom::webhook::verify_and_parse(
            &self.config.webhook_secret,
            &delivery.raw_body,
            delivery.header_or_empty("Cko-Signature"),
        )
        .map_err(|e| Error::InvalidRequest(e.to_string()))?;
        Ok(WebhookEvent::settlement(
            &self.id,
            event.event_id,
            event.reference,
            event.settled,
            event.amount_minor,
            event.currency,
        ))
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

    fn config() -> CheckoutComConfig {
        CheckoutComConfig {
            secret_key: "sk_test_fake".to_string(),
            webhook_secret: "cko_test_webhook_secret".to_string(),
            api_base_url: "http://127.0.0.1:1".to_string(),
            requires_kyc: true,
            currencies: Vec::new(),
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> CheckoutComRail {
        let mut rail = CheckoutComRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/checkoutcom_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(rail.id(), "checkoutcom");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.secret_key.clear();
        assert!(CheckoutComRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.webhook_secret.clear();
        assert!(CheckoutComRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.api_base_url.clear();
        assert!(CheckoutComRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_posts_expected_shape() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hosted-payments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "hp_test_1",
                "_links": {"redirect": {"href": "https://pay.checkout.com/hp_test_1"}}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(5000, "USD", "https://example.com/return", "ord_1"))
            .await
            .unwrap();
        assert_eq!(receipt.reference, "ord_1");
        assert_eq!(
            receipt.amount_minor, 0,
            "nothing has settled yet at charge time"
        );
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.payment_id, "hp_test_1");
        assert_eq!(
            proof.redirect_url.as_deref(),
            Some("https://pay.checkout.com/hp_test_1")
        );
    }

    #[tokio::test]
    async fn charge_http_500_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hosted-payments"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                json!({"error_type": "processing_error", "error_codes": ["internal_error"]}),
            ))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(5000, "USD", "https://example.com/return", "ord_1"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    #[tokio::test]
    async fn verify_settled_payment_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/payments/pay_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "pay_1",
                "status": "Captured",
                "amount": 5000,
                "currency": "USD",
                "reference": "ord_1"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "checkoutcom".into(),
            amount_minor: 0,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_id: "pay_1".into(),
                status_at_charge: "pending".into(),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_declined_payment_is_not_settled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/payments/pay_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "pay_1",
                "status": "Declined",
                "amount": 5000,
                "currency": "USD",
                "reference": "ord_1"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "checkoutcom".into(),
            amount_minor: 0,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_id: "pay_1".into(),
                status_at_charge: "pending".into(),
                redirect_url: None,
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
            .and(path("/payments/pay_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "pay_1",
                "status": "Captured",
                "amount": 500,
                "currency": "USD",
                "reference": "ord_1"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let base_proof = ChargeProof {
            payment_id: "pay_1".into(),
            status_at_charge: "pending".into(),
            redirect_url: None,
        }
        .to_bytes();

        let genuine = Receipt {
            rail_id: "checkoutcom".into(),
            amount_minor: 500,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: base_proof.clone(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&genuine).await.unwrap());

        let mut inflated = genuine.clone();
        inflated.amount_minor = 999_999;
        assert!(!rail.verify(&inflated).await.unwrap());

        let mut wrong_currency = genuine.clone();
        wrong_currency.currency = "EUR".into();
        assert!(!rail.verify(&wrong_currency).await.unwrap());

        let mut wrong_rail = genuine.clone();
        wrong_rail.rail_id = "some-other-rail".into();
        assert!(!rail.verify(&wrong_rail).await.unwrap());

        let mut garbage = genuine.clone();
        garbage.proof = vec![1, 2, 3];
        assert!(!rail.verify(&garbage).await.unwrap());
    }

    #[tokio::test]
    async fn refund_posts_expected_shape_and_is_always_pending() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payments/pay_1/refunds"))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({
                "action_id": "act_1",
                "reference": "ord_1"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let original = Receipt {
            rail_id: "checkoutcom".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_id: "pay_1".into(),
                status_at_charge: "pending".into(),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let refund_receipt = rail.refund(&original).await.unwrap();
        assert_eq!(
            refund_receipt.amount_minor, 0,
            "checkoutcom refunds never confirm completion synchronously"
        );
        let proof = RefundProof::from_bytes(&refund_receipt.proof).unwrap();
        assert_eq!(proof.action_id, "act_1");
    }

    #[tokio::test]
    async fn refund_rejects_a_receipt_from_a_different_rail() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let foreign = Receipt {
            rail_id: "manual".into(),
            amount_minor: 100,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.refund(&foreign).await.is_err());
    }
}
