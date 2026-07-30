//! [`MercadoPagoRail`] — the `PaymentRail` implementation. Ported from
//! cackle's `internal/payments/mercadopago.go` (`MercadoPagoProvider`).
//!
//! Built against Mercado Pago's DOCUMENTED public API (Checkout Pro /
//! Preferences, Payments) — see this crate's `PORTING.md` "UNVERIFIED
//! AGAINST LIVE" disclosure; cackle's own file doc comment additionally
//! self-rates its confidence as "MEDIUM-HIGH on the webhook signature
//! manifest template... MEDIUM on the Preferences API request/response
//! shape", not run against a real Mercado Pago test account either.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a Checkout Pro Preference, returns its hosted
//!   `init_point`) maps to [`PaymentRail::charge`]. `PayRequest::reference`
//!   maps directly onto Mercado Pago's own `external_reference` — no
//!   callback-url reinterpretation is *required* the way Stripe/Adyen/
//!   Mollie need one (cackle's own `Begin` only sets `back_urls` "if
//!   `o.CallbackURL != ""`" — optional). This port still reinterprets
//!   `PayRequest::destination` as the optional post-payment return URL for
//!   consistency with every other redirect-flow rail in this crate — if
//!   `destination` happens to equal the buyer's email instead (a plausible
//!   caller mistake, since other rails in this crate use `destination` for
//!   an email), Mercado Pago's own hosted checkout simply ignores an
//!   invalid `back_urls` entry rather than failing, so this is a safe
//!   default, not a silent misuse.
//! - cackle's `Order.BuyerEmail` (optional for Mercado Pago, sets
//!   `payer.email`) has no `PayRequest` field and is simply never sent —
//!   noted as an information-loss gap, same as `PORTING.md` §3 describes
//!   for every other adapter's non-reinterpreted `Order` fields
//!   (`EventID`/`OrgID`/`BuyerName`/most of `Metadata`).
//! - cackle's `Verify(reference)` maps to [`PaymentRail::verify`], searching
//!   `GET /v1/payments/search?external_reference=...` — **the simplest case
//!   in this crate, like `paystack::rail`**: `receipt.reference` IS Mercado
//!   Pago's own `external_reference`, no separate id lives in `proof` (see
//!   `proof.rs`'s module docs).
//! - **Flagged, deliberate divergence from cackle's own `Verify`**: cackle's
//!   `Verify` returns `ErrMercadoPagoPaymentNotFound` (a hard `error`) when
//!   the search returns zero results. `patala_core::PaymentRail::verify`'s
//!   own binding contract (`PORTING.md` §6) requires `Ok(false)`, never
//!   `Err`, for "this payment doesn't exist (yet)" — that is not an
//!   operational failure to perform the check, which is the only thing
//!   `Err` is reserved for. This port therefore returns `Ok(false)` for "no
//!   results", conforming to `patala_core`'s stricter contract rather than
//!   cackle's `Provider` interface's looser one — this is adapting to the
//!   TARGET seam's own rules, not "fixing" cackle, exactly the kind of
//!   divergence `PORTING.md` asks to be surfaced rather than silently
//!   ported.
//! - cackle's `Webhook` is ported as
//!   [`crate::mercadopago::webhook::verify_signature_and_extract_id`] (the
//!   pure signature-verification half) plus [`MercadoPagoRail::handle_webhook`]
//!   (the network half, re-fetching `GET /v1/payments/{id}` and checking the
//!   fetched id matches, exactly as cackle's own `Webhook` does) — same
//!   structural divergence from a pure `webhook.rs` as `mollie::webhook`,
//!   for the same underlying reason: Mercado Pago's webhook body never
//!   carries the settled amount, so an authenticated re-fetch is mandatory,
//!   not optional.
//! - `refund()`: **left as the trait default (`Error::Unsupported`), NOT
//!   implemented.** Unlike Stripe/Paystack/Adyen/Checkout.com/Mollie (all of
//!   which cackle marks `Capabilities.Refunds: true`, several with an
//!   explicit "supports it, not implemented here" comment), cackle's own
//!   Mercado Pago adapter sets `Capabilities.Refunds: false` with NO such
//!   comment — the one adapter in this batch where cackle's own signal
//!   reads as "not investigated" rather than "known-supported, just not
//!   wired up". Per `PORTING.md` §7's own escape hatch ("if you cannot find
//!   your provider's refund API documented... leave the trait default"),
//!   this port does not fabricate a refund implementation for an endpoint
//!   this environment has not independently confirmed against Mercado
//!   Pago's own docs.
//! - **Not ported**: `Countries` (`mercadoPagoCountries`) — `RailCapabilities`
//!   has no country field at all, same gap every other adapter in this
//!   crate has (see `PORTING.md` §4).

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::mercadopago::config::{MercadoPagoConfig, MERCADOPAGO_API_BASE};
use crate::mercadopago::models::{self, Payment};
use crate::mercadopago::proof::ChargeProof;
use crate::mercadopago::webhook;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The settlement outcome of a webhook delivery, once
/// [`MercadoPagoRail::handle_webhook`] has re-fetched and evaluated the
/// payment the signed manifest names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MercadoPagoWebhookEvent {
    pub event_id: String,
    pub reference: String,
    pub settled: bool,
    pub amount_minor: u64,
    pub currency: String,
}

