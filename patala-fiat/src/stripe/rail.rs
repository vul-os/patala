//! [`StripeRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/stripe.go` (`StripeProvider`).
//!
//! Built against Stripe's DOCUMENTED public API (Checkout Sessions), exactly
//! as cackle's own adapter is — see this crate's `PORTING.md` for the
//! "UNVERIFIED AGAINST LIVE" disclosure every rail beyond `manual` in this
//! crate carries (`PATALA.md` §8).
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a Checkout Session, returns its hosted
//!   payment page URL) maps to [`PaymentRail::charge`]. **Gap vs cackle**
//!   (flagged in `PORTING.md`): cackle's `Order` carries a `CallbackURL`
//!   field Stripe Checkout requires (`success_url`/`cancel_url`) and
//!   `patala_core::PayRequest` has none — this port reinterprets
//!   `PayRequest::destination` (documented as "an opaque processor-side
//!   destination token") AS that callback/return URL for this rail
//!   specifically, since it is the only free-form per-request string slot
//!   available. Callers of `StripeRail::charge` must pass the desired
//!   post-checkout return URL as `destination`, NOT a payment method token.
//!   `PayRequest::validate()` already requires `destination` non-empty,
//!   which is exactly cackle's own "callback_url is required" check, so
//!   nothing extra needs enforcing here.
//! - cackle's `Verify(reference)` has an OPEN, cackle-acknowledged ambiguity
//!   (see `proof.rs`'s module docs: cackle's own comment admits it treats
//!   `reference` as a Stripe session id, not Cackle's order reference, and
//!   calls this "a one-line fix ... once confirmed"). `patala_core::Receipt`
//!   carries both `reference` (echoed from `PayRequest`) AND an opaque
//!   `proof` blob, so this port sidesteps the ambiguity entirely: the real
//!   Stripe session id lives in `proof` (see `proof::ChargeProof`), and
//!   `verify()` always looks it up from there — never from a bare string.
//!   The Stripe API call, JSON parsing, and settlement-status mapping
//!   (`models::evaluate_session`) are otherwise byte-for-byte the same as
//!   cackle's `parseStripeSession`.
//! - cackle's `Webhook` maps to [`PaymentRail::verify_webhook`], which
//!   delegates to the free function
//!   [`crate::stripe::webhook::verify_and_parse`]. The function keeps the
//!   pure, directly-testable shape; the trait method is what a consumer
//!   dispatching through `dyn PaymentRail` — the UniFFI binding, the
//!   sidecar — can actually reach.
//! - `refund()`: **NOT a cackle port.** Cackle's `Provider` interface never
//!   had a `Refund` method at all — `Capabilities.Refunds: true` on
//!   cackle's `StripeProvider` is descriptive metadata only ("Stripe
//!   supports refunds on the merchant's own dashboard/API"), not a
//!   Cackle-side implementation. Since `patala_core::PaymentRail` DOES
//!   require every rail to answer `refund()` (returning
//!   `Error::Unsupported` if it can't), and this task's brief asks for "real
//!   refund where the processor supports it", this method is NEW code
//!   grounded directly in Stripe's own public Refunds API
//!   (<https://docs.stripe.com/api/refunds/create>), following the exact
//!   same honesty conventions (pending-not-yet-moved, fail-closed, cite
//!   sources) as every ported method in this file. See `proof::RefundProof`.

use async_trait::async_trait;
use serde::Deserialize;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::stripe::config::StripeConfig;
use crate::stripe::models::{self, StripeSessionPayload};
use crate::stripe::proof::{ChargeProof, RefundProof};

const STRIPE_API_BASE: &str = "https://api.stripe.com/v1";

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A path segment (a Stripe session/refund id used directly in a URL) must
/// not itself carry URL structure. Mirrors `patala-hyperswitch::rail`'s
/// `safe_path_segment` helper exactly (same rationale: a simpler,
/// dependency-free, fail-closed alternative to a percent-encoding crate for
/// a value that should always be an opaque alphanumeric-ish token).
fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for a stripe id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to Stripe's Checkout Sessions API. See module
/// docs for the full `Provider` -> `PaymentRail` mapping.
pub struct StripeRail {
    id: String,
    config: StripeConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl StripeRail {
    /// Build a rail from configuration. Fails if `secret_key` or
    /// `webhook_secret` are empty — mirrors cackle's `NewStripe` refusing a
    /// half-configured adapter.
    pub fn new(config: StripeConfig) -> Result<Self> {
        if config.secret_key.trim().is_empty() {
            return Err(Error::InvalidRequest("secret_key must not be empty".into()));
        }
        if config.webhook_secret.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_secret must not be empty".into(),
            ));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building stripe http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Stripe (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
            atomic_multi_party: false, // always false: N payouts here are N independent API calls, never one atomic event (B3)
        };

