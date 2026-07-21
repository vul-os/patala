//! [`PaystackRail`] — the `PaymentRail` implementation. Ported from
//! cackle's `internal/payments/paystack.go` (`PaystackProvider`).
//!
//! Reference: <https://paystack.com/docs/api/transaction/> (Initialize,
//! Verify). Not re-verified live from this environment — see this crate's
//! `PORTING.md` "UNVERIFIED AGAINST LIVE" note.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (initializes a Paystack transaction, returns its
//!   hosted payment page URL) maps to [`PaymentRail::charge`]. **Gap vs
//!   cackle** (flagged in `PORTING.md`): Paystack's Initialize API REQUIRES
//!   a buyer email (cackle: `"payments: paystack: buyer email is
//!   required"`), and `patala_core::PayRequest` has no buyer-contact field
//!   at all. This port reinterprets `PayRequest::destination` (documented
//!   as "an opaque processor-side destination token") AS the buyer's email
//!   address for this rail specifically — a DIFFERENT reinterpretation than
//!   `stripe::StripeRail` gives the same field (there, it's the callback
//!   URL), which is fine: `patala_core`'s own docs say `destination` is
//!   opaque and rail-specific, "only the rail that receives it knows
//!   which". Callers of `PaystackRail::charge` must pass the buyer's email
//!   as `destination`.
//! - cackle's `Order.CallbackURL` (optional for Paystack, unlike Stripe) has
//!   no `PayRequest` equivalent either and is simply never sent — Paystack
//!   falls back to its own default post-payment page, which cackle's own
//!   adapter already tolerates (it only sets `callback_url` "if
//!   `o.CallbackURL != ""`").
//! - cackle's `Order.Currency == ""` -> default `"ZAR"` fallback in `Begin`
//!   is NOT ported: `PayRequest::validate()` already rejects an empty
//!   `currency` before `charge()` runs, so that branch is structurally
//!   unreachable here (a currency is always present by the time this rail
//!   sees the request).
//! - cackle's `Verify(reference)` maps directly to [`PaymentRail::verify`].
//!   **Genuine divergence, not an inconsistency** (see `proof.rs`'s module
//!   docs): Paystack's own transaction `reference` IS the caller's
//!   reference (no separate provider-assigned id the way a Stripe session
//!   id is), so `verify()`/`refund()` use `Receipt::reference` directly
//!   rather than anything decoded from `proof`.
//! - cackle's `Webhook` is ported as the free function
//!   [`crate::paystack::webhook::verify_and_parse`], not a trait method —
//!   same reasoning as `stripe::webhook` and `manual.rs`.
//! - `refund()`: **NOT a cackle port**, same as `stripe::rail::StripeRail`'s
//!   `refund()` — cackle's `PaystackProvider.Capabilities().Refunds` is
//!   `false` with an explicit comment ("Paystack supports it; not
//!   implemented here"). This method is NEW code grounded directly in
//!   Paystack's own public Refund API
//!   (<https://paystack.com/docs/api/refund/>), same honesty conventions as
//!   every ported method here.
//! - **Not ported**: cackle's `ListBanks`/`CreateRecipient` (payout/transfer-
//!   recipient management — `internal/payments/paystack.go`). These are
//!   organiser PAYOUT features, not part of the `quote`/`charge`/`verify`/
//!   `refund` seam this task is porting — out of scope, see `PORTING.md`.

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result, Settlement,
};

use crate::paystack::config::PaystackConfig;
use crate::paystack::models::{self, InitializeResponse, VerifyResponse};
use crate::paystack::proof::ChargeProof;

