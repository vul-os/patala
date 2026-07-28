//! [`MidtransRail`] — the `PaymentRail` implementation. Ported from
//! cackle's `internal/payments/midtrans.go` (`MidtransProvider`).
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a Midtrans Snap transaction, returns its
//!   hosted `redirect_url`) maps to [`PaymentRail::charge`]. **Gap vs
//!   cackle**: Midtrans's Snap API accepts an OPTIONAL `customer_details`
//!   email, and cackle's own `Begin` only sends it `if o.BuyerEmail != ""`
//!   — no single `Order` field is strictly REQUIRED beyond amount/currency
//!   the way Stripe's callback URL or Paystack's email are. Since
//!   `patala_core::PayRequest::validate()` still requires `destination`
//!   non-empty for every rail, this port reinterprets it as the buyer's
//!   email (sent as `customer_details.email` when non-empty) — the closest
//!   available fit, but genuinely disclosed FRICTION, not a hidden
//!   requirement: a caller must supply *something* non-empty as
//!   `destination` even though Midtrans's own API could proceed without
//!   one.
//! - **Money quirk, ported exactly** — see `mod.rs`'s module docs on the
//!   IDR 2-decimal-but-whole-rupiah bridging, and the note below on
//!   `gross_amount`'s wire representation.
//! - **Disclosed implementation technique, not a Go-parity issue**: cackle's
//!   `mustParseJSONInt` wraps the already-formatted decimal string
//!   (`minorToMajorString`'s output, e.g. `"10000.00"`) as Go's
//!   `json.Number`, which `encoding/json` marshals as a BARE (unquoted)
//!   number token, byte-for-byte identical to the string. This port does
//!   the same using `serde_json::value::RawValue` (see `Cargo.toml`'s
//!   comment on why `serde_json`'s `raw_value` feature is enabled crate-
//!   wide) rather than a lossy round-trip through `f64` (which could
//!   collapse a trailing `.00`/`.50` on re-serialization) — the amount
//!   itself is never parsed as a float anywhere in this file; the decimal
//!   string produced by [`crate::currency::minor_to_major_string`] is
//!   embedded verbatim.
//! - cackle's `Verify(reference)` (`GET /{order_id}/status` against the
//!   Core API) maps to [`PaymentRail::verify`], keyed on `Receipt::reference`
//!   directly — Midtrans's own tracking key IS `order_id`, which cackle's
//!   `Begin` always sets to the caller's own reference (see `proof.rs`).
//! - cackle's `Webhook` maps to [`PaymentRail::verify_webhook`], which
//!   delegates to the free function
//!   [`crate::midtrans::webhook::verify_and_parse`]. The function keeps the
//!   pure, directly-testable shape; the trait method is what a consumer
//!   dispatching through `dyn PaymentRail` — the UniFFI binding, the
//!   sidecar — can actually reach.
//! - `refund()`: **not implemented.** Cackle's `Capabilities().Refunds` is
//!   `false` for Midtrans with no "supports it, not implemented here"
//!   comment — same reasoning as `flutterwave::rail`/`iyzico::rail`.
//!   Returns the trait default (`Error::Unsupported`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::midtrans::config::{MidtransConfig, CORE_API_BASE, SNAP_API_BASE};
use crate::midtrans::models::{self, MidtransTransactionStatus};
use crate::midtrans::proof::ChargeProof;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Mirrors `stripe::rail`/`paystack::rail`'s identical `safe_path_segment`.
fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for a midtrans order_id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to Midtrans's Snap (charge) + Core (status)
/// APIs. See module docs for the full `Provider` -> `PaymentRail` mapping.
pub struct MidtransRail {
    id: String,
    config: MidtransConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    snap_base_url: String, // overridable in tests only
    core_base_url: String, // overridable in tests only
}

impl MidtransRail {
    /// Build a rail from configuration. Fails if `server_key` is empty.
    pub fn new(config: MidtransConfig) -> Result<Self> {
        if config.server_key.trim().is_empty() {
            return Err(Error::InvalidRequest("server_key must not be empty".into()));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building midtrans http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Midtrans (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: vec!["IDR".to_string()], // hardcoded, matches cackle -- see config.rs
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "midtrans".to_string(),
            config,
            http,
            capabilities,
            snap_base_url: SNAP_API_BASE.to_string(),
            core_base_url: CORE_API_BASE.to_string(),
        })
    }

