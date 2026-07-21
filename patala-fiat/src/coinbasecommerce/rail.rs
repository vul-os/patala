//! [`CoinbaseCommerceRail`] — the `PaymentRail` implementation. Ported from
//! cackle's `internal/payments/coinbasecommerce.go`
//! (`CoinbaseCommerceProvider`).
//!
//! Reference: <https://docs.cloud.coinbase.com/commerce/reference>. Not
//! re-verified live from this environment — see this crate's `PORTING.md`
//! "UNVERIFIED AGAINST LIVE" note. Cackle's own file doc comment also flags
//! that Coinbase Commerce stopped onboarding new merchants at points in its
//! history — confirm it is still available before depending on it.
//!
//! ## `RailClass`/`holds_funds` — what was chosen and why
//!
//! `RailClass::CustodialReversible`, `holds_funds: true` — **identical
//! reasoning to `opennode::rail`'s**, and the same deliberate divergence
//! from `btcpay`/`lnbits`. Cackle's own file doc comment places this
//! adapter at "same tier as opennode.go... Coinbase Commerce briefly
//! touches funds before paying the organiser's own Coinbase/bank account
//! out." See `opennode::rail`'s module docs for the full reasoning, which
//! applies here unchanged.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a Coinbase Commerce charge, returns its
//!   hosted checkout URL) maps to [`PaymentRail::charge`]. Cackle's
//!   `Order.CallbackURL` is optional here (`if o.CallbackURL != ""
//!   redirect_url`) — not required the way Stripe's is — so (like
//!   `btcpay`/`lnbits`/`opennode`) this port does NOT reinterpret
//!   `PayRequest::destination` as anything adapter-specific (mirrors
//!   `manual.rs`'s precedent). `Order.EventID`/`OrgID` map to
//!   `metadata.event_id`/`metadata.org_id` in cackle but have no
//!   `PayRequest` equivalent and are simply not sent.
//! - cackle's `Verify(reference)` maps to [`PaymentRail::verify`]. **Same
//!   structural gap as `opennode::proof`'s**: the Coinbase Commerce charge
//!   id lives in `proof` (see `proof::ChargeProof`), never
//!   `receipt.reference`. The charge-fetch call and the LATEST-timeline-
//!   entry status mapping (`models::classify_charge_state`) are
//!   byte-for-byte the same as cackle's `coinbaseCommerceResultFromCharge`,
//!   INCLUDING its choice to fail with an actual `Err` (not merely
//!   `Ok(false)`) for `UNRESOLVED`/`RESOLVED` (this is where
//!   under/overpayment surfaces — cackle's own comment: *"a distinct,
//!   fail-closed condition... FLAGGED for a human to check the Coinbase
//!   Commerce dashboard, never silently accepted as paid and never silently
//!   discarded as an ordinary failure either"*) AND for a truly
//!   unrecognised timeline status (cackle errors here too, unlike
//!   `opennode`'s equivalent branch, which maps an unrecognised status to
//!   the ordinary "not paid" `Failed` case — this genuine per-adapter
//!   difference in cackle is preserved, not smoothed over).
//! - cackle's `Webhook` is ported as the free function
//!   [`crate::coinbasecommerce::webhook::verify_and_extract`] — see that
//!   module's docs for why it does not itself refetch (same pattern as
//!   `btcpay`/`lnbits`/`opennode`).
//! - `refund()`: **left as the trait default (`Error::Unsupported`)**, same
//!   reasoning as `opennode::rail`'s — cackle's `Capabilities.Refunds:
//!   false` has no "supports it, not implemented" signal here either.

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result, Settlement,
};

