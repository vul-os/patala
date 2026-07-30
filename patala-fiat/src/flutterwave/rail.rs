//! [`FlutterwaveRail`] — the `PaymentRail` implementation. Ported from
//! cackle's `internal/payments/flutterwave.go` (`FlutterwaveProvider`).
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (initializes a Flutterwave Standard payment, returns
//!   its hosted checkout `link`) maps to [`PaymentRail::charge`]. **Gap vs
//!   cackle** (see `PORTING.md` §3): Flutterwave's Initialize API REQUIRES
//!   a buyer email (cackle: `"payments: flutterwave: buyer email is
//!   required"`), and `patala_core::PayRequest` has no buyer-contact field.
//!   This port reinterprets `PayRequest::destination` AS the buyer's email
//!   address, exactly the same choice `paystack::PaystackRail` makes for the
//!   identical structural reason. Callers of `FlutterwaveRail::charge` must
//!   pass the buyer's email as `destination`.
//! - cackle's `Order.CallbackURL` (Flutterwave's `redirect_url`) has no
//!   `PayRequest` home either and is never sent — Flutterwave falls back to
//!   its own default post-payment behaviour, same as cackle's adapter does
//!   when `CallbackURL` is empty (cackle sends it unconditionally, empty
//!   string included; this port matches that literally).
//! - **Money quirk, ported exactly** (see `models.rs`'s module docs):
//!   Flutterwave's `amount` field is a decimal string in MAJOR units, not
//!   Paystack/Stripe-style integer minor units — every conversion here goes
//!   through [`crate::currency::minor_to_major_string`]/
//!   [`crate::currency::major_string_to_minor`].
//! - cackle's `Verify(reference)` maps to [`PaymentRail::verify`].
//!   **Genuine divergence, not an inconsistency** (mirrors `paystack::rail`
//!   exactly): Flutterwave's `Begin` returns `Charge.Reference = o.Reference`
//!   (the caller's own reference — no separate provider-assigned id), so
//!   `verify()` operates on `Receipt::reference` directly (used as the
//!   `tx_ref` query parameter Flutterwave's `verify_by_reference` is keyed
//!   on) rather than anything decoded from `proof`. See `proof.rs`.
//! - **Divergence from cackle's OWN error convention, adopted from this
//!   crate's established pilots**: cackle's `Verify` returns a hard `error`
//!   when the API's top-level `status != "success"` or the returned `tx_ref`
//!   doesn't match the one requested. `paystack::rail::PaystackRail::verify`
//!   already established this crate's convention of treating exactly this
//!   class of content-level response inconsistency as `Ok(false)`
//!   (fail-closed-as-not-settled) rather than `Err` — reserving `Err`
//!   strictly for "the HTTP call itself failed" or "the response body
//!   didn't even parse as JSON". This port follows that same convention for
//!   consistency with the rest of the crate; it does not change WHAT is
//!   trusted, only how the "don't trust it" verdict is spelled.
//! - cackle's `Webhook` maps to [`PaymentRail::verify_webhook`], which
//!   delegates to the free function
//!   [`crate::flutterwave::webhook::verify_and_parse`]. The function keeps the
//!   pure, directly-testable shape; the trait method is what a consumer
//!   dispatching through `dyn PaymentRail` — the UniFFI binding, the
//!   sidecar — can actually reach.
//! - **Not ported**: cackle's tests that exercise `HandleWebhook`'s replay
//!   protection and reconciliation (`TestFlutterwaveWebhook_ReplayedThroughHandleWebhook`,
//!   `..._AmountMismatchFailsClosed`, `..._CurrencyMismatchFailsClosed`).
//!   `patala_core::PaymentRail` has no `HandleWebhook`-equivalent
//!   orchestration layer at all (replay dedup and reconciliation against a
//!   caller's own stored order are explicitly the CALLER's job — see
//!   `PORTING.md` §6's `event_id` discussion); `stripe`/`paystack`'s own
//!   ports omit the identical cackle tests for the identical reason, so this
//!   is consistent with the established pattern, not an oversight.
//! - `refund()`: **not implemented.** Cackle's `Capabilities().Refunds` is
//!   `false` for Flutterwave with NO "supports it, not implemented here"
//!   comment (unlike Paystack's) — unlike `stripe`/`paystack`, this port
//!   does not add refund as new code, since fabricating an undocumented
//!   money-movement wire shape without cackle's own reference AND without a
//!   verified live account to check against is exactly the kind of
//!   unverifiable invention `PORTING.md` §7 and §10 warn against. Returns
//!   the trait default (`Error::Unsupported`).

use async_trait::async_trait;
use serde::Deserialize;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::flutterwave::config::FlutterwaveConfig;
use crate::flutterwave::models::{self, FlutterwaveTransactionPayload};
use crate::flutterwave::proof::ChargeProof;

const FLUTTERWAVE_API_BASE: &str = "https://api.flutterwave.com/v3";

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Light sanity check on a reference used as a query-string value. Actual
/// percent-encoding is delegated to `reqwest`'s `.query()` (backed by the
/// `url` crate it already depends on) — this only rejects control
/// characters that have no legitimate place in a reference string.
fn safe_reference(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['\t', '\n', '\r']) {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe reference"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to Flutterwave's v3 Standard Payments API. See
/// module docs for the full `Provider` -> `PaymentRail` mapping.
pub struct FlutterwaveRail {
    id: String,
    config: FlutterwaveConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl FlutterwaveRail {
    /// Build a rail from configuration. Fails if `secret_key`,
    /// `webhook_hash`, or `currencies` are empty — mirrors cackle's
    /// `NewFlutterwave` requiring both credentials up front.
    pub fn new(config: FlutterwaveConfig) -> Result<Self> {
        if config.secret_key.trim().is_empty() {
            return Err(Error::InvalidRequest("secret_key must not be empty".into()));
        }
        if config.webhook_hash.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_hash must not be empty".into(),
            ));
        }
        if config.currencies.is_empty() {
            return Err(Error::InvalidRequest("currencies must not be empty".into()));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building flutterwave http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Flutterwave (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "flutterwave".to_string(),
            config,
            http,
            capabilities,
            base_url: FLUTTERWAVE_API_BASE.to_string(),
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

    async fn do_json_post(&self, path: &str, body: &serde_json::Value) -> Result<(Vec<u8>, u16)> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.secret_key),
            )
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Rail(format!("flutterwave: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("flutterwave: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("flutterwave: {e}")))?;
        Ok((bytes.to_vec(), status))
    }

    async fn do_get_query(&self, path: &str, query: &[(&str, &str)]) -> Result<(Vec<u8>, u16)> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.secret_key),
            )
            .query(query)
            .send()
            .await
            .map_err(|e| Error::Rail(format!("flutterwave: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("flutterwave: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("flutterwave: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for FlutterwaveRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::buyer_email`], because on the `flutterwave` rail
    /// `destination` is not a payout address: it is the **buyer's** email
    /// address, sent as Flutterwave's `customer.email` (see this module's docs above).
    ///
    /// So the honest ceiling here is
    /// [`patala_core::DestinationStatus::Unknown`], never
    /// `StructurallyValid` — an email identifies a person, not a place money
    /// goes, and whether the mailbox exists or is the right one is not
    /// decidable offline. What *is* decided: a string that is plainly not an
    /// email address is refused, and a blockchain address or a private key
    /// pasted here is refused **by name**.
    ///
    /// Giving a customer their money back on this `CustodialReversible` rail
    /// is [`PaymentRail::refund`] — back the way it came, no destination
    /// involved — not a charge to a customer-supplied address.
    fn validate_destination(&self, dest: &str) -> patala_core::DestinationVerdict {
        crate::destination::buyer_email(self.id(), dest)
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        crate::currency::minor_to_major_string(req.amount_minor, &req.currency)
            .map_err(|e| Error::InvalidRequest(e.to_string()))?;

        // NEEDS-CONFIRMATION (mirrors every other rail's identical note):
        // Flutterwave's documented API has no pre-charge fee-quote endpoint,
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
        // See module docs: `destination` is reinterpreted as the buyer's
        // email address, which Flutterwave's Initialize API requires.
        let email = req.destination.trim();
        if email.is_empty() {
            return Err(Error::InvalidRequest(
                "flutterwave: destination (used as the buyer email) is required".into(),
            ));
        }
        let currency = req.currency.trim().to_ascii_uppercase();
        let major_amount = crate::currency::minor_to_major_string(req.amount_minor, &currency)
            .map_err(|e| Error::InvalidRequest(e.to_string()))?;

        let body = serde_json::json!({
            "tx_ref": req.reference,
            "amount": major_amount,
            "currency": currency,
            "redirect_url": "",
            "customer": { "email": email, "name": "" },
        });

        let (resp_body, status) = self.do_json_post("/payments", &body).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(Deserialize, Default)]
        struct CreateData {
            #[serde(default)]
            link: String,
        }
        #[derive(Deserialize)]
        struct CreateResponse {
            status: String,
            #[serde(default)]
            data: CreateData,
        }
        let parsed: CreateResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.status != "success" || parsed.data.link.is_empty() {
            return Err(models::malformed(&format!(
                "status={:?} or empty link",
                parsed.status
            )));
        }

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see PORTING.md §5
            currency,
            reference: req.reference.clone(),
            proof: ChargeProof {
                redirect_url: Some(parsed.data.link),
            }
            .to_bytes(),
            settled_at_unix: 0,
        })
    }

    async fn verify(&self, receipt: &Receipt) -> Result<bool> {
        if receipt.rail_id != self.id {
            return Ok(false);
        }
        let Ok(reference) = safe_reference(&receipt.reference) else {
            return Ok(false);
        };

        let (body, status) = self
            .do_get_query(
                "/transactions/verify_by_reference",
                &[("tx_ref", reference)],
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }

        #[derive(Deserialize)]
        struct VerifyResponse {
            status: String,
            #[serde(default)]
            data: FlutterwaveTransactionPayload,
        }
        let parsed: VerifyResponse =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        // See module docs: cackle treats these next two checks as hard
        // errors; this port follows paystack::rail's established
        // convention of Ok(false) for content-level response
        // inconsistencies, reserving Err for transport/parse failures.
        if parsed.status != "success" {
            return Ok(false);
        }
        if !parsed.data.tx_ref.is_empty() && parsed.data.tx_ref != reference {
            return Ok(false);
        }
        let Ok(outcome) = models::evaluate_transaction(&parsed.data) else {
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

    /// Verify a Flutterwave webhook delivery — see
    /// [`crate::flutterwave::webhook::verify_and_parse`]. The `verif-hash`
    /// header is a STATIC shared secret, not a keyed MAC over the body:
    /// that weakness is Flutterwave's, preserved faithfully rather than
    /// silently "improved" into a scheme Flutterwave does not send.
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event = crate::flutterwave::webhook::verify_and_parse(
            &self.config.webhook_hash,
            &delivery.raw_body,
            delivery.header_or_empty("verif-hash"),
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

    fn req(amount: u64, currency: &str, email: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: email.into(),
            reference: reference.into(),
        }
    }

    fn config() -> FlutterwaveConfig {
        FlutterwaveConfig {
            secret_key: "sk_test_fake".to_string(),
            webhook_hash: "test-webhook-hash".to_string(),
            requires_kyc: true,
            currencies: crate::flutterwave::config::DEFAULT_CURRENCIES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> FlutterwaveRail {
        let mut rail = FlutterwaveRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/flutterwave_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(rail.id(), "flutterwave");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.secret_key.clear();
        assert!(FlutterwaveRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.webhook_hash.clear();
        assert!(FlutterwaveRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.currencies.clear();
        assert!(FlutterwaveRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "message": "ok",
                "data": {"link": "https://checkout.flutterwave.com/pay/abc"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(10050, "NGN", "a@b.com", "ord_1"))
            .await
            .unwrap();
        assert_eq!(receipt.reference, "ord_1");
        assert_eq!(
            receipt.amount_minor, 0,
            "nothing has settled yet at charge time"
        );
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(
            proof.redirect_url.as_deref(),
            Some("https://checkout.flutterwave.com/pay/abc")
        );
    }

    #[tokio::test]
    async fn charge_requires_email() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let err = rail
            .charge(&req(10050, "NGN", "", "ord_1"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn charge_provider_error_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payments"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({"status":"error","message":"invalid key"})),
            )
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(100, "NGN", "a@b.com", "ord_1"))
            .await
            .unwrap_err();
        match err {
            Error::Rail(msg) => assert!(msg.contains("invalid key")),
            other => panic!("expected Error::Rail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/transactions/verify_by_reference"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {"id": 123, "tx_ref": "ord_1", "flw_ref": "FLW-REF", "amount": 100.50, "currency": "NGN", "status": "successful"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "flutterwave".into(),
            amount_minor: 0,
            currency: "NGN".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_failed_status_is_not_settled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/transactions/verify_by_reference"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {"id": 1, "tx_ref": "ord_1", "amount": 100, "currency": "NGN", "status": "failed"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "flutterwave".into(),
            amount_minor: 0,
            currency: "NGN".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_malformed_json_fails_closed_with_err() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/transactions/verify_by_reference"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "flutterwave".into(),
            amount_minor: 0,
            currency: "NGN".into(),
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
            .and(path("/transactions/verify_by_reference"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(json!({"status":"error","message":"oops"})),
            )
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "flutterwave".into(),
            amount_minor: 0,
            currency: "NGN".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_fails_closed_on_amount_or_currency_mismatch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/transactions/verify_by_reference"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {"id": 1, "tx_ref": "ord_1", "amount": 5, "currency": "NGN", "status": "successful"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let genuine = Receipt {
            rail_id: "flutterwave".into(),
            amount_minor: 500,
            currency: "NGN".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&genuine).await.unwrap());

        let mut inflated = genuine.clone();
        inflated.amount_minor = 999_999;
        assert!(!rail.verify(&inflated).await.unwrap());

        let mut wrong_currency = genuine.clone();
        wrong_currency.currency = "GHS".into();
        assert!(!rail.verify(&wrong_currency).await.unwrap());

        let mut wrong_rail = genuine.clone();
        wrong_rail.rail_id = "some-other-rail".into();
        assert!(!rail.verify(&wrong_rail).await.unwrap());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "flutterwave".into(),
            amount_minor: 100,
            currency: "NGN".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(matches!(
            rail.refund(&receipt).await,
            Err(Error::Unsupported(_))
        ));
    }
}