const PAYSTACK_API_BASE: &str = "https://api.paystack.co";

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Mirrors `patala-hyperswitch`/`stripe::rail`'s identical
/// `safe_path_segment` helper.
fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for a paystack reference"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to Paystack's Transaction API. See module docs
/// for the full `Provider` -> `PaymentRail` mapping.
pub struct PaystackRail {
    id: String,
    config: PaystackConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl PaystackRail {
    /// Build a rail from configuration. Fails if `secret_key` or
    /// `currencies` are empty.
    pub fn new(config: PaystackConfig) -> Result<Self> {
        if config.secret_key.trim().is_empty() {
            return Err(Error::InvalidRequest("secret_key must not be empty".into()));
        }
        if config.currencies.is_empty() {
            return Err(Error::InvalidRequest("currencies must not be empty".into()));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building paystack http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Paystack (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "paystack".to_string(),
            config,
            http,
            capabilities,
            base_url: PAYSTACK_API_BASE.to_string(),
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
                format!("Bearer {}", self.config.secret_key),
            )
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("paystack: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("paystack: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("paystack: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for PaystackRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        // NEEDS-CONFIRMATION (mirrors stripe/hyperswitch's identical note):
        // Paystack's documented API has no pre-charge fee-quote endpoint,
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
        // email address, which Paystack's Initialize API requires.
        let email = req.destination.trim();
        if email.is_empty() {
            return Err(Error::InvalidRequest(
                "paystack: destination (used as the buyer email) is required".into(),
            ));
        }
        let currency = req.currency.trim().to_ascii_uppercase();

        let body = serde_json::json!({
            "email": email,
            "amount": req.amount_minor,
            "reference": req.reference,
            "currency": currency,
        });

        let (resp_body, status) = self
            .do_json(
                reqwest::Method::POST,
                "/transaction/initialize",
                Some(&body),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        let parsed: InitializeResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if !parsed.status || parsed.data.authorization_url.is_empty() {
            return Err(models::malformed("status=false or empty authorization_url"));
        }

        let reference = if parsed.data.reference.is_empty() {
            req.reference.clone()
        } else {
            parsed.data.reference.clone()
        };

        let proof = ChargeProof {
            authorization_url: parsed.data.authorization_url,
            access_code: parsed.data.access_code,
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see module docs
            currency,
            reference,
            proof: proof.to_bytes(),
            settled_at_unix: 0,
        })
    }

    async fn verify(&self, receipt: &Receipt) -> Result<bool> {
        if receipt.rail_id != self.id {
            return Ok(false);
        }
        let Ok(reference) = safe_path_segment(&receipt.reference) else {
            return Ok(false);
        };

        let path = format!("/transaction/verify/{reference}");
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let parsed: VerifyResponse =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        if !parsed.status {
            return Ok(false);
        }
        // Fail closed on a reference Paystack claims differs from the one
        // requested -- mirrors cackle's `Verify` reference-mismatch check.
        if !parsed.data.reference.is_empty() && parsed.data.reference != reference {
            return Ok(false);
        }
        if !models::is_settled_success(&parsed.data.status) {
            return Ok(false);
        }
        if !parsed.data.currency.eq_ignore_ascii_case(&receipt.currency) {
            return Ok(false);
        }
        if parsed.data.amount < receipt.amount_minor {
            return Ok(false);
        }
        Ok(true)
    }

    async fn refund(&self, receipt: &Receipt) -> Result<Receipt> {
        // See module docs: this whole method is NEW code grounded in
        // Paystack's public Refund API, not a cackle port (cackle's own
        // adapter has Capabilities.Refunds: false / "not implemented here").
        if receipt.rail_id != self.id {
            return Err(Error::InvalidRequest(format!(
                "receipt names rail {:?}, not {:?}",
                receipt.rail_id, self.id
            )));
        }
        let reference = safe_path_segment(&receipt.reference)?;

        let body = serde_json::json!({
            "transaction": reference,
            "amount": receipt.amount_minor,
        });
        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/refund", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(serde::Deserialize, Default)]
        struct RefundData {
            #[serde(default)]
            amount: u64,
            #[serde(default)]
            currency: String,
            #[serde(default)]
            status: String,
        }
        #[derive(serde::Deserialize)]
        struct RefundResponse {
            status: bool,
            #[serde(default)]
            data: RefundData,
        }

        let parsed: RefundResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if !parsed.status {
            return Err(models::malformed("refund request returned status=false"));
        }

        // Paystack's documented refund `data.status` values include
        // "processed", "pending", "failed" (NEEDS-CONFIRMATION: not
        // re-verified live from this environment, see PORTING.md). Only
        // "processed" is ever reported as money having actually moved back
        // -- same fail-closed convention as every other rail in this crate.
        let succeeded = parsed.data.status == "processed";
        let currency = parsed.data.currency.trim().to_ascii_uppercase();

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: if succeeded { parsed.data.amount } else { 0 },
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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(amount: u64, currency: &str, email: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: email.into(),
            reference: reference.into(),
        }
    }

    fn config() -> PaystackConfig {
        PaystackConfig {
            secret_key: "sk_test_fake_secret_for_unit_tests".to_string(),
            requires_kyc: true,
            currencies: vec![
                "NGN".into(),
                "GHS".into(),
                "ZAR".into(),
                "KES".into(),
                "USD".into(),
            ],
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> PaystackRail {
        let mut rail = PaystackRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/paystack_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(rail.id(), "paystack");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.secret_key.clear();
        assert!(PaystackRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.currencies.clear();
        assert!(PaystackRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transaction/initialize"))
            .and(header("Authorization", "Bearer sk_test_fake_secret_for_unit_tests"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": true,
                "message": "ok",
                "data": {"authorization_url": "https://checkout.paystack.com/abc", "access_code": "abc", "reference": "ord_1"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(5000, "ZAR", "a@b.com", "ord_1"))
            .await
            .unwrap();
        assert_eq!(receipt.reference, "ord_1");
        assert_eq!(
            receipt.amount_minor, 0,
            "nothing has settled yet at charge time"
        );
    }

    #[tokio::test]
    async fn charge_requires_email() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let err = rail
            .charge(&req(5000, "ZAR", "", "ord_1"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn charge_provider_error_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transaction/initialize"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({"status": false, "message": "Invalid key"})),
            )
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(5000, "ZAR", "a@b.com", "ord_1"))
            .await
            .unwrap_err();
        match err {
            Error::Rail(msg) => assert!(msg.contains("Invalid key")),
            other => panic!("expected Error::Rail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/transaction/verify/ord_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": true,
                "message": "ok",
                "data": {"id": 9001, "status": "success", "reference": "ord_1", "amount": 5000, "currency": "ZAR", "paid_at": "2026-07-20T10:00:00.000Z"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "paystack".into(),
            amount_minor: 0,
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_abandoned_is_not_settled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/transaction/verify/ord_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": true,
                "data": {"id": 1, "status": "abandoned", "reference": "ord_1", "amount": 5000, "currency": "ZAR"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "paystack".into(),
            amount_minor: 0,
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_unknown_status_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/transaction/verify/ord_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": true,
                "data": {"id": 1, "status": "some-new-paystack-status", "reference": "ord_1", "amount": 5000, "currency": "ZAR"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "paystack".into(),
            amount_minor: 0,
            currency: "ZAR".into(),
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
            .and(path("/transaction/verify/ord_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": true,
                "data": {"id": 1, "status": "success", "reference": "ord_1", "amount": 500, "currency": "ZAR"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let genuine = Receipt {
            rail_id: "paystack".into(),
            amount_minor: 500,
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&genuine).await.unwrap());

        let mut inflated = genuine.clone();
        inflated.amount_minor = 999_999;
        assert!(!rail.verify(&inflated).await.unwrap());

        let mut wrong_currency = genuine.clone();
        wrong_currency.currency = "NGN".into();
        assert!(!rail.verify(&wrong_currency).await.unwrap());
    }

    #[tokio::test]
    async fn verify_provider_500_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/transaction/verify/ord_1"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(json!({"status": false, "message": "internal error"})),
            )
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "paystack".into(),
            amount_minor: 500,
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    #[tokio::test]
    async fn refund_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refund"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": true,
                "message": "ok",
                "data": {"amount": 5000, "currency": "ZAR", "status": "processed"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let original = Receipt {
            rail_id: "paystack".into(),
            amount_minor: 5000,
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        let refund_receipt = rail.refund(&original).await.unwrap();
        assert_eq!(refund_receipt.amount_minor, 5000);
        assert_eq!(refund_receipt.currency, "ZAR");
    }

    #[tokio::test]
    async fn refund_pending_is_not_yet_moved() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refund"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": true,
                "data": {"amount": 5000, "currency": "ZAR", "status": "pending"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let original = Receipt {
            rail_id: "paystack".into(),
            amount_minor: 5000,
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
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
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.refund(&foreign).await.is_err());
    }
}
