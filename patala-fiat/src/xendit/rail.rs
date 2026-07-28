//! [`XenditRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/xendit.go` (`XenditProvider`).
//!
//! Reference: <https://developers.xendit.co/api-reference/#create-invoice>
//! (Invoices API). Not re-verified live from this environment — see this
//! crate's `PORTING.md` "UNVERIFIED AGAINST LIVE" note; cackle's own
//! confidence note for this file is MEDIUM.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a Xendit invoice, returns its hosted
//!   checkout link) maps to [`PaymentRail::charge`]. **Gap vs cackle**
//!   (flagged in `PORTING.md` §3): cackle's `Order` carries BOTH an
//!   optional `BuyerEmail` (sent as `payer_email` if present) AND an
//!   optional `CallbackURL` (sent as `success_redirect_url` if present),
//!   but `patala_core::PayRequest` has only ONE extra opaque field
//!   (`destination`) to reinterpret as either. Since neither is
//!   *required* by Xendit's Begin (both are conditionally sent in
//!   cackle), this port reinterprets `destination` AS the optional
//!   `success_redirect_url` (the more consequential of the two for a
//!   hosted-checkout flow — the buyer needs somewhere to land after
//!   paying) and simply NEVER sends a `payer_email` at all — an
//!   acknowledged information-loss gap, not a functional regression
//!   (Xendit's own API tolerates its absence). Because
//!   `PayRequest::validate()` already requires `destination` non-empty
//!   (the uniform contract every rail in this crate shares),
//!   `success_redirect_url` ends up ALWAYS sent here — cackle's own `if
//!   o.CallbackURL != ""` conditional branch is structurally unreachable
//!   at this seam, mirroring `paystack::rail`'s identical observation
//!   about its own dead currency-fallback.
//! - cackle's `Order.Countries`/`Capabilities.Countries` has no
//!   `RailCapabilities` field to port to at all — dropped, see
//!   `config.rs`'s doc comment.
//! - cackle's `Verify(reference)` maps directly to [`PaymentRail::verify`].
//!   **Genuine divergence, not an inconsistency** (see `proof.rs`'s module
//!   docs): Xendit's own `external_id` IS the caller's own reference (no
//!   separate provider-assigned id the way a Stripe session id is), so
//!   `verify()` uses `Receipt::reference` directly rather than anything
//!   decoded from `proof`.
//! - **Fail-closed adaptations from cackle's `Err`-returning cases, per
//!   `PORTING.md` §6** (`patala_core::PaymentRail::verify` must return
//!   `Ok(false)` for "not settled", never `Err`, reserving `Err` for a
//!   genuine operational failure to even perform the check):
//!   - cackle's `ErrXenditInvoiceNotFound` (empty `invoices` array) ->
//!     `Ok(false)` here, not `Err`.
//!   - cackle's returned-`external_id`-mismatch error -> `Ok(false)` here
//!     (never trust a mismatched entry either way).
//!   - a validly-parsed-but-semantically-malformed invoice (via
//!     `models::invoice_to_outcome`'s own `Err` cases — missing
//!     `external_id`, non-positive amount while `"PAID"`, missing id) ->
//!     `Ok(false)` here too, mirroring `stripe::rail::verify`'s identical
//!     treatment of `models::evaluate_session`'s `Err` case (a
//!     validly-parsed-but-ambiguous payload is "cannot confirm settled",
//!     not an operational failure).
//!   - Only a non-2xx HTTP status or a body that doesn't even parse as
//!     the expected JSON shape is a genuine `Err(Error::Rail(...))`.
//! - cackle's `Webhook` maps to [`PaymentRail::verify_webhook`], which
//!   delegates to the free function
//!   [`crate::xendit::webhook::verify_and_parse`]. The function keeps the
//!   pure, directly-testable shape; the trait method is what a consumer
//!   dispatching through `dyn PaymentRail` — the UniFFI binding, the
//!   sidecar — can actually reach.
//! - `refund()`: leaves the trait default (`Err(Error::Unsupported(...))`).
//!   Cackle's `XenditProvider.Capabilities().Refunds` is `false` with NO
//!   revealing "supports it, not implemented here"-style comment (unlike
//!   Paystack's, which explicitly hinted at real support) — per
//!   `PORTING.md` §7's last bullet, this port does not fabricate a refund
//!   implementation it cannot ground in either cackle's own code or a
//!   confirmed Xendit refund API.
//! - **Not ported**: cackle's `Countries`/`Payouts` fields (out of scope,
//!   see `config.rs` and `PORTING.md` §4).

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::xendit::config::XenditConfig;
use crate::xendit::models::{self, XenditInvoice};
use crate::xendit::proof::ChargeProof;

