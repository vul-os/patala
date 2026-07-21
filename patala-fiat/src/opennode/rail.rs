//! [`OpenNodeRail`] — the `PaymentRail` implementation. Ported from
//! cackle's `internal/payments/opennode.go` (`OpenNodeProvider`).
//!
//! Reference: <https://developers.opennode.com/reference>. Not re-verified
//! live from this environment — see this crate's `PORTING.md` "UNVERIFIED
//! AGAINST LIVE" note.
//!
//! ## `RailClass`/`holds_funds` — what was chosen and why
//!
//! `RailClass::CustodialReversible`, `holds_funds: true` — **a deliberate
//! divergence from `btcpay`/`lnbits` in this same crate**, not an
//! inconsistency. cackle's own file doc comment is explicit that OpenNode
//! is NOT the same shape as BTCPay/LNbits: *"Unlike btcpay.go/lnbits.go,
//! OpenNode is not self-hosted: the organiser holds an OpenNode merchant
//! account and OpenNode itself briefly touches the funds before paying the
//! organiser out. That's why the payments contract ranks this as priority 3
//! ('hosted custodial services... as conveniences') behind BTCPay/LNbits,
//! not the flagship."* `patala_core::RailClass` only has two values, and
//! `NonCustodialFinal` describes a wallet-to-wallet rail with NO
//! intermediary custody at all (`PATALA.md` §3: "a wallet address plus a
//! signed final receipt") — that is false for OpenNode, which sits between
//! buyer and organiser as a genuine (if brief) custodian, exactly as
//! Paystack/Stripe do for fiat. `CustodialReversible` is the more honest
//! class even though the UNDERLYING settlement asset (Bitcoin/Lightning) has
//! no on-chain chargeback mechanism the way a card does — `reversible` is
//! set to `false` (below) to keep that distinction visible: this rail is
//! CUSTODIAL (a real intermediary in the money's path, `holds_funds: true`)
//! but NOT contractually reversible the way a disputed card charge is
//! (cackle's own `Capabilities.Refunds: false`, with no "supports it, not
//! implemented" comment the way Paystack's has — see `refund()` below).
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates an OpenNode charge, returns its hosted
//!   checkout URL) maps to [`PaymentRail::charge`]. Cackle's
//!   `Order.BuyerEmail`/`CallbackURL` are both OPTIONAL here (`if o.BuyerEmail
//!   != "" customer_email`; `if o.CallbackURL != "" success_url`) — neither
//!   is required the way Stripe's callback URL or Paystack's buyer email
//!   are, so (like `btcpay`/`lnbits`) this port does NOT reinterpret
//!   `PayRequest::destination` as anything OpenNode-specific (mirrors
//!   `manual.rs`'s precedent). `Order.EventID`/`OrgID` have no `PayRequest`
//!   equivalent and are simply not sent — a dropped-field gap.
//! - cackle's `Verify(reference)` maps to [`PaymentRail::verify`]. **Same
//!   structural gap as `btcpay::proof`'s**: cackle's `Begin` returns
//!   `Charge.Reference = envelope.Data.ID` (OpenNode's own charge id), so
//!   this port carries that id in `proof` instead (see `proof::ChargeProof`)
//!   and `verify()` always looks it up from there, never from
//!   `receipt.reference`. The charge-fetch call, the flexible
//!   number-or-string amount parsing (`models::flexible_json_amount_to_string`,
//!   cackle's own `flexibleJSONAmountToString`), and the status-mapping
//!   (`models::classify_charge_state`) are byte-for-byte the same as
//!   cackle's `opennodeResultFromCharge`.
//! - **Overpayment**: cackle's own file doc comment is explicit that
//!   OpenNode's documented status enum has NO distinct "overpaid" state
//!   (unlike BTCPay's `additionalStatus=="PaidOver"`) — *"If OpenNode itself
//!   tolerates/settles a modest overpayment silently as 'paid', this
//!   adapter reports whatever amount OpenNode's charge object says was
//!   settled — the generic Reconcile() step... is the backstop."* This
//!   port's `verify()` preserves exactly that backstop shape: it reports
//!   whatever amount OpenNode's charge object says (via the ordinary `Paid`
//!   branch), and the anti-fraud check against `receipt.amount_minor` (see
//!   `PORTING.md` §6 — `>=`, never `==`) is what would surface a caller-side
//!   discrepancy, exactly as cackle's own `Reconcile`/`ErrAmountMismatch`
//!   would at the layer above `Verify`.
//! - cackle's `Webhook` is ported as the free function
//!   [`crate::opennode::webhook::verify_and_extract`] — see that module's
//!   docs for why it does not itself refetch (same pattern as `btcpay`/
//!   `lnbits`/`coinbasecommerce`).
//! - `refund()`: **left as the trait default (`Error::Unsupported`)**.
//!   cackle's `Capabilities.Refunds: false` has no "supports it, not
//!   implemented here" comment the way Paystack's does (contrast
//!   `paystack::rail`'s `refund()`, which IS new code because cackle's own
//!   comment flags Paystack as a legitimate gap to fill — see `PORTING.md`
//!   §7's exact rule on this distinction). Without that signal, and without
//!   independently confirming a documented OpenNode refund endpoint from
//!   this environment, fabricating one would violate this crate's "never
//!   fabricate" rule (`PORTING.md` §10) — left `Unsupported` honestly.

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result, Settlement,
};

