//! [`YocoRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/yoco.go` (`YocoProvider`).
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a Yoco Checkout, returns its hosted
//!   `redirectUrl`) maps to [`PaymentRail::charge`]. **Gap vs cackle**:
//!   Yoco's Checkouts API accepts OPTIONAL `successUrl`/`cancelUrl`/
//!   `failureUrl` (cackle only sets them `if o.CallbackURL != ""`).
//!   `PayRequest::destination` is reinterpreted as that callback/return
//!   URL, the same choice `stripe::StripeRail` makes — and here it is a
//!   genuine reinterpretation, not mere friction, since a redirect flow
//!   without a return URL is of limited practical use anyway.
//! - **Money**: Yoco's `amount` is already an integer minor unit (cents),
//!   identical to `PayRequest::amount_minor` — no conversion through
//!   [`crate::currency`] needed or attempted, mirroring cackle's own file
//!   header note exactly.
//! - **Genuine cackle quirk, preserved via the seam, not silently changed**
//!   (see `proof.rs`'s module docs): cackle's `Begin` returns
//!   `Charge.Reference = parsed.ID` — Yoco's OWN checkout id, not
//!   `o.Reference`. This port's `Receipt::reference` stays the CALLER's own
//!   reference; Yoco's real checkout id lives in `proof` instead, and
//!   `verify()` always looks it up from there — same pattern as
//!   `stripe::proof`/`iyzico::proof`.
//! - cackle's `Verify(reference)` maps to [`PaymentRail::verify`], keyed by
//!   the checkout id embedded in `proof`.
//! - cackle's `Webhook` is ported as the free function
//!   [`crate::yoco::webhook::verify_and_parse`] — see that module's docs;
//!   unlike `iyzico`/`payfast`, Yoco's Svix signature needs no network
//!   round trip, so this stays a pure function like `stripe`/`paystack`.
//! - `refund()`: **not implemented.** Cackle's `Capabilities().Refunds` is
//!   `false` for Yoco with no "supports it, not implemented here" comment
//!   — same reasoning as the other regional adapters in this batch.
//!   Returns the trait default (`Error::Unsupported`).

use async_trait::async_trait;
use serde::Deserialize;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result, Settlement,
};

use crate::yoco::config::YocoConfig;
use crate::yoco::models::{self, YocoCheckout};
use crate::yoco::proof::ChargeProof;

const YOCO_API_BASE: &str = "https://payments.yoco.com/api";

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
            "value {s:?} is not a safe URL path segment for a yoco checkout id"
        )));
    }
    Ok(s)
}

/// Mirrors cackle's `decodeYocoWebhookSecret`: strips the `whsec_` prefix
/// and base64-decodes the remainder.
fn decode_webhook_secret(whsec: &str) -> Result<Vec<u8>> {
    let payload = whsec.strip_prefix("whsec_").unwrap_or(whsec);
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|e| {
            Error::InvalidRequest(format!(
                "yoco: webhook_secret not valid base64 after whsec_ prefix: {e}"
            ))
        })
}

