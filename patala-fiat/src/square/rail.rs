//! [`SquareRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/square.go` (`SquareProvider`).
//!
//! Built against Square's DOCUMENTED public API, per cackle's own file
//! header ("verified live against developer.squareup.com" by cackle's
//! author, though no sandbox/live account was used from THIS environment —
//! see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE" note).
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a Square Payment Link, returns its hosted
//!   checkout URL) maps to [`PaymentRail::charge`]. **`destination`
//!   reinterpretation, mirrors `stripe::rail`'s identical resolution for
//!   the identical need** (cited directly): Square's Payment Links API
//!   requires `checkout_options.redirect_url` (cackle:
//!   `"payments: square: callback_url is required"`) — reinterpret
//!   `PayRequest::destination` AS that redirect URL. Unlike Stripe (which
//!   needs a success_url/cancel_url PAIR), Square has only ONE redirect_url
//!   field, so this is the simpler single-URL case of the same problem.
//! - cackle's `Order.EventID`/`BuyerEmail`/`BuyerName` have no `PayRequest`
//!   equivalents and are never sent — documented info-loss (Square's
//!   Payment Links API doesn't require any of them anyway).
//! - **THE key structural gap, documented loudly (see `proof.rs`'s module
//!   docs, and cackle's own file-header HONESTY note 3)**: Square's Payment
//!   id — the only thing its Payments API can be queried by — is not known
//!   until the buyer actually pays, delivered only via a `payment.updated`
//!   webhook; cackle's own comment states there is no confirmed "look up a
//!   payment by our own reference_id" endpoint independent of the Order.
//!   So [`ChargeProof::payment_id`] is ALWAYS `None` right after
//!   [`SquareRail::charge`] returns, and [`PaymentRail::verify`] on that
//!   fresh receipt honestly returns `Ok(false)` — not because a check
//!   failed, but because there is structurally nothing yet to check. The
//!   ONLY way `verify()` can ever return `Ok(true)` for this rail: the
//!   caller receives Square's `payment.updated` webhook (via
//!   [`crate::square::webhook::verify_and_parse`], which resolves the real
//!   payment id), constructs a NEW `Receipt` with the SAME
//!   `reference`/`amount_minor`/`currency` as the original but with `proof`
//!   re-serialized via
//!   [`crate::square::proof::ChargeProof::with_resolved_payment_id`], and
//!   calls `verify()` again — which then re-fetches the payment directly
//!   from Square and fail-closed-checks it exactly like every other rail.
//! - cackle's `Verify(reference)` (a Square PAYMENT id, per the gap above —
//!   NOT Cackle's own order reference, same caveat cackle documents for
//!   `stripe.go`/`checkoutcom.go`'s `Verify`) maps to [`PaymentRail::verify`]
//!   once `proof.payment_id` is resolved, as described above.
//! - cackle's `Webhook` is ported as the free function
//!   [`crate::square::webhook::verify_and_parse`], NOT a trait method —
//!   same reasoning as every other adapter here.
//! - `refund()`: **THIS adapter gets a real implementation**, unlike
//!   payu/razorpay/xendit in this crate — cackle's
//!   `SquareProvider.Capabilities().Refunds: true` is a genuine
//!   asserted-support signal (not just descriptive metadata with no hint,
//!   the way payu/razorpay/xendit's plain `false` is). This method is NEW
//!   code (cackle's `Provider` interface never had a `Refund` method at
//!   all), grounded directly in Square's own public Refunds API
//!   (<https://developer.squareup.com/reference/square/refunds-api/refund-payment>),
//!   same honesty conventions as every ported method here. Requires
//!   `proof.payment_id` to already be resolved — you cannot refund a
//!   payment whose id was never learned, a direct consequence of the gap
//!   above.

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result, Settlement,
};

use crate::square::config::SquareConfig;
use crate::square::models::{self, SquarePaymentPayload};
use crate::square::proof::ChargeProof;

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
            "value {s:?} is not a safe URL path segment for a square id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to Square's Payment Links / Payments / Refunds