/// One `PaymentRail` talking to Mercado Pago's Checkout Pro (Preferences)
/// and Payments APIs. See module docs for the full `Provider` ->
/// `PaymentRail` mapping.
pub struct MercadoPagoRail {
    id: String,
    config: MercadoPagoConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl MercadoPagoRail {
    /// Build a rail from configuration. Fails if `access_token`,
    /// `webhook_secret`, or `currencies` are empty.
    pub fn new(config: MercadoPagoConfig) -> Result<Self> {
        if config.access_token.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "access_token must not be empty".into(),
            ));
        }
        if config.webhook_secret.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_secret must not be empty".into(),
            ));
        }
        if config.currencies.is_empty() {
            return Err(Error::InvalidRequest("currencies must not be empty".into()));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building mercadopago http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Mercado Pago (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "mercadopago".to_string(),
            config,
            http,
            capabilities,
            base_url: MERCADOPAGO_API_BASE.to_string(),
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
            .header(
                "Authorization",
                format!("Bearer {}", self.config.access_token),
            )
            .header("Content-Type", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("mercadopago: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("mercadopago: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("mercadopago: {e}")))?;
        Ok((bytes.to_vec(), status))
    }

    /// Handle a Mercado Pago webhook delivery — see module docs and
    /// `webhook.rs`'s module docs for why this needs `&self` (a mandatory
    /// authenticated re-fetch), unlike `stripe`/`paystack`/`adyen`/
    /// `checkoutcom`'s pure-function webhook handling. Mirrors cackle's
    /// `MercadoPagoProvider.Webhook` end to end: verify signature, extract
    /// `data.id`, `GET /v1/payments/{id}`, verify the fetched payment's own
    /// id matches what the webhook claimed, then evaluate.
    pub async fn handle_webhook(
        &self,
        raw_body: &[u8],
        x_signature: &str,
        x_request_id: &str,
    ) -> Result<MercadoPagoWebhookEvent> {
        let data_id = webhook::verify_signature_and_extract_id(
            &self.config.webhook_secret,
            raw_body,
            x_signature,
            x_request_id,
        )
        .map_err(|e| Error::InvalidRequest(e.to_string()))?;

        let path = format!("/v1/payments/{data_id}");
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let pay: Payment =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        if pay.id.to_string() != data_id {
            return Err(models::malformed(&format!(
                "fetched payment id {} does not match webhook data.id {data_id:?}",
                pay.id
            )));
        }
        let outcome = models::evaluate_payment(&pay)?;
        Ok(MercadoPagoWebhookEvent {
            event_id: outcome.event_id.clone(),
            reference: outcome.reference,
            settled: outcome.settled,
            amount_minor: outcome.amount_minor,
            currency: outcome.currency,
        })
    }
}

