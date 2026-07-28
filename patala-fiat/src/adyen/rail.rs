//! [`AdyenRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/adyen.go` (`AdyenProvider`).
//!
//! Built against Adyen's DOCUMENTED public API (Checkout API v71, Pay by
//! Link) — see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE"
//! disclosure every rail beyond `manual` carries.
//!
//! ## Which endpoint, and why (ported verbatim from cackle's file doc comment)
//!
//! Adyen's Checkout API offers two hosted paths: `/sessions` (returns
//! `sessionData` that must be consumed by Adyen's Web Drop-in/Components
//! JS — no plain redirect URL) and `/paymentLinks` (returns an actual `url`
//! field for a plain browser redirect, no client-side JS required). Since
//! `patala_core::PaymentRail::charge` has no notion of a client-side SDK,
//! `/paymentLinks` is the only one of the two that fits — this file uses
//! ONLY `/paymentLinks`, exactly as cackle's `adyen.go` does.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates an Adyen payment link, returns its hosted
//!   redirect URL) maps to [`PaymentRail::charge`]. **Gap vs cackle**: Adyen
//!   Pay by Link requires a `returnUrl`, and `PayRequest` has no callback-url
//!   field — this port reinterprets `PayRequest::destination` AS that return
//!   URL, exactly the same reinterpretation `stripe::rail::StripeRail`
//!   applies to the same field (see `PORTING.md` §3). Callers of
//!   `AdyenRail::charge` must pass the desired post-payment return URL as
//!   `destination`, NOT a payment method token.
//! - cackle's `Verify` is **NOT PORTED AS A WORKING CHECK** — it is ported
//!   FAITHFULLY as an always-failing call, because that is EXACTLY what
//!   cackle's own `adyen.go` `Verify` does: it unconditionally returns an
//!   error, with a long comment explaining why (Adyen's Pay by Link resource
//!   can only be retrieved by the LINK's own id, `GET /paymentLinks/{id}`,
//!   not by the merchant `reference` this rail was given, and that
//!   retrieval only reports the link's status — active/expired/completed —
//!   never authoritative payment settlement detail). Rather than fabricate a
//!   verify path cackle itself refused to build, this port's `verify()`
//!   ALWAYS returns `Err(Error::Unsupported(...))`, regardless of the
//!   receipt passed in. **Callers integrating this rail MUST rely on
//!   [`crate::adyen::webhook::verify_and_parse`] (the `AUTHORISATION`
//!   webhook) as the sole authoritative settlement signal** — exactly as
//!   Adyen's own docs and cackle's `adyen.go` recommend. This is a
//!   deliberate, disclosed divergence from `patala_core::PaymentRail::verify`'s
//!   own doc comment (which reserves `Err` for "an operational failure to
//!   even perform the check"): here the incapacity is structural, not
//!   transient, and returning a fabricated `Ok(false)` would be less honest
//!   than surfacing the same "not supported" signal cackle itself chose.
//! - cackle's `Webhook` maps to [`PaymentRail::verify_webhook`], which
//!   delegates to the free function
//!   [`crate::adyen::webhook::verify_and_parse`]. The function keeps the
//!   pure, directly-testable shape; the trait method is what a consumer
//!   dispatching through `dyn PaymentRail` — the UniFFI binding, the
//!   sidecar — can actually reach.
//! - `refund()`: **NOT a cackle port** (cackle's `Provider` interface has no
//!   `Refund` method at all; `Capabilities.Refunds: true` is descriptive
//!   metadata only). New code grounded in Adyen's own public Refunds API
//!   (<https://docs.adyen.com/online-payments/refund/>,
//!   endpoint `POST /payments/{paymentPspReference}/refunds`). **Requires a
//!   `psp_reference` already present on the receipt's proof** — unlike
//!   Stripe (whose `refund()` re-fetches the session to discover its
//!   PaymentIntent id), Adyen's Pay by Link create response never returns a
//!   `pspReference` at all (see `proof.rs`'s module docs), and `verify()`
//!   cannot re-fetch one either (see above) — so there is no network call
//!   this method could make to discover it. A caller must first receive and
//!   validate an `AUTHORISATION` webhook (via
//!   [`crate::adyen::webhook::verify_and_parse`]) and update its own stored
//!   `Receipt`'s proof with the resulting `psp_reference` before `refund()`
//!   can do anything. Adyen's refund endpoint is itself asynchronous — its
//!   response never confirms completion synchronously (only a later
//!   `REFUND` webhook notification would, and neither cackle's own adapter
//!   nor this port builds one) — so this method ALWAYS returns
//!   `Receipt { amount_minor: 0, .. }`, honestly reporting "refund
//!   initiated, not yet confirmed moved" rather than fabricating a
//!   synchronous success.

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::adyen::config::AdyenConfig;
use crate::adyen::models;
use crate::adyen::proof::{ChargeProof, RefundProof};

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Mirrors `patala-hyperswitch`/`stripe::rail`/`paystack::rail`'s identical
/// `safe_path_segment` helper.
fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for an adyen id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to Adyen's Checkout API (Pay by Link). See
/// module docs for the full `Provider` -> `PaymentRail` mapping.
pub struct AdyenRail {
    id: String,
    api_key: String,
    merchant_account: String,
    hmac_key: Vec<u8>,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl AdyenRail {
    /// Build a rail from configuration. Fails if any of `api_key`,
    /// `merchant_account`, `hmac_key_hex`, or `api_base_url` are empty, or if
    /// `hmac_key_hex` is not valid hex — mirrors cackle's `NewAdyen`
    /// requiring all four env vars and hex-decoding the HMAC key.
    pub fn new(config: AdyenConfig) -> Result<Self> {
        if config.api_key.trim().is_empty() {
            return Err(Error::InvalidRequest("api_key must not be empty".into()));
        }
        if config.merchant_account.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "merchant_account must not be empty".into(),
            ));
        }
        if config.hmac_key_hex.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "hmac_key_hex must not be empty".into(),
            ));
        }
        if config.api_base_url.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "api_base_url must not be empty".into(),
            ));
        }
        let hmac_key = hex::decode(config.hmac_key_hex.trim())
            .map_err(|e| Error::InvalidRequest(format!("hmac_key_hex is not valid hex: {e}")))?;

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building adyen http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Adyen (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "adyen".to_string(),
            api_key: config.api_key,
            merchant_account: config.merchant_account,
            hmac_key,
            http,
            capabilities,
            base_url: config.api_base_url.trim_end_matches('/').to_string(),
        })
    }

    /// The (hex-decoded) HMAC key this rail was configured with, for a
    /// caller to pass into [`crate::adyen::webhook::verify_and_parse`] --
    /// see module docs on why webhook verification is a free function that
    /// needs this passed in explicitly rather than a rail method.
    pub fn hmac_key(&self) -> &[u8] {
        &self.hmac_key
    }

    /// Mirrors `stripe::rail`'s identical `check_currency`: an empty
    /// `currencies` config means unrestricted, matching cackle's own
    /// `Currencies: nil` for Adyen.
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
            .header("X-API-Key", &self.api_key)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("adyen: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("adyen: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("adyen: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for AdyenRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        models::adyen_amount(req.amount_minor, &req.currency)?;

        // NEEDS-CONFIRMATION (mirrors stripe/paystack's identical note):
        // Adyen's documented API has no pre-charge fee-quote endpoint
        // either, and cackle's own adapter has no Quote-equivalent method.
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
        let amount = models::adyen_amount(req.amount_minor, &currency)?;

        // See module docs: `destination` is reinterpreted as the returnUrl
        // Adyen Pay by Link requires (cackle's `Order.CallbackURL`).
        let body = serde_json::json!({
            "amount": {"value": amount, "currency": currency},
            "reference": req.reference,
            "returnUrl": req.destination,
            "merchantAccount": self.merchant_account,
        });

        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/paymentLinks", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        let parsed: models::PaymentLinkResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.id.is_empty() || parsed.url.is_empty() {
            return Err(models::malformed("empty payment link id or url"));
        }

        let proof = ChargeProof {
            payment_link_id: parsed.id,
            psp_reference: None, // not returned by /paymentLinks -- see proof.rs
            redirect_url: Some(parsed.url),
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

    /// ALWAYS fails — see module docs' `Provider` -> `PaymentRail` mapping
    /// section for why this faithfully mirrors cackle's own `Verify`, which
    /// unconditionally returns an error for every input.
    async fn verify(&self, _receipt: &Receipt) -> Result<bool> {
        Err(Error::Unsupported(
            "verify (Adyen's Pay by Link resource cannot be looked up by merchant reference; rely on the AUTHORISATION webhook via crate::adyen::webhook::verify_and_parse as the authoritative settlement signal)",
        ))
    }

    async fn refund(&self, receipt: &Receipt) -> Result<Receipt> {
        // See module docs: new code grounded in Adyen's public Refunds API,
        // not a cackle port.
        if receipt.rail_id != self.id {
            return Err(Error::InvalidRequest(format!(
                "receipt names rail {:?}, not {:?}",
                receipt.rail_id, self.id
            )));
        }
        let proof = ChargeProof::from_bytes(&receipt.proof).ok_or_else(|| {
            Error::InvalidRequest("receipt proof is not an adyen charge proof".into())
        })?;
        let psp_reference = proof.psp_reference.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
            Error::Rail(
                "adyen: receipt has no psp_reference -- refund requires a receipt whose proof was updated from a verified AUTHORISATION webhook (see proof.rs module docs)".into(),
            )
        })?;
        let psp_reference = safe_path_segment(&psp_reference)?.to_string();

        let currency = receipt.currency.trim().to_ascii_uppercase();
        let amount = models::adyen_amount(receipt.amount_minor, &currency)?;
        let body = serde_json::json!({
            "amount": {"value": amount, "currency": currency},
            "merchantAccount": self.merchant_account,
            "reference": receipt.reference,
        });

        let path = format!("/payments/{psp_reference}/refunds");
        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, &path, Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(serde::Deserialize, Default)]
        struct RefundResponse {
            #[serde(default, rename = "pspReference")]
            psp_reference: String,
            #[serde(default)]
            status: String,
        }
        let parsed: RefundResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;

        // Adyen's refund modification response never confirms completion
        // synchronously (only "received"/similar acknowledgement statuses --
        // see module docs); this always reports amount_minor: 0 until a
        // REFUND webhook (not built here, see module docs) would confirm it.
        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0,
            currency,
            reference: receipt.reference.clone(),
            proof: RefundProof {
                refund_psp_reference: parsed.psp_reference,
                status_at_refund: parsed.status,
            }
            .to_bytes(),
            settled_at_unix: 0,
        })
    }

    /// Verify an Adyen notification (HMAC-SHA256 over the colon-joined
    /// signing string, signature carried in
    /// `additionalData.hmacSignature` — **no HTTP header**) — see
    /// [`crate::adyen::webhook::verify_and_parse`].
    ///
    /// This is Adyen's authoritative settlement signal: [`Self::verify`]
    /// returns [`Error::Unsupported`] because a Pay by Link resource cannot
    /// be looked up by merchant reference, so for this rail the push path is
    /// the *only* path.
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event = crate::adyen::webhook::verify_and_parse(&self.hmac_key, &delivery.raw_body)
            .map_err(|e| Error::InvalidRequest(e.to_string()))?;
        let psp_reference = event.psp_reference.clone();
        Ok(WebhookEvent::settlement(
            &self.id,
            event.event_id,
            event.reference,
            event.settled,
            event.amount_minor,
            event.currency,
        )
        .with_object_id(psp_reference))
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

    fn config() -> AdyenConfig {
        AdyenConfig {
            api_key: "test-api-key".to_string(),
            merchant_account: "TestMerchant".to_string(),
            hmac_key_hex: hex::encode(b"test-adyen-hmac-key-32-bytes!!!!"),
            api_base_url: "http://127.0.0.1:1".to_string(),
            requires_kyc: true,
            currencies: Vec::new(),
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> AdyenRail {
        let mut rail = AdyenRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/adyen_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.reversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(rail.id(), "adyen");
    }

    #[test]
    fn new_requires_all_four_fields() {
        let mut cfg = config();
        cfg.api_key.clear();
        assert!(AdyenRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.merchant_account.clear();
        assert!(AdyenRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.hmac_key_hex.clear();
        assert!(AdyenRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.api_base_url.clear();
        assert!(AdyenRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.hmac_key_hex = "not-hex-zzz".to_string();
        assert!(AdyenRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn quote_never_fabricates_a_fee_and_refuses_non_iso_standard_currency() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let q = rail
            .quote(&req(5000, "EUR", "https://example.com/return", "ord_1"))
            .await
            .unwrap();
        assert_eq!(q.fee_minor, 0);
        assert_eq!(q.total_minor, 5000);

        assert!(rail
            .quote(&req(1000, "ISK", "https://example.com/return", "ord_1"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn charge_posts_expected_shape() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/paymentLinks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "LINK123",
                "url": "https://test.adyen.link/LINK123",
                "status": "active"
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
        assert_eq!(proof.payment_link_id, "LINK123");
        assert!(proof.psp_reference.is_none());
        assert_eq!(
            proof.redirect_url.as_deref(),
            Some("https://test.adyen.link/LINK123")
        );
    }

    #[tokio::test]
    async fn charge_refuses_non_iso_standard_currency_without_calling_server() {
        let server = MockServer::start().await;
        // No Mock registered -- if the adapter called the server anyway
        // wiremock would panic on an unexpected request.
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(1000, "ISK", "https://example.com/return", "ord_1"))
            .await
            .expect_err("non-ISO-standard currency must be refused before any network call");
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn charge_http_500_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/paymentLinks"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                json!({"status": 500, "errorCode": "901", "message": "Invalid Merchant Account"}),
            ))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(5000, "EUR", "https://example.com/return", "ord_1"))
            .await
            .unwrap_err();
        match err {
            Error::Rail(msg) => assert!(msg.contains("Invalid Merchant Account")),
            other => panic!("expected Error::Rail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_always_fails_matching_cackle() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "adyen".into(),
            amount_minor: 0,
            currency: "EUR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_link_id: "LINK123".into(),
                psp_reference: None,
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[tokio::test]
    async fn refund_requires_psp_reference_on_proof() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "adyen".into(),
            amount_minor: 5000,
            currency: "EUR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_link_id: "LINK123".into(),
                psp_reference: None,
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let err = rail.refund(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    #[tokio::test]
    async fn refund_posts_expected_shape_and_is_always_pending() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payments/psp_1/refunds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "pspReference": "psp_refund_1",
                "status": "received"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "adyen".into(),
            amount_minor: 5000,
            currency: "EUR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                payment_link_id: "LINK123".into(),
                psp_reference: Some("psp_1".into()),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let refund_receipt = rail.refund(&receipt).await.unwrap();
        assert_eq!(
            refund_receipt.amount_minor, 0,
            "adyen refunds never confirm completion synchronously"
        );
        let proof = RefundProof::from_bytes(&refund_receipt.proof).unwrap();
        assert_eq!(proof.refund_psp_reference, "psp_refund_1");
    }

    #[tokio::test]
    async fn refund_rejects_a_receipt_from_a_different_rail() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let foreign = Receipt {
            rail_id: "manual".into(),
            amount_minor: 100,
            currency: "EUR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.refund(&foreign).await.is_err());
    }
}