const XENDIT_API_BASE: &str = "https://api.xendit.co";

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One `PaymentRail` talking to Xendit's Invoices API. See module docs for
/// the full `Provider` -> `PaymentRail` mapping.
pub struct XenditRail {
    id: String,
    config: XenditConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl XenditRail {
    /// Build a rail from configuration. Fails if `secret_key`,
    /// `webhook_token`, or `currencies` are empty.
    pub fn new(config: XenditConfig) -> Result<Self> {
        if config.secret_key.trim().is_empty() {
            return Err(Error::InvalidRequest("secret_key must not be empty".into()));
        }
        if config.webhook_token.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_token must not be empty".into(),
            ));
        }
        if config.currencies.is_empty() {
            return Err(Error::InvalidRequest("currencies must not be empty".into()));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building xendit http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: false, // mirrors cackle's Capabilities.Refunds: false
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Xendit (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "xendit".to_string(),
            config,
            http,
            capabilities,
            base_url: XENDIT_API_BASE.to_string(),
        })
    }

    fn check_currency(&self, currency: &str) -> Result<()> {
        if self
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
            // Mirrors cackle's `req.SetBasicAuth(p.secretKey, "")` -- an
            // EMPTY password, not an absent one.
            .basic_auth(&self.config.secret_key, Some(""))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("xendit: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("xendit: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("xendit: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for XenditRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        // Also confirm the amount is convertible to Xendit's decimal
        // major-unit wire format before quoting -- mirrors stripe's own
        // early-validation-in-quote pattern (never fabricates a fee it
        // cannot obtain; Xendit's documented API has no pre-charge
        // fee-quote endpoint, and cackle's own adapter has no
        // Quote-equivalent method either).
        crate::currency::minor_to_major_string(req.amount_minor, &req.currency)
            .map_err(|e| Error::InvalidRequest(format!("xendit: {e}")))?;
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
        let major_amount = crate::currency::minor_to_major_string(req.amount_minor, &currency)
            .map_err(|e| Error::InvalidRequest(format!("xendit: {e}")))?;

        // See module docs: `destination` is reinterpreted as the optional
        // success_redirect_url -- always non-empty here since
        // `PayRequest::validate()` already ran above, so this is always
        // sent (cackle's own `if o.CallbackURL != ""` branch is
        // structurally unreachable at this seam). `payer_email` is never
        // sent -- no second opaque field is available to carry it.
        let body = serde_json::json!({
            "external_id": req.reference,
            "amount": major_amount,
            "currency": currency,
            "success_redirect_url": req.destination,
        });

        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/v2/invoices", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        let inv: XenditInvoice =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if inv.invoice_url.is_empty() {
            return Err(models::malformed("empty invoice_url"));
        }

        let proof = ChargeProof {
            invoice_url: inv.invoice_url,
            invoice_id: inv.id,
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
        let Ok(reference) = models::safe_query_value(&receipt.reference) else {
            return Ok(false);
        };

        let path = format!("/v2/invoices?external_id={reference}");
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let invoices: Vec<XenditInvoice> =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;

        // Empty result -- fail closed to "not settled", not an Err. See
        // module docs: this is a deliberate adaptation of cackle's own
        // Err(ErrXenditInvoiceNotFound).
        let Some(inv) = invoices.first() else {
            return Ok(false);
        };
        // Never trust a mismatched external_id, even if the HTTP call
        // otherwise succeeded -- mirrors cackle's own check (an Err there,
        // Ok(false) here per the same fail-closed adaptation).
        if inv.external_id != reference {
            return Ok(false);
        }

        // A validly-parsed-but-semantically-malformed invoice also fails
        // closed to Ok(false) here (see module docs, mirrors
        // stripe::rail::verify's identical treatment of
        // models::evaluate_session's Err case).
        let Ok(outcome) = models::invoice_to_outcome(inv) else {
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

    // refund(): trait default `Err(Error::Unsupported("refund"))` -- see
    // module docs.

    /// Verify a Xendit callback — see
    /// [`crate::xendit::webhook::verify_and_parse`]. The `x-callback-token`
    /// header is a STATIC per-account shared secret echoed back, not a body
    /// signature (Xendit's design, preserved not strengthened); the parser
    /// only produces an event for a `PAID` invoice, so a delivery that
    /// reaches this point is settled.
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event = crate::xendit::webhook::verify_and_parse(
            &self.config.webhook_token,
            &delivery.raw_body,
            delivery.header_or_empty("x-callback-token"),
        )
        .map_err(|e| Error::InvalidRequest(e.to_string()))?;
        Ok(WebhookEvent::settlement(
            &self.id,
            event.event_id,
            event.reference,
            true,
            event.amount_minor,
            event.currency,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(amount: u64, currency: &str, destination: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: destination.into(),
            reference: reference.into(),
        }
    }

    fn config() -> XenditConfig {
        XenditConfig {
            secret_key: "xnd_test_fake".to_string(),
            webhook_token: "test-callback-token".to_string(),
            requires_kyc: true,
            currencies: vec![
                "IDR".into(),
                "PHP".into(),
                "VND".into(),
                "THB".into(),
                "MYR".into(),
            ],
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> XenditRail {
        let mut rail = XenditRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/xendit_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert!(!caps.reversible, "cackle's Capabilities.Refunds is false");
        assert_eq!(rail.id(), "xendit");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.secret_key.clear();
        assert!(XenditRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.webhook_token.clear();
        assert!(XenditRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.currencies.clear();
        assert!(XenditRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("^/v2/invoices$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_1",
                "external_id": "ord_1",
                "status": "PENDING",
                "amount": 10000,
                "currency": "IDR",
                "invoice_url": "https://checkout.xendit.co/web/inv_1"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(10000, "IDR", "https://example.com/return", "ord_1"))
            .await
            .unwrap();
        assert_eq!(receipt.reference, "ord_1");
        assert_eq!(
            receipt.amount_minor, 0,
            "nothing has settled yet at charge time"
        );
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.invoice_url, "https://checkout.xendit.co/web/inv_1");
    }

    #[tokio::test]
    async fn charge_sends_basic_auth_with_empty_password() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("^/v2/invoices$"))
            .respond_with(|req: &wiremock::Request| {
                let auth = req.headers.get("Authorization").unwrap().to_str().unwrap();
                assert!(auth.starts_with("Basic "));
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "inv_1",
                    "external_id": "ord_1",
                    "status": "PENDING",
                    "amount": 10000,
                    "currency": "IDR",
                    "invoice_url": "https://checkout.xendit.co/web/inv_1"
                }))
            })
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        rail.charge(&req(10000, "IDR", "https://example.com/return", "ord_1"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn charge_provider_error_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("^/v2/invoices$"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"message": "invalid amount"})),
            )
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(10000, "IDR", "https://example.com/return", "ord_1"))
            .await
            .unwrap_err();
        match err {
            Error::Rail(msg) => assert!(msg.contains("invalid amount")),
            other => panic!("expected Error::Rail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/v2/invoices$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": "inv_1",
                "external_id": "ord_1",
                "status": "PAID",
                "amount": 10000,
                "paid_amount": 10000,
                "currency": "IDR",
                "paid_at": "2026-07-20T10:00:00.000Z"
            }])))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "xendit".into(),
            amount_minor: 0,
            currency: "IDR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_not_found_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/v2/invoices$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "xendit".into(),
            amount_minor: 0,
            currency: "IDR".into(),
            reference: "ord_missing".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(
            !rail.verify(&receipt).await.unwrap(),
            "empty invoice list must be Ok(false), not Err"
        );
    }

    #[tokio::test]
    async fn verify_pending_is_not_paid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/v2/invoices$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": "inv_1",
                "external_id": "ord_1",
                "status": "PENDING",
                "amount": 10000,
                "currency": "IDR"
            }])))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "xendit".into(),
            amount_minor: 0,
            currency: "IDR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_fails_closed_on_amount_or_currency_mismatch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/v2/invoices$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": "inv_1",
                "external_id": "ord_1",
                "status": "PAID",
                "amount": 10000,
                "paid_amount": 10000,
                "currency": "IDR"
            }])))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let genuine = Receipt {
            rail_id: "xendit".into(),
            amount_minor: 1_000_000,
            currency: "IDR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&genuine).await.unwrap());

        let mut inflated = genuine.clone();
        inflated.amount_minor = 999_999_999;
        assert!(!rail.verify(&inflated).await.unwrap());

        let mut wrong_currency = genuine.clone();
        wrong_currency.currency = "PHP".into();
        assert!(!rail.verify(&wrong_currency).await.unwrap());
    }

    #[tokio::test]
    async fn verify_malformed_json_fails_closed_as_err() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/v2/invoices$"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "xendit".into(),
            amount_minor: 0,
            currency: "IDR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_provider_500_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/v2/invoices$"))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(json!({"message": "internal error"})),
            )
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "xendit".into(),
            amount_minor: 0,
            currency: "IDR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "xendit".into(),
            amount_minor: 100,
            currency: "IDR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        let err = rail.refund(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }
}