/// One `PaymentRail` talking to Yoco's Checkouts API. See module docs for
/// the full `Provider` -> `PaymentRail` mapping.
pub struct YocoRail {
    id: String,
    config: YocoConfig,
    webhook_secret: Vec<u8>,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl YocoRail {
    /// Build a rail from configuration. Fails if `secret_key` is empty, or
    /// `webhook_secret` is empty/not valid `whsec_<base64>`.
    pub fn new(config: YocoConfig) -> Result<Self> {
        if config.secret_key.trim().is_empty() {
            return Err(Error::InvalidRequest("secret_key must not be empty".into()));
        }
        if config.webhook_secret.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_secret must not be empty".into(),
            ));
        }
        let webhook_secret = decode_webhook_secret(&config.webhook_secret)?;

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building yoco http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Yoco (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: vec!["ZAR".to_string()], // hardcoded, matches cackle -- see config.rs
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "yoco".to_string(),
            config,
            webhook_secret,
            http,
            capabilities,
            base_url: YOCO_API_BASE.to_string(),
        })
    }

    /// The decoded webhook secret — exposed so a caller can pass it to
    /// [`crate::yoco::webhook::verify_and_parse`] without re-decoding
    /// `config.webhook_secret` itself.
    pub fn webhook_secret(&self) -> &[u8] {
        &self.webhook_secret
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
            .header("Content-Type", "application/json");
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("yoco: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("yoco: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("yoco: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for YocoRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        if !req.currency.eq_ignore_ascii_case("ZAR") {
            return Err(models::unsupported_currency(&req.currency));
        }

        // NEEDS-CONFIRMATION (mirrors every other rail's identical note):
        // Yoco's documented API has no pre-charge fee-quote endpoint, and
        // cackle's own adapter has no Quote-equivalent method either.
        Ok(Quote {
            rail_id: self.id.clone(),
            amount_minor: req.amount_minor,
            currency: "ZAR".to_string(),
            fee_minor: 0,
            total_minor: req.amount_minor,
            settlement: self.capabilities.settlement,
            expires_at_unix: now_unix().saturating_add(300),
        })
    }

    async fn charge(&self, req: &PayRequest) -> Result<Receipt> {
        req.validate()?;
        if !req.currency.eq_ignore_ascii_case("ZAR") {
            return Err(models::unsupported_currency(&req.currency));
        }
        // See module docs: `destination` is reinterpreted as the
        // successUrl/cancelUrl/failureUrl Yoco's Checkouts API accepts.
        let callback = req.destination.trim();

        let body = serde_json::json!({
            "amount": req.amount_minor,
            "currency": "ZAR",
            "metadata": { "reference": req.reference },
            "successUrl": callback,
            "cancelUrl": callback,
            "failureUrl": callback,
        });

        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/checkouts", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(Deserialize)]
        struct CreateResponse {
            #[serde(default)]
            id: String,
            #[serde(default, rename = "redirectUrl")]
            redirect_url: String,
        }
        let parsed: CreateResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.id.is_empty() || parsed.redirect_url.is_empty() {
            return Err(models::malformed("empty id or redirectUrl"));
        }

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see PORTING.md §5
            currency: "ZAR".to_string(),
            reference: req.reference.clone(), // the CALLER's own reference -- see proof.rs
            proof: ChargeProof {
                checkout_id: parsed.id,
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
        let Some(proof) = ChargeProof::from_bytes(&receipt.proof) else {
            return Ok(false);
        };
        let Ok(checkout_id) = safe_path_segment(&proof.checkout_id) else {
            return Ok(false);
        };

        let path = format!("/checkouts/{checkout_id}");
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let parsed: YocoCheckout =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        if !parsed.id.is_empty() && parsed.id != checkout_id {
            return Ok(false);
        }
        let Ok(outcome) = models::evaluate_checkout(&parsed) else {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(amount: u64, currency: &str, callback: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: callback.into(),
            reference: reference.into(),
        }
    }

    fn config() -> YocoConfig {
        use base64::Engine as _;
        YocoConfig {
            secret_key: "sk_test_fake".to_string(),
            webhook_secret: format!(
                "whsec_{}",
                base64::engine::general_purpose::STANDARD
                    .encode(b"0123456789abcdef0123456789abcdef")
            ),
            requires_kyc: true,
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> YocoRail {
        let mut rail = YocoRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/yoco_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(caps.currencies, vec!["ZAR".to_string()]);
        assert_eq!(rail.id(), "yoco");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.secret_key.clear();
        assert!(YocoRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.webhook_secret.clear();
        assert!(YocoRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.webhook_secret = "whsec_not-valid-base64!!!".to_string();
        assert!(YocoRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_rejects_non_zar() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let err = rail
            .charge(&req(1000, "USD", "https://example.com/return", "ord_1"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkouts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chk_abc",
                "redirectUrl": "https://pay.yoco.com/chk_abc"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(1000, "ZAR", "https://example.com/return", "ord_1"))
            .await
            .unwrap();
        assert_eq!(receipt.reference, "ord_1");
        assert_eq!(receipt.amount_minor, 0);
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.checkout_id, "chk_abc");
    }

    #[tokio::test]
    async fn verify_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checkouts/chk_abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chk_abc", "status": "completed", "amount": 1000, "currency": "ZAR"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "yoco".into(),
            amount_minor: 0,
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                checkout_id: "chk_abc".into(),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_garbage_proof_fails_closed() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "yoco".into(),
            amount_minor: 0,
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: vec![9, 9, 9],
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_provider_500_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checkouts/chk_abc"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({"message":"oops"})))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "yoco".into(),
            amount_minor: 0,
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                checkout_id: "chk_abc".into(),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "yoco".into(),
            amount_minor: 100,
            currency: "ZAR".into(),
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
