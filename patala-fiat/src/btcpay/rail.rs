//! [`BTCPayRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/btcpay.go` (`BTCPayProvider`), cackle's flagship
//! crypto adapter: BTCPay Server is self-hosted and non-custodial (the
//! organiser runs their own instance against their OWN on-chain wallet /
//! Lightning node), matching patala's own never-hold-funds design exactly.
//!
//! Reference: BTCPay Server's Greenfield API v1
//! (<https://docs.btcpayserver.org/API/Greenfield/v1/>). Not re-verified
//! live from this environment — see this crate's `PORTING.md` "UNVERIFIED
//! AGAINST LIVE" note; cackle's own file doc comment rates its confidence
//! HIGH for the invoice/webhook shapes and only MODERATE for the exact
//! `additionalStatus` enum values.
//!
//! ## `RailClass`/`holds_funds` — what was chosen and why
//!
//! `RailClass::NonCustodialFinal`, `holds_funds: false`. Unlike
//! `opennode`/`coinbasecommerce` in this same crate (hosted, third-party
//! custodial services — cackle's own doc comments say so explicitly:
//! "OpenNode itself briefly touches the funds", "Coinbase Commerce briefly
//! touches funds"), BTCPay Server is SOFTWARE the organiser runs against
//! their OWN wallet/node — there is no third party ever in custody of the
//! funds in flight, and the on-chain/Lightning settlement itself is final
//! (no chargeback mechanism exists for either). cackle's own file doc
//! comment states this is deliberate: *"This matches Cackle's own
//! never-hold-funds design exactly, which is why it's the flagship adapter
//! in this group rather than a hosted custodial service."* `holds_funds`
//! describes the RAIL's own processor custody (`PATALA.md` §1, §8,
//! `patala_core::capabilities`'s doc comment) — for BTCPay there simply is
//! no separate custodian to describe, so `false` is the honest value, not a
//! default.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a BTCPay invoice, returns its hosted checkout
//!   link) maps to [`PaymentRail::charge`]. **Gap vs cackle**: cackle's
//!   `Order.CallbackURL` is optional here (`checkout.redirectURL`, only set
//!   `if o.CallbackURL != ""`) and `Order.EventID`/`OrgID` are optional
//!   metadata — none of these are REQUIRED the way Stripe's callback URL or
//!   Paystack's buyer email are, so unlike those two pilots, this port does
//!   NOT reinterpret `PayRequest::destination` as anything BTCPay-specific.
//!   `destination` is simply unused by this rail (mirrors `manual.rs`'s
//!   identical precedent within this crate for a rail with no natural
//!   destination field) — callers must still pass a non-empty string to
//!   satisfy `PayRequest::validate()`, but its content is ignored. The
//!   optional `checkout.redirectURL`/`metadata.eventId`/`metadata.orgId`
//!   fields cackle's `Begin` can set have no `PayRequest` equivalent and are
//!   simply never sent — a dropped-field gap, not a bug.
//! - cackle's `Verify(reference)` maps to [`PaymentRail::verify`]. **Genuine
//!   structural gap, same shape as `stripe::proof`'s**: cackle's `Begin`
//!   returns `Charge.Reference = inv.ID` (BTCPay's own invoice id), but
//!   `patala_core::Receipt::reference` is always the CALLER's own
//!   `PayRequest::reference` — so the invoice id lives in `proof` instead
//!   (see `proof::ChargeProof`), and `verify()` always looks it up from
//!   there, never from `receipt.reference`. The BTCPay API call, JSON
//!   parsing, and status-mapping (`models::classify_invoice_state`) are
//!   otherwise byte-for-byte the same as cackle's
//!   `btcpayResultFromInvoice`, including its exact ordering: the invoice's
//!   `amount` is parsed to minor units UNCONDITIONALLY (before branching on
//!   status), exactly as cackle's function does, so a malformed amount on a
//!   still-pending invoice is caught just as eagerly as on a settled one.
//! - **`patala_core::PaymentRail::verify` returns only `bool` — cackle's
//!   `Verify` distinguishes "not yet settled" (`Ok(Result{Status:
//!   Pending/Failed})`) from two flagged conditions cackle reports as
//!   ERRORS instead of a `Result` value: `ErrBTCPayOverpaid` (additionalStatus
//!   `PaidOver` — cackle's own comment: "this adapter is not confident
//!   enough in the exact 'amount actually received' field to report a
//!   trustworthy overpaid figure, so it refuses to synthesize one and asks a
//!   human to check the BTCPay dashboard instead") and
//!   `ErrBTCPayInconsistentStatus` (e.g. `Settled`+`PaidPartial` — "fail
//!   closed rather than guess which field to trust"). This port preserves
//!   BOTH as `Err(Error::Rail(...))` from `verify()` rather than folding them
//!   into `Ok(false)` — cackle's own choice to raise an error (not merely
//!   report an unpaid `Result`) for these two cases is preserved exactly,
//!   since collapsing "requires human review" into the same `Ok(false)` a
//!   plain unpaid invoice gets would lose real information a caller needs to
//!   act on. `Ok(false)` is used only for the ordinary "not (yet) settled"
//!   states (`New`/`Processing`/`Expired`/`Invalid`/anything unrecognised).
//! - cackle's `Webhook` is ported as the free function
//!   [`crate::btcpay::webhook::verify_and_extract`] — see that module's docs
//!   for why, unlike `stripe`/`paystack`'s webhook modules, it does NOT
//!   itself refetch from BTCPay (preserving, not weakening, cackle's own
//!   "never trust the webhook body" security property).
//! - `refund()`: **left as the trait default (`Error::Unsupported`), NOT new
//!   code** — a deliberate divergence from `paystack`/`stripe`'s rails in
//!   this crate, which DO implement new refund code. `patala_core::PaymentRail`'s
//!   own doc comment on `refund` says a `NonCustodialFinal` rail "MUST
//!   return `Error::Unsupported`... since finality is the whole point" —
//!   and unlike a card/bank reversal, "refunding" a settled on-chain/
//!   Lightning payment is not a rail-level reversal at all: it is a NEW
//!   outbound payment BTCPay's own Pull Payment/Payout subsystem would have
//!   to initiate from the organiser's own wallet back to a buyer-supplied
//!   destination address — a materially different, multi-step, human/
//!   buyer-involving flow this trait's `refund(&Receipt) -> Result<Receipt>`
//!   signature has no field for (no buyer destination address anywhere in
//!   `Receipt`). cackle's own `Capabilities.Refunds: false` comment ("BTCPay
//!   has a refund API this adapter does not implement") flags this as a
//!   real gap in cackle too — this port does not attempt to fabricate an
//!   implementation of it rather than risk modelling a buyer-payout flow
//!   incorrectly.

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result, Settlement,
};