#[async_trait]
impl PaymentRail for MercadoPagoRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::redirect_url`], because on the `mercadopago` rail
    /// `destination` is not a payout address: it is the post-checkout return
    /// URL, sent as MercadoPago's `back_urls` (`success`/`failure`/`pending`) (see this module's docs above).
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
        models::amount_minor_to_json_number(req.amount_minor, &req.currency)?;

        // NEEDS-CONFIRMATION (mirrors stripe/paystack's identical note):
        // Mercado Pago's documented API has no pre-charge fee-quote
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
        let unit_price = models::amount_minor_to_json_number(req.amount_minor, &currency)?;

        // See module docs: `destination` is reinterpreted as the (optional)
        // post-payment return URL (cackle's `Order.CallbackURL`), used for
        // all three of success/failure/pending back_urls.
        let mut body = serde_json::json!({
            "external_reference": req.reference,
            "items": [{
                "title": format!("Order {}", req.reference),
                "quantity": 1,
                "unit_price": unit_price,
                "currency_id": currency,
            }],
        });
        if !req.destination.trim().is_empty() {
            body["back_urls"] = serde_json::json!({
                "success": req.destination,
                "failure": req.destination,
                "pending": req.destination,
            });
        }

        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/checkout/preferences", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        let parsed: models::PreferenceResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.init_point.is_empty() {
            return Err(models::malformed("empty init_point"));
        }

        let proof = ChargeProof {
            preference_id: parsed.id,
            init_point: Some(parsed.init_point),
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
        if receipt.reference.trim().is_empty() {
            return Ok(false);
        }

        let path = format!(
            "/v1/payments/search?external_reference={}",
            urlencode(&receipt.reference)
        );
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }

        #[derive(serde::Deserialize, Default)]
        struct SearchResponse {
            #[serde(default)]
            results: Vec<Payment>,
        }
        let parsed: SearchResponse =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;

        // Flagged divergence from cackle's own Verify -- see module docs:
        // "no results" is Ok(false), never Err, per patala_core's own
        // fail-closed contract for verify().
        if parsed.results.is_empty() {
            return Ok(false);
        }

        for pay in &parsed.results {
            if pay.external_reference != receipt.reference {
                continue;
            }
            let Ok(outcome) = models::evaluate_payment(pay) else {
                continue;
            };
            if !outcome.settled {
                continue;
            }
            if !outcome.currency.eq_ignore_ascii_case(&receipt.currency) {
                continue;
            }
            if outcome.amount_minor < receipt.amount_minor {
                continue;
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Verify a Mercado Pago webhook delivery — delegates to
    /// [`Self::handle_webhook`] (signature check over the `x-signature`
    /// manifest, then the mandatory authenticated re-fetch of the payment
    /// the manifest names).
    ///
    /// Headers: `x-signature`, `x-request-id`.
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event = self
            .handle_webhook(
                &delivery.raw_body,
                delivery.header_or_empty("x-signature"),
                delivery.header_or_empty("x-request-id"),
            )
            .await?;
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

/// Minimal, dependency-free query-string value encoder -- this crate
/// deliberately avoids pulling in the `url` crate just for one query
/// parameter (see `Cargo.toml`'s `mercadopago` feature, which needs only
/// `reqwest`/`hmac`/`sha2`/`hex`).
fn urlencode(s: &str) -> String {
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

    fn config() -> MercadoPagoConfig {
        MercadoPagoConfig {
            access_token: "APP_USR-test-token".to_string(),
            webhook_secret: "test-mp-webhook-secret".to_string(),
            requires_kyc: true,
            currencies: vec!["ARS".into(), "BRL".into()],
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> MercadoPagoRail {
        let mut rail = MercadoPagoRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/mercadopago_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(rail.id(), "mercadopago");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.access_token.clear();
        assert!(MercadoPagoRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.webhook_secret.clear();
        assert!(MercadoPagoRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.currencies.clear();
        assert!(MercadoPagoRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/checkout/preferences$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "pref_1",
                "init_point": "https://www.mercadopago.com/checkout/v1/redirect?pref_id=pref_1"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(10050, "ARS", "https://example.com/return", "ord_1"))
            .await
            .unwrap();
        assert_eq!(receipt.reference, "ord_1");
        assert_eq!(
            receipt.amount_minor, 0,
            "nothing has settled yet at charge time"
        );
    }

    #[tokio::test]
    async fn verify_approved_payment_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v1/payments/search$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"id": 123, "status": "approved", "transaction_amount": 100.50, "currency_id": "ARS", "external_reference": "ord_1"}]
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "mercadopago".into(),
            amount_minor: 10050,
            currency: "ARS".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_no_results_is_ok_false_not_err() {
        // Flagged divergence from cackle's own Verify -- see module docs:
        // "not found" must be Ok(false), never Err.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v1/payments/search$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "mercadopago".into(),
            amount_minor: 10050,
            currency: "ARS".into(),
            reference: "ord_missing".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_fails_closed_on_amount_or_currency_mismatch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v1/payments/search$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"id": 1, "status": "approved", "transaction_amount": 5.00, "currency_id": "ARS", "external_reference": "ord_1"}]
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let genuine = Receipt {
            rail_id: "mercadopago".into(),
            amount_minor: 500,
            currency: "ARS".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&genuine).await.unwrap());

        let mut inflated = genuine.clone();
        inflated.amount_minor = 999_999;
        assert!(!rail.verify(&inflated).await.unwrap());

        let mut wrong_currency = genuine.clone();
        wrong_currency.currency = "BRL".into();
        assert!(!rail.verify(&wrong_currency).await.unwrap());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "mercadopago".into(),
            amount_minor: 100,
            currency: "ARS".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        let err = rail.refund(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[tokio::test]
    async fn handle_webhook_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v1/payments/555$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 555, "status": "approved", "transaction_amount": 100.50, "currency_id": "ARS", "external_reference": "ord_1"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());

        let body = br#"{"action":"payment.created","type":"payment","data":{"id":"555"}}"#;
        let sig = {
            use hmac::Mac;
            let manifest = "id:555;request-id:req-1;ts:1700000000;";
            let mut mac =
                hmac::Hmac::<sha2::Sha256>::new_from_slice(b"test-mp-webhook-secret").unwrap();
            mac.update(manifest.as_bytes());
            format!(
                "ts=1700000000,v1={}",
                hex::encode(mac.finalize().into_bytes())
            )
        };
        let event = rail.handle_webhook(body, &sig, "req-1").await.unwrap();
        assert!(event.settled);
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.amount_minor, 10050);
    }

    #[tokio::test]
    async fn handle_webhook_fetched_id_mismatch_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v1/payments/555$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                // Server returns a DIFFERENT payment id than requested -- never trust it.
                "id": 999, "status": "approved", "transaction_amount": 100.50, "currency_id": "ARS", "external_reference": "ord_1"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());

        let body = br#"{"action":"payment.created","type":"payment","data":{"id":"555"}}"#;
        let sig = {
            use hmac::Mac;
            let manifest = "id:555;request-id:req-1;ts:1700000000;";
            let mut mac =
                hmac::Hmac::<sha2::Sha256>::new_from_slice(b"test-mp-webhook-secret").unwrap();
            mac.update(manifest.as_bytes());
            format!(
                "ts=1700000000,v1={}",
                hex::encode(mac.finalize().into_bytes())
            )
        };
        assert!(rail.handle_webhook(body, &sig, "req-1").await.is_err());
    }
}