        Ok(Self {
            id: "stripe".to_string(),
            config,
            http,
            capabilities,
            base_url: STRIPE_API_BASE.to_string(),
        })
    }

    /// Mirrors cackle's `Capabilities.SupportsCurrency`: an empty
    /// `currencies` config means unrestricted, matching cackle's own
    /// `Currencies: nil` for Stripe (see `config.rs`'s gap note).
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

    async fn do_form(
        &self,
        method: reqwest::Method,
        path: &str,
        form: Option<&[(String, String)]>,
        idempotency_key: Option<&str>,
    ) -> Result<(Vec<u8>, u16)> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .http
            .request(method, &url)
            .basic_auth(&self.config.secret_key, Option::<&str>::None);
        if let Some(form) = form {
            req = req.form(form);
        }
        if let Some(key) = idempotency_key {
            req = req.header("Idempotency-Key", key);
        }
        req = req.header("Accept", "application/json");

        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("stripe: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("stripe: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("stripe: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for StripeRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::redirect_url`], because on the `stripe` rail
    /// `destination` is not a payout address: it is the post-checkout return
    /// URL, sent as Stripe's `success_url`/`cancel_url` (see this module's docs above).
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
        models::stripe_amount(req.amount_minor, &req.currency)?;

        // NEEDS-CONFIRMATION (mirrors patala-hyperswitch's identical note):
        // Stripe's documented API has no pre-charge fee-quote endpoint
        // either, and cackle's own adapter has no Quote-equivalent method —
        // this never fabricates a fee it cannot obtain.
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
        let amount = models::stripe_amount(req.amount_minor, &currency)?;

        // See module docs: `destination` is reinterpreted as the
        // success_url/cancel_url Stripe Checkout requires (cackle's
        // `Order.CallbackURL`) -- there is no separate field for it in
        // `PayRequest`.
        let form: Vec<(String, String)> = vec![
            ("mode".into(), "payment".into()),
            ("client_reference_id".into(), req.reference.clone()),
            ("line_items[0][quantity]".into(), "1".into()),
            (
                "line_items[0][price_data][currency]".into(),
                currency.to_ascii_lowercase(),
            ),
            (
                "line_items[0][price_data][unit_amount]".into(),
                amount.to_string(),
            ),
            (
                "line_items[0][price_data][product_data][name]".into(),
                format!("patala order {}", req.reference),
            ),
            ("success_url".into(), req.destination.clone()),
            ("cancel_url".into(), req.destination.clone()),
            ("metadata[patala_reference]".into(), req.reference.clone()),
        ];

        let (body, status) = self
            .do_form(
                reqwest::Method::POST,
                "/checkout/sessions",
                Some(&form),
                Some(&format!("begin:{}", req.reference)),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }

        #[derive(Deserialize)]
        struct CreateResponse {
            id: String,
            url: String,
        }
        let parsed: CreateResponse =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.id.is_empty() || parsed.url.is_empty() {
            return Err(models::malformed("empty session id or url"));
        }

        let proof = ChargeProof {
            session_id: parsed.id,
            // Stripe's Checkout Session create response (as cackle's own
            // `Begin` decodes it: just {id, url}) does not include
            // `payment_status`. "open" is Stripe's own documented initial
            // status for a freshly created session -- a neutral placeholder
            // `verify()` never trusts, always re-fetching the true status.
            status_at_charge: "open".to_string(),
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

    async fn verify(&self, receipt: &Receipt) -> Result<bool> {
        // Fail closed on anything that doesn't even look like a receipt
        // this rail issued -- never assume valid.
        if receipt.rail_id != self.id {
            return Ok(false);
        }
        let Some(proof) = ChargeProof::from_bytes(&receipt.proof) else {
            return Ok(false);
        };
        let Ok(session_id) = safe_path_segment(&proof.session_id) else {
            return Ok(false);
        };

        let path = format!("/checkout/sessions/{session_id}");
        let (body, status) = self
            .do_form(reqwest::Method::GET, &path, None, None)
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let session: StripeSessionPayload =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        let Ok(outcome) = models::evaluate_session(&session) else {
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
        // See module docs: this whole method is NEW code grounded in
        // Stripe's public Refunds API, not a cackle port (cackle's Provider
        // interface has no Refund method at all).
        if receipt.rail_id != self.id {
            return Err(Error::InvalidRequest(format!(
                "receipt names rail {:?}, not {:?}",
                receipt.rail_id, self.id
            )));
        }
        let proof = ChargeProof::from_bytes(&receipt.proof).ok_or_else(|| {
            Error::InvalidRequest("receipt proof is not a stripe charge proof".into())
        })?;
        let session_id = safe_path_segment(&proof.session_id)?;

        // Refunding needs the PaymentIntent id, which cackle's own minimal
        // session response model never captured -- fetch the session fresh
        // (mirrors the same "verification must reflect reality" principle
        // `verify()` already uses).
        let path = format!("/checkout/sessions/{session_id}");
        let (body, status) = self
            .do_form(reqwest::Method::GET, &path, None, None)
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let session: StripeSessionPayload =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        let payment_intent = session
            .payment_intent
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                Error::Rail(
                    "stripe: session has no payment_intent to refund (payment not completed)"
                        .into(),
                )
            })?;

        let form = vec![("payment_intent".to_string(), payment_intent)];
        let (body, status) = self
            .do_form(reqwest::Method::POST, "/refunds", Some(&form), None)
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }

        #[derive(Deserialize)]
        struct RefundResponse {
            id: String,
            status: String,
            amount: u64,
            currency: String,
        }
        let parsed: RefundResponse =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        // Stripe's documented Refund object status values: "succeeded",
        // "pending", "failed", "canceled" (some redirect-based refund
        // methods also use "requires_action"). Only "succeeded" is ever
        // reported as money having moved -- everything else, including a
        // value this adapter does not recognise, is treated as not-yet-
        // settled, same fail-closed convention as `charge()`/`verify()`.
        let succeeded = parsed.status == "succeeded";
        let currency = parsed.currency.trim().to_ascii_uppercase();
        let amount_minor = if succeeded {
            models::stripe_amount_to_minor(parsed.amount, &currency)?
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

    /// Verify a Stripe webhook delivery — the push counterpart to
    /// [`Self::verify`]. The scheme (HMAC-SHA256 over `"{t}.{body}"`, `v1`
    /// only, 5-minute replay window) lives in
    /// [`crate::stripe::webhook::verify_and_parse`]; this method is what
    /// makes it reachable through `dyn PaymentRail`, and therefore through
    /// the UniFFI binding and the sidecar.
    ///
    /// Header: `Stripe-Signature`. Replay window is checked against
    /// [`WebhookDelivery::now_unix`].
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event = crate::stripe::webhook::verify_and_parse(
            &self.config.webhook_secret,
            &delivery.raw_body,
            delivery.header_or_empty("Stripe-Signature"),
            delivery.now_unix,
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

    fn config() -> StripeConfig {
        StripeConfig {
            secret_key: "sk_test_fake".to_string(),
            webhook_secret: "whsec_fake".to_string(),
            requires_kyc: true,
            currencies: Vec::new(),
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> StripeRail {
        let mut rail = StripeRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/stripe_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.reversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(rail.id(), "stripe");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.secret_key.clear();
        assert!(StripeRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.webhook_secret.clear();
        assert!(StripeRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn quote_never_fabricates_a_fee_and_refuses_three_decimal_currency() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let q = rail
            .quote(&req(5000, "USD", "https://example.com/return", "order-1"))
            .await
            .unwrap();
        assert_eq!(q.fee_minor, 0);
        assert_eq!(q.total_minor, 5000);

        assert!(rail
            .quote(&req(1000, "KWD", "https://example.com/return", "order-1"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn charge_posts_expected_shape() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkout/sessions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "cs_test_1",
                "url": "https://checkout.stripe.com/pay/cs_test_1"
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
        assert_eq!(proof.session_id, "cs_test_1");
        assert_eq!(
            proof.redirect_url.as_deref(),
            Some("https://checkout.stripe.com/pay/cs_test_1")
        );
    }

    #[tokio::test]
    async fn charge_refuses_three_decimal_currency_without_calling_server() {
        let server = MockServer::start().await;
        // No Mock registered -- if the adapter called the server anyway
        // wiremock would panic on an unexpected request.
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(1000, "KWD", "https://example.com/return", "ord_1"))
            .await
            .expect_err("three-decimal currency must be refused before any network call");
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn charge_http_500_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkout/sessions"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(json!({"error":{"message":"internal error"}})),
            )
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(5000, "USD", "https://example.com/return", "ord_1"))
            .await
            .unwrap_err();
        match err {
            Error::Rail(msg) => assert!(msg.contains("internal error")),
            other => panic!("expected Error::Rail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_settled_session_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checkout/sessions/cs_test_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "cs_test_1",
                "payment_status": "paid",
                "amount_total": 5000,
                "currency": "usd",
                "client_reference_id": "ord_1"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "stripe".into(),
            amount_minor: 0,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                session_id: "cs_test_1".into(),
                status_at_charge: "open".into(),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_pending_redirect_session_is_not_settled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checkout/sessions/cs_test_pending"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "cs_test_pending",
                "payment_status": "unpaid",
                "amount_total": 5000,
                "currency": "usd",
                "client_reference_id": "ord_1"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "stripe".into(),
            amount_minor: 0,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                session_id: "cs_test_pending".into(),
                status_at_charge: "open".into(),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(
            !rail.verify(&receipt).await.unwrap(),
            "a still-pending checkout session must not verify as settled"
        );
    }

    #[tokio::test]
    async fn verify_fails_closed_on_amount_or_currency_mismatch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checkout/sessions/cs_test_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "cs_test_1",
                "payment_status": "paid",
                "amount_total": 500,
                "currency": "usd",
                "client_reference_id": "ord_1"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let base_proof = ChargeProof {
            session_id: "cs_test_1".into(),
            status_at_charge: "open".into(),
            redirect_url: None,
        }
        .to_bytes();

        let genuine = Receipt {
            rail_id: "stripe".into(),
            amount_minor: 500,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: base_proof.clone(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&genuine).await.unwrap());

        let mut inflated = genuine.clone();
        inflated.amount_minor = 999_999;
        assert!(
            !rail.verify(&inflated).await.unwrap(),
            "receipt claiming more than settled must not verify"
        );

        let mut wrong_currency = genuine.clone();
        wrong_currency.currency = "ZAR".into();
        assert!(!rail.verify(&wrong_currency).await.unwrap());

        let mut wrong_rail = genuine.clone();
        wrong_rail.rail_id = "some-other-rail".into();
        assert!(!rail.verify(&wrong_rail).await.unwrap());

        let mut garbage = genuine.clone();
        garbage.proof = vec![1, 2, 3];
        assert!(!rail.verify(&garbage).await.unwrap());
    }

    #[tokio::test]
    async fn refund_posts_expected_shape_and_returns_new_receipt() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checkout/sessions/cs_test_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "cs_test_1",
                "payment_status": "paid",
                "amount_total": 5000,
                "currency": "usd",
                "client_reference_id": "ord_1",
                "payment_intent": "pi_abc123"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/refunds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "re_xyz",
                "status": "succeeded",
                "amount": 5000,
                "currency": "usd"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let original = Receipt {
            rail_id: "stripe".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                session_id: "cs_test_1".into(),
                status_at_charge: "open".into(),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };

        let refund_receipt = rail.refund(&original).await.unwrap();
        assert_eq!(refund_receipt.amount_minor, 5000);
        let proof = RefundProof::from_bytes(&refund_receipt.proof).unwrap();
        assert_eq!(proof.refund_id, "re_xyz");
    }

    #[tokio::test]
    async fn refund_reports_a_pending_refund_as_not_yet_moved() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checkout/sessions/cs_test_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "cs_test_1",
                "payment_status": "paid",
                "amount_total": 5000,
                "currency": "usd",
                "client_reference_id": "ord_1",
                "payment_intent": "pi_abc123"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/refunds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "re_pending",
                "status": "pending",
                "amount": 5000,
                "currency": "usd"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let original = Receipt {
            rail_id: "stripe".into(),
            amount_minor: 5000,
            currency: "USD".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                session_id: "cs_test_1".into(),
                status_at_charge: "open".into(),
                redirect_url: None,
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