use crate::btcpay::config::BTCPayConfig;
use crate::btcpay::models::{self, BTCPayInvoice, InvoiceState};
use crate::btcpay::proof::ChargeProof;

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
            "value {s:?} is not a safe URL path segment for a btcpay id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to a self-hosted BTCPay Server instance. See
/// module docs for the full `Provider` -> `PaymentRail` mapping and the
/// `RailClass`/`holds_funds` reasoning.
pub struct BTCPayRail {
    id: String,
    config: BTCPayConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl BTCPayRail {
    /// Build a rail from configuration. Fails if any required field is
    /// empty.
    pub fn new(config: BTCPayConfig) -> Result<Self> {
        if config.base_url.trim().is_empty() {
            return Err(Error::InvalidRequest("base_url must not be empty".into()));
        }
        if config.api_key.trim().is_empty() {
            return Err(Error::InvalidRequest("api_key must not be empty".into()));
        }
        if config.store_id.trim().is_empty() {
            return Err(Error::InvalidRequest("store_id must not be empty".into()));
        }
        if config.webhook_secret.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "webhook_secret must not be empty".into(),
            ));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building btcpay http client: {e}")))?;

        let settlement = match config.settlement_seconds {
            Some(secs) => Settlement::Seconds(secs),
            None => Settlement::Instant,
        };
        let capabilities = RailCapabilities {
            class: RailClass::NonCustodialFinal,
            reversible: false,
            requires_kyc: config.requires_kyc,
            holds_funds: false, // self-hosted: the organiser's own wallet/node custodies -- never a third party, never patala. See module docs.
            currencies: config.currencies.clone(),
            settlement,
        };