    async fn do_request(
        &self,
        base_url: &str,
        method: reqwest::Method,
        path: &str,
        body_bytes: Option<&[u8]>,
    ) -> Result<(Vec<u8>, u16)> {
        let url = format!("{base_url}{path}");
        let mut req = self
            .http
            .request(method, &url)
            .basic_auth(&self.config.server_key, Option::<&str>::None)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        if let Some(b) = body_bytes {
            req = req.body(b.to_vec());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("midtrans: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("midtrans: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("midtrans: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for MidtransRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        if !req.currency.eq_ignore_ascii_case("IDR") {
            return Err(models::unsupported_currency(&req.currency));
        }
        crate::currency::minor_to_major_string(req.amount_minor, "IDR")
            .map_err(|e| Error::InvalidRequest(e.to_string()))?;

        // NEEDS-CONFIRMATION (mirrors every other rail's identical note):
        // Midtrans's documented API has no pre-charge fee-quote endpoint,
        // and cackle's own adapter has no Quote-equivalent method either.
        Ok(Quote {
            rail_id: self.id.clone(),
            amount_minor: req.amount_minor,
            currency: "IDR".to_string(),
            fee_minor: 0,
            total_minor: req.amount_minor,
            settlement: self.capabilities.settlement,
            expires_at_unix: now_unix().saturating_add(300),
        })
    }

    async fn charge(&self, req: &PayRequest) -> Result<Receipt> {
        req.validate()?;
        if !req.currency.eq_ignore_ascii_case("IDR") {
            return Err(models::unsupported_currency(&req.currency));
        }
        let gross_amount_str = crate::currency::minor_to_major_string(req.amount_minor, "IDR")
            .map_err(|e| Error::InvalidRequest(e.to_string()))?;
        let email = req.destination.trim();

        let raw_amount = serde_json::value::RawValue::from_string(gross_amount_str)
            .map_err(|e| Error::Rail(format!("midtrans: encode gross_amount: {e}")))?;

        #[derive(Serialize)]
        struct TransactionDetails<'a> {
            order_id: &'a str,
            gross_amount: &'a serde_json::value::RawValue,
        }
        #[derive(Serialize)]
        struct CustomerDetails<'a> {
            email: &'a str,
            first_name: &'a str,
        }
        #[derive(Serialize)]
        struct BeginRequest<'a> {
            transaction_details: TransactionDetails<'a>,
            #[serde(skip_serializing_if = "Option::is_none")]
            customer_details: Option<CustomerDetails<'a>>,
        }
        let body = BeginRequest {
            transaction_details: TransactionDetails {
                order_id: &req.reference,
                gross_amount: &raw_amount,
            },
            customer_details: if email.is_empty() {
                None
            } else {
                Some(CustomerDetails {
                    email,
                    first_name: "",
                })
            },
        };
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| Error::Rail(format!("midtrans: encode request: {e}")))?;

        let (resp_body, status) = self
            .do_request(
                &self.snap_base_url,
                reqwest::Method::POST,
                "/transactions",
                Some(&body_bytes),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(Deserialize)]
        struct CreateResponse {
            #[serde(default)]
            token: String,
            #[serde(default)]
            redirect_url: String,
        }
        let parsed: CreateResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.redirect_url.is_empty() {
            return Err(models::malformed("empty redirect_url"));
        }
        let _ = parsed.token; // present on the wire but unused, mirrors cackle's own dead field

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see PORTING.md §5
            currency: "IDR".to_string(),
            reference: req.reference.clone(),
            proof: ChargeProof {
                redirect_url: Some(parsed.redirect_url),
            }
            .to_bytes(),
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

        let path = format!("/{reference}/status");
        let (body, status) = self
            .do_request(&self.core_base_url, reqwest::Method::GET, &path, None)
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let parsed: MidtransTransactionStatus =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        if !parsed.order_id.is_empty() && parsed.order_id != reference {
            return Ok(false);
        }
        let Ok(outcome) = models::evaluate_status(&parsed) else {
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

    /// Verify a Midtrans notification — see
    /// [`crate::midtrans::webhook::verify_and_parse`]. Midtrans's
    /// `signature_key` is an unkeyed SHA-512 over
    /// `order_id + status_code + gross_amount + server_key`, carried in the
    /// JSON body rather than a header, so this method reads no header at
    /// all.
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event =
            crate::midtrans::webhook::verify_and_parse(&self.config.server_key, &delivery.raw_body)
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

    fn config() -> MidtransConfig {
        MidtransConfig {
            server_key: "SB-Mid-server-fake-key".to_string(),
            requires_kyc: true,
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> MidtransRail {
        let mut rail = MidtransRail::new(config()).unwrap();
        rail.snap_base_url = base_url.clone();
        rail.core_base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/midtrans_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(caps.currencies, vec!["IDR".to_string()]);
        assert_eq!(rail.id(), "midtrans");
    }

    #[test]
    fn new_rejects_empty_server_key() {
        let mut cfg = config();
        cfg.server_key.clear();
        assert!(MidtransRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_rejects_non_idr() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let err = rail
            .charge(&req(10000, "USD", "a@b.com", "ord_1"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token": "tok_abc",
                "redirect_url": "https://app.midtrans.com/snap/v3/redirection/tok_abc"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(10000, "IDR", "a@b.com", "ord_1"))
            .await
            .unwrap();
        assert_eq!(receipt.amount_minor, 0);
        assert_eq!(receipt.reference, "ord_1");
    }

    #[tokio::test]
    async fn verify_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ord_1/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "order_id": "ord_1", "transaction_id": "txn_1", "transaction_status": "settlement",
                "gross_amount": "10000.00", "currency": "IDR", "status_code": "200",
                "settlement_time": "2026-07-20 10:00:00"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "midtrans".into(),
            amount_minor: 0,
            currency: "IDR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_capture_without_fraud_accept_is_not_settled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ord_1/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "order_id": "ord_1", "transaction_id": "txn_1", "transaction_status": "capture",
                "fraud_status": "challenge", "gross_amount": "10000.00", "currency": "IDR"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "midtrans".into(),
            amount_minor: 0,
            currency: "IDR".into(),
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
            .and(path("/ord_1/status"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "midtrans".into(),
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
            .and(path("/ord_1/status"))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(json!({"status_message":"error"})),
            )
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "midtrans".into(),
            amount_minor: 0,
            currency: "IDR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "midtrans".into(),
            amount_minor: 100,
            currency: "IDR".into(),
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
