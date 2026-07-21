//! [`RazorpayRail`] — the `PaymentRail` implementation. Ported from
//! cackle's `internal/payments/razorpay.go` (`RazorpayProvider`).
//!
//! Reference: <https://razorpay.com/docs/api/orders/> (Create an Order),
//! <https://razorpay.com/docs/api/payments/fetch-payments-for-order>
//! (Verify). Not re-verified live from this environment — see this crate's
//! `PORTING.md` "UNVERIFIED AGAINST LIVE" note.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a Razorpay Order server-side, returns its
//!   id) maps to [`PaymentRail::charge`]. **`destination` is unused by this
//!   rail** — a genuine divergence from every other adapter in this crate,
//!   not an oversight: Razorpay's Standard Checkout is an INLINE flow
//!   (Checkout.js widget, rendered client-side by the caller with the
//!   returned order id + the non-secret `key_id`), and cackle's own `Begin`
//!   needs neither a callback URL nor a buyer email to create an order —
//!   only `amount`, `currency`, and `receipt`. `PayRequest::validate()`
//!   still requires `destination` to be non-empty (the uniform contract
//!   every rail in this crate shares), but `RazorpayRail::charge` never
//!   reads it — callers may pass any non-empty placeholder specifically for
//!   this rail.
//! - cackle's `Order.EventID` (optionally sent as `notes.event_id`) has no
//!   `PayRequest` equivalent and is simply never sent — documented
//!   info-loss.
//! - cackle's `Capabilities().Currencies = ["INR"]` is ported as a
//!   hardcoded, unconfigurable `capabilities.currencies` (see
//!   [`RazorpayRail::new`]'s doc comment) — but note cackle's own `Begin`
//!   does NOT actually enforce INR-only (no equality check there, unlike
//!   PayU); this port's standard `check_currency` gate (same helper shape
//!   `stripe`/`paystack` already use) enforces it anyway, for consistency
//!   with every rail in this crate — an intentional ADDED protection beyond
//!   cackle's literal `Begin` logic, not a regression.
//! - **`Receipt::reference` ambiguity, resolved the same way `stripe::rail`
//!   resolves its identical session-id-vs-caller-reference problem** (see
//!   `proof.rs`'s module docs, cited directly): cackle's `Begin` sets
//!   `Charge.Reference` to Razorpay's OWN generated order id, discarding
//!   the caller's original reference from the returned `Charge` — but
//!   `patala_core::Receipt::reference` is documented as "The
//!   `PayRequest::reference` this receipt fulfills," so this port keeps
//!   `Receipt::reference` ALWAYS equal to the caller's own
//!   `PayRequest::reference` and embeds Razorpay's real order id in `proof`
//!   instead, used only by `verify()`'s lookup.
//! - cackle's `Verify(reference)` (lists payments for a Razorpay order,
//!   returns the first captured one) maps to [`PaymentRail::verify`], keyed
//!   off `proof`'s embedded order id rather than `receipt.reference` — see
//!   above. **Deliberate, disclosed divergence from cackle** (`PORTING.md`
//!   §6): cackle returns `Err(ErrRazorpayNoCapturedPayment)` when no
//!   captured payment is found; this port returns `Ok(false)` instead
//!   (not-yet-settled is not an operational failure).
//! - cackle's `Webhook` is ported as the free function
//!   [`crate::razorpay::webhook::verify_and_parse`], NOT a trait method —
//!   same reasoning as every other adapter here.
//! - `refund()`: trait default (`Err(Error::Unsupported("refund"))`).
//!   Cackle's `RazorpayProvider.Capabilities().Refunds` is `false` with NO
//!   revealing "supports it, not implemented here"-style comment (unlike
//!   Paystack's) — so per `PORTING.md` §7's last bullet, this port does not
//!   fabricate a refund implementation cackle gives no evidence for, even
//!   though Razorpay does have a real-world public Refunds API.
//! - **Not ported**: cackle's amounts are plain integer minor units on the
//!   wire (paise), matching `PayRequest::amount_minor` directly — no
//!   `crate::currency` decimal-string conversion is needed anywhere in this
//!   file (unlike PayU/Xendit).

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result, Settlement,
};

use crate::razorpay::config::RazorpayConfig;
use crate::razorpay::models::{self, RazorpayPayment};
use crate::razorpay::proof::ChargeProof;

const RAZORPAY_API_BASE: &str = "https://api.razorpay.com/v1";

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
            "value {s:?} is not a safe URL path segment for a razorpay id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to Razorpay's Orders/Payments API. See module