use crate::opennode::config::OpenNodeConfig;
use crate::opennode::models::{self, ChargeState, Envelope, OpenNodeCharge};
use crate::opennode::proof::ChargeProof;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for an opennode charge id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to OpenNode's hosted checkout API. See module
/// docs for the full `Provider` -> `PaymentRail` mapping and the
/// `RailClass`/`holds_funds` reasoning.
pub struct OpenNodeRail {
    id: String,
    config: OpenNodeConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl OpenNodeRail {
    /// Build a rail from configuration. Fails if `api_key` is empty.
    pub fn new(config: OpenNodeConfig) -> Result<Self> {
        if config.api_key.trim().is_empty() {
            return Err(Error::InvalidRequest("api_key must not be empty".into()));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building opennode http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: false, // cackle: Refunds: false, no "supports it" signal. See module docs.
            requires_kyc: config.requires_kyc,
            holds_funds: true, // OpenNode (the PROCESSOR) briefly custodies -- never patala. See module docs.
            currencies: config.currencies.clone(),
            settlement: Settlement::Instant,
        };

        let base_url = config.base_url.clone();
        Ok(Self {
            id: "opennode".to_string(),
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
            .header("Authorization", &self.config.api_key)
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("opennode: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("opennode: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("opennode: {e}")))?;
        Ok((bytes.to_vec(), status))
    }

    async fn fetch_charge(&self, charge_id: &str) -> Result<OpenNodeCharge> {
        let charge_id = safe_path_segment(charge_id)?;
        let path = format!("/v1/charge/{charge_id}");
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
impl PaymentRail for OpenNodeRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        // NEEDS-CONFIRMATION: no pre-charge fee-quote endpoint in OpenNode's
        // documented API or cackle's own adapter.
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
            .map_err(|e| Error::InvalidRequest(format!("opennode: {e}")))?;
        let amount_float: f64 = amount_str.parse().map_err(|_| {
            Error::Rail("opennode: could not render amount as a number".to_string())
        })?;

        let body = serde_json::json!({
            "amount": amount_float,
            "currency": currency,
            "order_id": req.reference,
            "description": format!("patala order {}", req.reference),
        });

        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, "/v1/charges", Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }
        let envelope: Envelope =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if envelope.data.id.is_empty() || envelope.data.hosted_checkout_url.is_empty() {
            return Err(models::malformed("empty id or hosted_checkout_url"));
        }

        let proof = ChargeProof {
            charge_id: envelope.data.id,
            hosted_checkout_url: envelope.data.hosted_checkout_url,
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
        let amount_str = models::flexible_json_amount_to_string(&charge.amount)?;
        let amount_minor = crate::currency::major_string_to_minor(&amount_str, &charge.currency)
            .map_err(|e| models::malformed(&e.to_string()))?;

        match models::classify_charge_state(&charge.status) {
            ChargeState::Paid => {
                if !charge.currency.eq_ignore_ascii_case(&receipt.currency) {
                    return Ok(false);
                }
                if amount_minor < receipt.amount_minor {
                    return Ok(false);
                }
                Ok(true)
            }
            ChargeState::Pending | ChargeState::Failed => Ok(false),
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
            destination: "unused-for-opennode".into(),
            reference: reference.into(),
        }
    }

    fn config() -> OpenNodeConfig {
        OpenNodeConfig {
            api_key: "test-api-key".to_string(),
            base_url: "http://unused".to_string(),
            requires_kyc: false,
            currencies: Vec::new(),
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> OpenNodeRail {
        let mut cfg = config();
        cfg.base_url = base_url;
        OpenNodeRail::new(cfg).unwrap()
    }

    // Ported from cackle's internal/payments/opennode_test.go.

    #[test]
    fn capabilities_reflect_hosted_custodial_model() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR briefly custodies");
        assert!(!caps.reversible);
    }

    #[test]
    fn new_requires_api_key() {
        let mut cfg = config();
        cfg.api_key.clear();
        assert!(OpenNodeRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/charges"))
            .and(header("Authorization", "test-api-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "charge_1", "status": "unpaid", "amount": 12.34, "currency": "USD",
                         "hosted_checkout_url": "https://checkout.opennode.com/charge_1"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail.charge(&req(1234, "USD", "order_1")).await.unwrap();
        assert_eq!(receipt.reference, "order_1");
        assert_eq!(receipt.amount_minor, 0);
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.charge_id, "charge_1");
    }

    #[tokio::test]
    async fn charge_rejects_non_positive_amount() {
        let rail = rail_for("http://127.0.0.1:1".into());
        assert!(rail.charge(&req(0, "USD", "order_1")).await.is_err());
    }

    fn receipt_with(amount_minor: u64, currency: &str, charge_id: &str) -> Receipt {
        Receipt {
            rail_id: "opennode".into(),
            amount_minor,
            currency: currency.into(),
            reference: "order_1".into(),
            proof: ChargeProof {
                charge_id: charge_id.into(),
                hosted_checkout_url: "https://checkout.opennode.com/x".into(),
            }
            .to_bytes(),
            settled_at_unix: 0,
        }
    }

    #[tokio::test]
    async fn verify_paid_maps_to_true() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/charge/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "charge_1", "status": "paid", "amount": 12.34, "currency": "USD"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_string_amount_also_parses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/charge/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "charge_1", "status": "paid", "amount": "12.34", "currency": "USD"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_underpaid_never_settles() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/charge/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "charge_1", "status": "underpaid", "amount": 12.34, "currency": "USD"}
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
            .and(path("/v1/charge/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "charge_1", "status": "expired", "amount": 12.34, "currency": "USD"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_refunded_never_reports_paid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/charge/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "charge_1", "status": "refunded", "amount": 12.34, "currency": "USD"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_malformed_amount_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/charge/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "charge_1", "status": "paid", "amount": null, "currency": "USD"}
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
            .and(path("/v1/charge/charge_1"))
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
            .and(path("/v1/charge/charge_1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(1234, "USD", "charge_1");
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_amount_mismatch_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/charge/charge_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"id": "charge_1", "status": "paid", "amount": 5.00, "currency": "USD"}
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let mut receipt = receipt_with(500, "USD", "charge_1");
        assert!(rail.verify(&receipt).await.unwrap());
        receipt.amount_minor = 999_999;
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = receipt_with(1234, "USD", "charge_1");
        let err = rail.refund(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported("refund")));
    }
}