use crate::coinbasecommerce::config::CoinbaseCommerceConfig;
use crate::coinbasecommerce::models::{self, Charge, ChargeState, Envelope};
use crate::coinbasecommerce::proof::ChargeProof;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for a coinbasecommerce charge id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to Coinbase Commerce's hosted checkout API.
/// See module docs for the full `Provider` -> `PaymentRail` mapping and the
/// `RailClass`/`holds_funds` reasoning.
pub struct CoinbaseCommerceRail {
    id: String,
    config: CoinbaseCommerceConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl CoinbaseCommerceRail {
    /// Build a rail from configuration. Fails if `api_key` or
    /// `webhook_secret` are empty.
    pub fn new(config: CoinbaseCommerceConfig) -> Result<Self> {
        if config.api_key.trim().is_empty() {
            return Err(Error::InvalidRequest("api_key must not be empty".into()));
        }
        if config.webhook_secret.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_secret must not be empty".into(),
            ));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| {
                Error::Rail(format!("failed building coinbasecommerce http client: {e}"))
            })?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: false, // cackle: Refunds: false, no "supports it" signal. See module docs.
            requires_kyc: config.requires_kyc,
            holds_funds: true, // Coinbase Commerce (the PROCESSOR) briefly custodies -- never patala.
            currencies: config.currencies.clone(),
            settlement: Settlement::Instant,
        };

        let base_url = config.base_url.clone();
        Ok(Self {
            id: "coinbasecommerce".to_string(),
            config,
            http,
            capabilities,
            base_url,
        })
    }

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
            .header("X-CC-Api-Key", &self.config.api_key)
            .header("X-CC-Version", crate::coinbasecommerce::config::API_VERSION)
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("coinbasecommerce: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(|e| {
            Error::Rail(format!(
                "coinbasecommerce: failed reading response body: {e}"
            ))
        })?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("coinbasecommerce: {e}")))?;
        Ok((bytes.to_vec(), status))
    }

    async fn fetch_charge(&self, charge_id: &str) -> Result<Charge> {
        let charge_id = safe_path_segment(charge_id)?;
        let path = format!("/charges/{charge_id}");
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let envelope: Envelope =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        if envelope.data.id.is_empty() {
            return Err(models::malformed("empty charge id"));
        }
        Ok(envelope.data)
    }
}