/// docs for the full `Provider` -> `PaymentRail` mapping.
pub struct RazorpayRail {
    id: String,
    config: RazorpayConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl RazorpayRail {
    /// Build a rail from configuration. Fails if `key_id`, `key_secret`, or
    /// `webhook_secret` are empty.
    ///
    /// `capabilities.currencies` is hardcoded to `["INR"]`, unconditionally
    /// — see module docs for why this rail has no currency-list config/env
    /// override (cackle itself has no such mechanism here either).
    pub fn new(config: RazorpayConfig) -> Result<Self> {
        if config.key_id.trim().is_empty() {
            return Err(Error::InvalidRequest("key_id must not be empty".into()));
        }
        if config.key_secret.trim().is_empty() {
            return Err(Error::InvalidRequest("key_secret must not be empty".into()));
        }
        if config.webhook_secret.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_secret must not be empty".into(),
            ));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building razorpay http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: false, // mirrors cackle's Capabilities.Refunds: false
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Razorpay (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: vec!["INR".to_string()], // hardcoded -- see module docs
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "razorpay".to_string(),
            config,
            http,
            capabilities,
            base_url: RAZORPAY_API_BASE.to_string(),
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
            .basic_auth(&self.config.key_id, Some(&self.config.key_secret))
            .header("Content-Type", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("razorpay: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("razorpay: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("razorpay: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for RazorpayRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        // NEEDS-CONFIRMATION (mirrors every other rail's identical note):
        // Razorpay's documented API has no pre-charge fee-quote endpoint,
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
        // See module docs: `destination` is deliberately unused by this
        // rail -- Razorpay's inline Checkout.js flow needs none of
        // PayRequest's optional-field analogs.
        let currency = req.currency.trim().to_ascii_uppercase();

        let body = serde_json::json!({
            "amount": req.amount_minor,
            "currency": currency,
            "receipt": req.reference,
        });

        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/orders", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(serde::Deserialize)]
        struct CreateResponse {
            id: String,
        }
        let parsed: CreateResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.id.is_empty() {
            return Err(models::malformed("empty order id"));
        }

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see module docs
            currency,
            reference: req.reference.clone(), // ALWAYS the caller's own reference -- see proof.rs's module docs
            proof: ChargeProof {
                order_id: parsed.id,
            }
            .to_bytes(),
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
        let Ok(order_id) = safe_path_segment(&proof.order_id) else {
            return Ok(false);
        };

        let path = format!("/orders/{order_id}/payments");
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }

        #[derive(serde::Deserialize)]
        struct ListResponse {
            #[serde(default)]
            items: Vec<RazorpayPayment>,
        }
        let parsed: ListResponse =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;

        // Never trust an entry that disagrees on order_id -- mirrors
        // cackle's `if item.OrderID != reference { continue }`.
        let captured = parsed
            .items
            .iter()
            .filter(|item| item.order_id == order_id)
            .find(|item| item.status == "captured");

        let Some(pay) = captured else {
            // Deliberate divergence from cackle's ErrRazorpayNoCapturedPayment
            // (an Err there) -- see module docs: not-yet-settled is Ok(false).
            return Ok(false);
        };

        let Ok(outcome) = models::evaluate_payment(pay) else {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(amount: u64, currency: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: "unused-for-razorpay".into(),
            reference: reference.into(),
        }
    }

    fn config() -> RazorpayConfig {
        RazorpayConfig {
            key_id: "rzp_test_fake".to_string(),
            key_secret: "test_secret".to_string(),
            webhook_secret: "test-razorpay-webhook-secret".to_string(),
            requires_kyc: true,
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> RazorpayRail {
        let mut rail = RazorpayRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/razorpay_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(caps.currencies, vec!["INR".to_string()]);
        assert_eq!(rail.id(), "razorpay");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.key_id.clear();
        assert!(RazorpayRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.key_secret.clear();
        assert!(RazorpayRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.webhook_secret.clear();
        assert!(RazorpayRail::new(cfg).is_err());
    }

    // TestRazorpayBegin_Success
    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "order_abc123",
                "status": "created"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail.charge(&req(10000, "INR", "ord_1")).await.unwrap();
        assert_eq!(
            receipt.reference, "ord_1",
            "Receipt::reference always echoes the caller's own reference"
        );
        assert_eq!(receipt.amount_minor, 0, "nothing has settled yet");
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.order_id, "order_abc123");
    }

    // TestRazorpayBegin_ProviderErrorFailsClosed
    #[tokio::test]
    async fn charge_provider_error_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/orders"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({"error":{"description":"bad request"}})),
            )
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let err = rail.charge(&req(10000, "INR", "ord_1")).await.unwrap_err();
        match err {
            Error::Rail(msg) => assert!(msg.contains("bad request")),
            other => panic!("expected Error::Rail, got {other:?}"),
        }
    }

    // TestRazorpayVerify_Success
    #[tokio::test]
    async fn verify_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/orders/order_abc123/payments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"id":"pay_1","order_id":"order_abc123","amount":10000,"currency":"INR","status":"captured","created_at":1753000000}]
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "razorpay".into(),
            amount_minor: 10000,
            currency: "INR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "order_abc123".into(),
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    // TestRazorpayVerify_NoCapturedPaymentFailsClosed -- Ok(false), not Err.
    #[tokio::test]
    async fn verify_no_captured_payment_fails_closed_as_ok_false() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/orders/order_abc123/payments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"id":"pay_1","order_id":"order_abc123","amount":10000,"currency":"INR","status":"authorized"}]
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "razorpay".into(),
            amount_minor: 10000,
            currency: "INR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "order_abc123".into(),
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    // TestRazorpayVerify_MalformedJSONFailsClosed
    #[tokio::test]
    async fn verify_malformed_json_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/orders/order_abc123/payments"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "razorpay".into(),
            amount_minor: 10000,
            currency: "INR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "order_abc123".into(),
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    // TestRazorpayVerify_Provider500FailsClosed
    #[tokio::test]
    async fn verify_provider_500_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/orders/order_abc123/payments"))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(json!({"error":{"description":"oops"}})),
            )
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "razorpay".into(),
            amount_minor: 10000,
            currency: "INR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                order_id: "order_abc123".into(),
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    #[tokio::test]
    async fn verify_fails_closed_on_garbage_proof() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "razorpay".into(),
            amount_minor: 10000,
            currency: "INR".into(),
            reference: "ord_1".into(),
            proof: vec![1, 2, 3],
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "razorpay".into(),
            amount_minor: 100,
            currency: "INR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(matches!(
            rail.refund(&receipt).await.unwrap_err(),
            Error::Unsupported(_)
        ));
    }
}