/// API. See module docs for the full `Provider` -> `PaymentRail` mapping
/// and the payment-id structural gap.
pub struct SquareRail {
    id: String,
    config: SquareConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl SquareRail {
    /// Build a rail from configuration. Fails if any of the five required
    /// config fields are empty.
    pub fn new(config: SquareConfig) -> Result<Self> {
        if config.access_token.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "access_token must not be empty".into(),
            ));
        }
        if config.webhook_signature_key.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_signature_key must not be empty".into(),
            ));
        }
        if config.location_id.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "location_id must not be empty".into(),
            ));
        }
        if config.notification_url.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "notification_url must not be empty".into(),
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
            .map_err(|e| Error::Rail(format!("failed building square http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true, // mirrors cackle's Capabilities.Refunds: true
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Square (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
        };

        let base_url = config.api_base_url.clone();
        Ok(Self {
            id: "square".to_string(),
            config,
            http,
            capabilities,
            base_url,
        })
    }

    /// Mirrors cackle's `Capabilities.SupportsCurrency`: an empty
    /// `currencies` config means unrestricted, matching cackle's own
    /// `Currencies: nil`.
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
                format!("Bearer {}", self.config.access_token),
            )
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("square: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("square: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("square: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for SquareRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        models::square_amount(req.amount_minor, &req.currency)?;

        // NEEDS-CONFIRMATION (mirrors every other rail's identical note):
        // Square's documented API has no pre-charge fee-quote endpoint,
        // and cackle's own adapter has no Quote-equivalent method either.
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
        let amount = models::square_amount(req.amount_minor, &currency)?;

        // See module docs: `destination` is reinterpreted as Square's
        // single checkout_options.redirect_url.
        let redirect_url = req.destination.trim();
        if redirect_url.is_empty() {
            return Err(Error::InvalidRequest(
                "square: destination (used as the redirect_url) is required".into(),
            ));
        }

        let item_name = format!("patala order {}", req.reference);
        let body = serde_json::json!({
            "idempotency_key": req.reference,
            "order": {
                "location_id": self.config.location_id,
                "reference_id": req.reference,
                "line_items": [{
                    "name": item_name,
                    "quantity": "1",
                    "base_price_money": {"amount": amount, "currency": currency},
                }],
            },
            "checkout_options": {"redirect_url": redirect_url},
        });

        let (resp_body, status) = self
            .do_json(
                reqwest::Method::POST,
                "/v2/online-checkout/payment-links",
                Some(&body),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(serde::Deserialize, Default)]
        struct PaymentLink {
            #[serde(default)]
            id: String,
            #[serde(default)]
            url: String,
            #[serde(default)]
            order_id: String,
        }
        #[derive(serde::Deserialize, Default)]
        struct CreateResponse {
            #[serde(default)]
            payment_link: PaymentLink,
        }
        let parsed: CreateResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.payment_link.id.is_empty() || parsed.payment_link.url.is_empty() {
            return Err(models::malformed("empty payment link id or url"));
        }

        let proof = ChargeProof {
            order_id: parsed.payment_link.order_id,
            payment_link_id: parsed.payment_link.id,
            payment_id: None, // not known until the buyer pays -- see module docs
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet
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
        // See module docs: no payment_id yet means nothing to check yet --
        // an honest Ok(false), not an error, and no network call happens.
        let Some(payment_id) = proof.payment_id else {
            return Ok(false);
        };
        let Ok(payment_id) = safe_path_segment(&payment_id) else {
            return Ok(false);
        };

        let path = format!("/v2/payments/{payment_id}");
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }

        #[derive(serde::Deserialize, Default)]
        struct VerifyResponse {
            #[serde(default)]
            payment: SquarePaymentPayload,
        }
        let parsed: VerifyResponse =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        let Ok(outcome) = models::parse_square_payment(&parsed.payment) else {
            return Ok(false);
        };

        if outcome.reference != receipt.reference {
            return Ok(false);
        }
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

    /// **NEW code, not a cackle port** — see module docs. Grounded in
    /// Square's public Refunds API
    /// (<https://developer.squareup.com/reference/square/refunds-api/refund-payment>):
    /// `POST /v2/refunds` with `{idempotency_key, amount_money, payment_id}`,
    /// response `{refund: {id, status, amount_money}}`. Documented status
    /// values: `PENDING`, `COMPLETED`, `REJECTED`, `FAILED` — only
    /// `COMPLETED` is ever reported as money having actually moved back.
    async fn refund(&self, receipt: &Receipt) -> Result<Receipt> {
        if receipt.rail_id != self.id {
            return Err(Error::InvalidRequest(format!(
                "receipt names rail {:?}, not {:?}",
                receipt.rail_id, self.id
            )));
        }
        let proof = ChargeProof::from_bytes(&receipt.proof).ok_or_else(|| {
            Error::InvalidRequest("receipt proof is not a square charge proof".into())
        })?;
        let payment_id = proof.payment_id.ok_or_else(|| {
            Error::InvalidRequest(
                "square: receipt has no resolved payment_id -- cannot refund a payment whose id \
                 was never learned (see proof.rs's module docs)"
                    .into(),
            )
        })?;
        let payment_id = safe_path_segment(&payment_id)?;

        let body = serde_json::json!({
            "idempotency_key": format!("refund:{}", receipt.reference),
            "amount_money": {"amount": receipt.amount_minor, "currency": receipt.currency},
            "payment_id": payment_id,
        });
        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/v2/refunds", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(serde::Deserialize, Default)]
        struct RefundMoney {
            #[serde(default)]
            amount: u64,
            #[serde(default)]
            currency: String,
        }
        #[derive(serde::Deserialize, Default)]
        struct RefundEntity {
            #[serde(default)]
            id: String,
            #[serde(default)]
            status: String,
            #[serde(default)]
            amount_money: RefundMoney,
        }
        #[derive(serde::Deserialize, Default)]
        struct RefundResponse {
            #[serde(default)]
            refund: RefundEntity,
        }
        let parsed: RefundResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.refund.id.is_empty() {
            return Err(models::malformed("empty refund id"));
        }

        let succeeded = parsed.refund.status == "COMPLETED";
        let currency = parsed
            .refund
            .amount_money
            .currency
            .trim()
            .to_ascii_uppercase();

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: if succeeded {
                parsed.refund.amount_money.amount
            } else {
                0
            },
            currency: if currency.is_empty() {
                receipt.currency.clone()
            } else {
                currency
            },
            reference: receipt.reference.clone(),
            proof: Vec::new(),
            settled_at_unix: now_unix(),
        })
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

    fn config() -> SquareConfig {
        SquareConfig {
            access_token: "sq0atp-test".to_string(),
            webhook_signature_key: "square-test-webhook-signature-key".to_string(),
            location_id: "L123".to_string(),
            notification_url: "https://example.com/webhooks/square".to_string(),
            api_base_url: "https://connect.squareupsandbox.com".to_string(),
            requires_kyc: true,
            currencies: Vec::new(),
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> SquareRail {
        let mut rail = SquareRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/square_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.reversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(rail.id(), "square");
    }

    #[test]
    fn new_rejects_empty_config() {
        for clear in [
            "access_token",
            "webhook_signature_key",
            "location_id",
            "notification_url",
            "api_base_url",
        ] {
            let mut cfg = config();
            match clear {
                "access_token" => cfg.access_token.clear(),
                "webhook_signature_key" => cfg.webhook_signature_key.clear(),
                "location_id" => cfg.location_id.clear(),
                "notification_url" => cfg.notification_url.clear(),
                "api_base_url" => cfg.api_base_url.clear(),
                _ => unreachable!(),
            }
            assert!(SquareRail::new(cfg).is_err(), "{clear}");
        }
    }

    // TestSquareBegin_Success
    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/online-checkout/payment-links"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "payment_link": {"id": "PLINK1", "url": "https://square.link/u/PLINK1", "order_id": "ORDER1"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(5000, "USD", "https://example.com/return", "ord_1"))
            .await
            .unwrap();
        assert_eq!(receipt.reference, "ord_1");
        assert_eq!(receipt.amount_minor, 0);
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.order_id, "ORDER1");
        assert_eq!(proof.payment_link_id, "PLINK1");
        assert_eq!(proof.payment_id, None);
    }

    // TestSquareBegin_RefusesThreeDecimalCurrency
    #[tokio::test]
    async fn charge_refuses_three_decimal_currency_without_calling_server() {
        let server = MockServer::start().await;
        // No Mock registered -- an unexpected request would panic.
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(1000, "KWD", "https://example.com", "ord_1"))
            .await
            .expect_err("three-decimal currency must be refused before any network call");
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    // TestSquareBegin_HTTP500FailsClosed
    #[tokio::test]
    async fn charge_http_500_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/online-checkout/payment-links"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(json!({"errors":[{"category":"API_ERROR","code":"INTERNAL_SERVER_ERROR","detail":"boom"}]})),
            )
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(5000, "USD", "https://example.com", "ord_1"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    // TestSquareBegin_MalformedJSONFailsClosed
    #[tokio::test]
    async fn charge_malformed_json_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/online-checkout/payment-links"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(5000, "USD", "https://example.com", "ord_1"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    /// New test (not cackle-sourced): verify() on a fresh receipt (no
    /// resolved payment_id) must return Ok(false) WITHOUT making any
    /// network call -- see module docs' structural gap.
    #[tokio::test]
    async fn verify_without_resolved_payment_id_is_ok_false_without_calling_server() {
        let server = MockServer::start().await;
        // No Mock registered -- an unexpected request would panic.
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "square".into(),
            amount_minor: 0,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "ORDER1".into(),
                payment_link_id: "PLINK1".into(),
                payment_id: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    /// New test (not cackle-sourced): the full round-trip described in
    /// module docs and proof.rs -- charge() has no payment_id, the caller
    /// later learns one (e.g. from the webhook), resolves it into a new
    /// proof, and verify() then confirms directly against Square.
    #[tokio::test]
    async fn verify_succeeds_once_payment_id_is_resolved_via_round_trip() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/payments/pay_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "payment": {"id":"pay_1","status":"COMPLETED","reference_id":"ord_1","order_id":"ORDER1","amount_money":{"amount":5000,"currency":"USD"}}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let original_proof = ChargeProof {
            order_id: "ORDER1".into(),
            payment_link_id: "PLINK1".into(),
            payment_id: None,
        };
        let resolved_proof = original_proof.with_resolved_payment_id("pay_1".into());
        let receipt = Receipt {
            rail_id: "square".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: resolved_proof.to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    // TestSquareVerify_PendingIsNotPaid
    #[tokio::test]
    async fn verify_pending_is_not_paid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/payments/pay_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "payment": {"id":"pay_1","status":"PENDING","reference_id":"ord_1","order_id":"ORDER1","amount_money":{"amount":5000,"currency":"USD"}}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "square".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "ORDER1".into(),
                payment_link_id: "PLINK1".into(),
                payment_id: Some("pay_1".into()),
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    // TestSquareVerify_HTTP500FailsClosed
    #[tokio::test]
    async fn verify_http_500_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/payments/pay_1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "square".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "ORDER1".into(),
                payment_link_id: "PLINK1".into(),
                payment_id: Some("pay_1".into()),
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    // TestSquareVerify_MalformedJSONFailsClosed
    #[tokio::test]
    async fn verify_malformed_json_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/payments/pay_1"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "square".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "ORDER1".into(),
                payment_link_id: "PLINK1".into(),
                payment_id: Some("pay_1".into()),
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    #[tokio::test]
    async fn verify_fails_closed_on_reference_mismatch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/payments/pay_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "payment": {"id":"pay_1","status":"COMPLETED","reference_id":"some-other-order","order_id":"ORDER1","amount_money":{"amount":5000,"currency":"USD"}}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "square".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "ORDER1".into(),
                payment_link_id: "PLINK1".into(),
                payment_id: Some("pay_1".into()),
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn refund_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/refunds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "refund": {"id": "ref_1", "status": "COMPLETED", "amount_money": {"amount": 5000, "currency": "USD"}}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let original = Receipt {
            rail_id: "square".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "ORDER1".into(),
                payment_link_id: "PLINK1".into(),
                payment_id: Some("pay_1".into()),
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let refund_receipt = rail.refund(&original).await.unwrap();
        assert_eq!(refund_receipt.amount_minor, 5000);
        assert_eq!(refund_receipt.currency, "USD");
    }

    #[tokio::test]
    async fn refund_pending_is_not_yet_moved() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/refunds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "refund": {"id": "ref_1", "status": "PENDING", "amount_money": {"amount": 5000, "currency": "USD"}}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let original = Receipt {
            rail_id: "square".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "ORDER1".into(),
                payment_link_id: "PLINK1".into(),
                payment_id: Some("pay_1".into()),
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
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.refund(&foreign).await.is_err());
    }

    #[tokio::test]
    async fn refund_rejects_missing_payment_id() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "square".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "ORDER1".into(),
                payment_link_id: "PLINK1".into(),
                payment_id: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(matches!(
            rail.refund(&receipt).await.unwrap_err(),
            Error::InvalidRequest(_)
        ));
    }
}