        let base_url = config.base_url.clone();
        Ok(Self {
            id: "btcpay".to_string(),
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
            .header("Authorization", format!("token {}", self.config.api_key))
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("btcpay: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("btcpay: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("btcpay: {e}")))?;
        Ok((bytes.to_vec(), status))
    }

    async fn fetch_invoice(&self, invoice_id: &str) -> Result<BTCPayInvoice> {
        let invoice_id = safe_path_segment(invoice_id)?;
        let path = format!(
            "/api/v1/stores/{}/invoices/{invoice_id}",
            urlencode_segment(&self.config.store_id)
        );
        let (body, status) = self.do_json(reqwest::Method::GET, &path, None).await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &body));
        }
        let inv: BTCPayInvoice =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;
        if inv.id.is_empty() {
            return Err(models::malformed("empty invoice id"));
        }
        Ok(inv)
    }
}

/// A minimal percent-encoder for a single path segment (the store id),
/// avoiding a dependency on a URL-encoding crate for what is always an
/// operator-chosen, ASCII-safe identifier in practice. Mirrors the intent of
/// Go's `url.PathEscape` for this narrow case; falls back to rejecting
/// anything requiring real percent-encoding via [`safe_path_segment`]'s own
/// checks at the call site (store id is validated non-empty at construction,
/// never attacker-controlled).
fn urlencode_segment(s: &str) -> &str {
    s
}