#[async_trait]
impl PaymentRail for CoinbaseCommerceRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
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
        // See module docs: `destination` is unused by this rail.
        let currency = req.currency.trim().to_ascii_uppercase();
        let amount_str = crate::currency::minor_to_major_string(req.amount_minor, &currency)
            .map_err(|e| Error::InvalidRequest(format!("coinbasecommerce: {e}")))?;

        let body = serde_json::json!({
            "name": format!("patala order {}", req.reference),
            "description": format!("patala order {}", req.reference),
            "pricing_type": "fixed_price",
            "local_price": {"amount": amount_str, "currency": currency},
            "metadata": {"order_id": req.reference},
        });

        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/charges", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }
        let envelope: Envelope =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if envelope.data.id.is_empty() || envelope.data.hosted_url.is_empty() {
            return Err(models::malformed("empty id or hosted_url"));
        }

        let proof = ChargeProof {
            charge_id: envelope.data.id,
            hosted_url: envelope.data.hosted_url,
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see PORTING.md §5
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

        let charge = self.fetch_charge(&proof.charge_id).await?;
        let Some(latest) = charge.timeline.last() else {
            return Err(models::malformed("empty timeline"));
        };

        match models::classify_charge_state(latest) {
            ChargeState::Paid => {
                let amount_minor = crate::currency::major_string_to_minor(
                    &charge.pricing.local.amount,
                    &charge.pricing.local.currency,
                )
                .map_err(|e| models::malformed(&e.to_string()))?;
                if !charge
                    .pricing
                    .local
                    .currency
                    .eq_ignore_ascii_case(&receipt.currency)
                {
                    return Ok(false);
                }
                if amount_minor < receipt.amount_minor {
                    return Ok(false);
                }
                Ok(true)
            }
            ChargeState::Pending | ChargeState::Failed => Ok(false),
            ChargeState::RequiresManualReview(context) => {
                let ctx = context.unwrap_or_default();
                Err(Error::Rail(format!(
                    "coinbasecommerce: charge requires manual review in the Coinbase Commerce \
                     dashboard (unresolved/resolved timeline state) -- this adapter will not \
                     guess whether it was paid in full: context={ctx:?}"
                )))
            }
            ChargeState::Unrecognised(status) => Err(Error::Rail(format!(
                "coinbasecommerce: unrecognised timeline status {status:?}"
            ))),
        }
    }

    // refund(): left as the trait default (Error::Unsupported). See module
    // docs for why this is honest rather than a fabricated implementation.
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(amount: u64, currency: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: "unused-for-coinbasecommerce".into(),
            reference: reference.into(),
        }
    }

    fn config() -> CoinbaseCommerceConfig {
        CoinbaseCommerceConfig {
            api_key: "test-api-key".to_string(),
            webhook_secret: "test-webhook-secret".to_string(),
            base_url: "http://unused".to_string(),
            requires_kyc: false,
            currencies: Vec::new(),
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> CoinbaseCommerceRail {
        let mut cfg = config();
        cfg.base_url = base_url;
        CoinbaseCommerceRail::new(cfg).unwrap()
    }

    // Ported from cackle's internal/payments/coinbasecommerce_test.go.

    #[test]
    fn capabilities_reflect_hosted_custodial_model() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds);
        assert!(!caps.reversible);
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.api_key.clear();
        assert!(CoinbaseCommerceRail::new(cfg).is_err());
        let mut cfg = config();
        cfg.webhook_secret.clear();
        assert!(CoinbaseCommerceRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/charges"))
            .and(header("X-CC-Api-Key", "test-api-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "charge_1",
                    "hosted_url": "https://commerce.coinbase.com/charges/charge_1",
                    "timeline": [{"time": "2024-01-01T00:00:00Z", "status": "NEW"}],
                    "pricing": {"local": {"amount": "12.34", "currency": "USD"}}
                }
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail.charge(&req(1234, "USD", "order_1")).await.unwrap();
        assert_eq!(receipt.reference, "order_1");
        assert_eq!(receipt.amount_minor, 0);
    }

    #[tokio::test]
    async fn charge_rejects_non_positive_amount() {
        let rail = rail_for("http://127.0.0.1:1".into());
        assert!(rail.charge(&req(0, "USD", "order_1")).await.is_err());
    }

    fn receipt_with(amount_minor: u64, currency: &str, charge_id: &str) -> Receipt {
        Receipt {
            rail_id: "coinbasecommerce".into(),
            amount_minor,
            currency: currency.into(),
            reference: "order_1".into(),
            proof: ChargeProof {
                charge_id: charge_id.into(),
                hosted_url: "https://commerce.coinbase.com/charges/x".into(),
            }
            .to_bytes(),
            settled_at_unix: 0,
        }
    }

    #[tokio::test]
    async fn verify_completed_maps_to_true() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charges/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "charge_1",
                    "timeline": [{"status": "NEW"}, {"status": "PENDING"}, {"status": "COMPLETED"}],
                    "pricing": {"local": {"amount": "12.34", "currency": "USD"}}
                }
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_pending_stays_not_paid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charges/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "charge_1",
                    "timeline": [{"status": "NEW"}, {"status": "PENDING"}],
                    "pricing": {"local": {"amount": "12.34", "currency": "USD"}}
                }
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_expired_never_settles() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charges/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "charge_1",
                    "timeline": [{"status": "NEW"}, {"status": "EXPIRED"}],
                    "pricing": {"local": {"amount": "12.34", "currency": "USD"}}
                }
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_unresolved_is_flagged_not_accepted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charges/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "charge_1",
                    "timeline": [{"status": "NEW"}, {"status": "UNRESOLVED", "context": "OVERPAID"}],
                    "pricing": {"local": {"amount": "12.34", "currency": "USD"}}
                }
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(msg) if msg.contains("OVERPAID")));
    }

    #[tokio::test]
    async fn verify_unrecognised_status_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charges/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "charge_1",
                    "timeline": [{"status": "SOME_FUTURE_STATUS"}],
                    "pricing": {"local": {"amount": "12.34", "currency": "USD"}}
                }
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_empty_timeline_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charges/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "charge_1",
                    "timeline": [],
                    "pricing": {"local": {"amount": "12.34", "currency": "USD"}}
                }
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_malformed_json_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charges/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_server_error_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charges/charge_1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = receipt_with(1234, "USD", "charge_1");
        let err = rail.refund(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported("refund")));
    }
}