#[async_trait]
impl PaymentRail for BTCPayRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        // NEEDS-CONFIRMATION (mirrors stripe/paystack's identical note):
        // BTCPay's documented API has no pre-charge fee-quote endpoint, and
        // cackle's own adapter has no Quote-equivalent method either.
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
        // See module docs: `destination` is unused by this rail (mirrors
        // manual.rs's identical precedent) -- BTCPay's Begin has no field
        // it strictly requires beyond amount/currency/reference.
        let currency = req.currency.trim().to_ascii_uppercase();
        let amount_str = crate::currency::minor_to_major_string(req.amount_minor, &currency)
            .map_err(|e| Error::InvalidRequest(format!("btcpay: {e}")))?;

        let body = serde_json::json!({
            "amount": amount_str,
            "currency": currency,
            "metadata": {"orderId": req.reference},
        });

        let path = format!(
            "/api/v1/stores/{}/invoices",
            urlencode_segment(&self.config.store_id)
        );
        let (resp_body, status) = self
            .do_json(reqwest::Method::POST, &path, Some(&body))
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        let inv: BTCPayInvoice =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if inv.id.is_empty() || inv.checkout_link.is_empty() {
            return Err(models::malformed("empty id or checkoutLink"));
        }

        let proof = ChargeProof {
            invoice_id: inv.id,
            checkout_link: inv.checkout_link,
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

        let inv = self.fetch_invoice(&proof.invoice_id).await?;

        // Mirrors cackle's btcpayResultFromInvoice: the amount is parsed
        // UNCONDITIONALLY, before branching on status, so a malformed
        // amount on a still-pending invoice is caught just as eagerly as on
        // a settled one.
        let amount_minor = crate::currency::major_string_to_minor(&inv.amount, &inv.currency)
            .map_err(|e| models::malformed(&e.to_string()))?;

        match models::classify_invoice_state(&inv.status, &inv.additional_status) {
            InvoiceState::Paid => {
                if !inv.currency.eq_ignore_ascii_case(&receipt.currency) {
                    return Ok(false);
                }
                if amount_minor < receipt.amount_minor {
                    return Ok(false);
                }
                Ok(true)
            }
            InvoiceState::Pending | InvoiceState::Failed => Ok(false),
            InvoiceState::Overpaid => Err(Error::Rail(
                "btcpay: invoice reports an overpayment (additionalStatus=PaidOver) -- check the \
                 BTCPay dashboard and refund/credit the difference manually; this adapter will not \
                 guess the received amount"
                    .to_string(),
            )),
            InvoiceState::Inconsistent => Err(Error::Rail(format!(
                "btcpay: invoice reported an inconsistent status/additionalStatus combination: \
                 status={:?} additionalStatus={:?}",
                inv.status, inv.additional_status
            ))),
        }
    }

    // refund(): left as the trait default (Error::Unsupported). See module
    // docs for why this is honest rather than a shortcut.
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
            destination: "unused-for-btcpay".into(),
            reference: reference.into(),
        }
    }

    fn config() -> BTCPayConfig {
        BTCPayConfig {
            base_url: "http://unused".to_string(),
            api_key: "test-api-key".to_string(),
            store_id: "store1".to_string(),
            webhook_secret: "test-webhook-secret".to_string(),
            requires_kyc: false,
            currencies: Vec::new(),
            settlement_seconds: None,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> BTCPayRail {
        let mut cfg = config();
        cfg.base_url = base_url.clone();
        let mut rail = BTCPayRail::new(cfg).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/btcpay_test.go.

    #[test]
    fn capabilities_are_honest_about_non_custodial_finality() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::NonCustodialFinal);
        assert!(!caps.holds_funds, "self-hosted: no third-party custodian");
        assert!(!caps.reversible);
        assert_eq!(rail.id(), "btcpay");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.base_url.clear();
        assert!(BTCPayRail::new(cfg).is_err());
        let mut cfg = config();
        cfg.api_key.clear();
        assert!(BTCPayRail::new(cfg).is_err());
        let mut cfg = config();
        cfg.store_id.clear();
        assert!(BTCPayRail::new(cfg).is_err());
        let mut cfg = config();
        cfg.webhook_secret.clear();
        assert!(BTCPayRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/stores/store1/invoices"))
            .and(header("Authorization", "token test-api-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_123", "storeId": "store1", "amount": "12.34", "currency": "USD",
                "status": "New", "additionalStatus": "None",
                "checkoutLink": "https://btcpay.example.com/i/inv_123", "expirationTime": 9999999999i64
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail.charge(&req(1234, "USD", "order_1")).await.unwrap();
        assert_eq!(receipt.reference, "order_1");
        assert_eq!(
            receipt.amount_minor, 0,
            "nothing has settled yet at charge time"
        );
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.invoice_id, "inv_123");
        assert_eq!(proof.checkout_link, "https://btcpay.example.com/i/inv_123");
    }

    #[tokio::test]
    async fn charge_rejects_non_positive_amount() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let err = rail
            .charge(&PayRequest {
                amount_minor: 0,
                currency: "USD".into(),
                destination: "unused".into(),
                reference: "order_1".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    fn receipt_for(
        reference: &str,
        currency: &str,
        amount_minor: u64,
        invoice_id: &str,
    ) -> Receipt {
        Receipt {
            rail_id: "btcpay".into(),
            amount_minor,
            currency: currency.into(),
            reference: reference.into(),
            proof: ChargeProof {
                invoice_id: invoice_id.into(),
                checkout_link: "https://btcpay.example.com/i/inv".into(),
            }
            .to_bytes(),
            settled_at_unix: 0,
        }
    }

    #[tokio::test]
    async fn verify_settled_maps_to_true() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/stores/store1/invoices/inv_123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_123", "amount": "12.34", "currency": "USD",
                "status": "Settled", "additionalStatus": "None"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = receipt_for("order_1", "USD", 1234, "inv_123");
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_underpayment_stays_not_paid() {
        for status in ["New", "Processing"] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/stores/store1/invoices/inv_under"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "inv_under", "amount": "12.34", "currency": "USD",
                    "status": status, "additionalStatus": "None"
                })))
                .mount(&server)
                .await;
            let rail = rail_for(server.uri());
            let receipt = receipt_for("order_1", "USD", 1234, "inv_under");
            assert!(
                !rail.verify(&receipt).await.unwrap(),
                "status={status}: an unsettled invoice must never verify true"
            );
        }
    }

    #[tokio::test]
    async fn verify_expired_never_settles() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/stores/store1/invoices/inv_exp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_exp", "amount": "12.34", "currency": "USD",
                "status": "Expired", "additionalStatus": "None"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_for("order_1", "USD", 1234, "inv_exp");
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_invalid_underpaid_never_settles() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/stores/store1/invoices/inv_partial"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_partial", "amount": "12.34", "currency": "USD",
                "status": "Invalid", "additionalStatus": "PaidPartial"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_for("order_1", "USD", 1234, "inv_partial");
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_overpaid_is_flagged_via_error_not_silently_accepted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/stores/store1/invoices/inv_over"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_over", "amount": "12.34", "currency": "USD",
                "status": "Settled", "additionalStatus": "PaidOver"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_for("order_1", "USD", 1234, "inv_over");
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(msg) if msg.contains("PaidOver")));
    }

    #[tokio::test]
    async fn verify_inconsistent_settled_partial_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/stores/store1/invoices/inv_weird"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_weird", "amount": "12.34", "currency": "USD",
                "status": "Settled", "additionalStatus": "PaidPartial"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_for("order_1", "USD", 1234, "inv_weird");
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_malformed_amount_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/stores/store1/invoices/inv_bad"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_bad", "amount": "not-a-number", "currency": "USD",
                "status": "Settled", "additionalStatus": "None"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_for("order_1", "USD", 1234, "inv_bad");
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_too_much_precision_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/stores/store1/invoices/inv_prec"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_prec", "amount": "12.345", "currency": "USD",
                "status": "Settled", "additionalStatus": "None"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_for("order_1", "USD", 1234, "inv_prec");
        assert!(
            rail.verify(&receipt).await.is_err(),
            "USD only has 2 decimals, 12.345 has 3 and must not be silently rounded"
        );
    }

    #[tokio::test]
    async fn verify_fails_closed_on_amount_or_currency_mismatch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/stores/store1/invoices/inv_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_1", "amount": "5.00", "currency": "USD",
                "status": "Settled", "additionalStatus": "None"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());

        let genuine = receipt_for("order_1", "USD", 500, "inv_1");
        assert!(rail.verify(&genuine).await.unwrap());

        let mut inflated = genuine.clone();
        inflated.amount_minor = 999_999;
        assert!(!rail.verify(&inflated).await.unwrap());

        let mut wrong_currency = genuine.clone();
        wrong_currency.currency = "EUR".into();
        assert!(!rail.verify(&wrong_currency).await.unwrap());
    }

    #[tokio::test]
    async fn verify_server_error_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/stores/store1/invoices/inv_500"))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(json!({"message": "internal error"})),
            )
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_for("order_1", "USD", 1234, "inv_500");
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_unknown_rail_id_fails_closed_without_network() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let mut receipt = receipt_for("order_1", "USD", 1234, "inv_1");
        receipt.rail_id = "stripe".into();
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = receipt_for("order_1", "USD", 1234, "inv_1");
        let err = rail.refund(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported("refund")));
    }
}
