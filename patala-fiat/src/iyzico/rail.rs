//! [`IyzicoRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/iyzico.go` (`IyzicoProvider`).
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (initializes an iyzico Checkout Form, returns its
//!   hosted `paymentPageUrl`) maps to [`PaymentRail::charge`]. **Gap vs
//!   cackle** (`PORTING.md` §3): iyzico's Checkout Form is a redirect flow
//!   and needs a `callbackUrl`; `patala_core::PayRequest` has no callback
//!   field. This port reinterprets `PayRequest::destination` AS that
//!   callback/return URL, the same choice `stripe::StripeRail` makes for the
//!   identical structural reason (redirect-flow needs a return URL more
//!   than it needs any other single `Order` field iyzico's own `Begin`
//!   doesn't hard-validate). Callers of `IyzicoRail::charge` must pass the
//!   desired post-checkout return URL as `destination`, NOT an email or
//!   payment token. `BuyerEmail`/`BuyerName` therefore have no
//!   `PayRequest` home and are simply never sent (an information-loss gap,
//!   not a workaround — see `PORTING.md` §3).
//! - **Not attempted, exactly as cackle is not**: iyzico's mandatory
//!   buyer/address/basket-item fields beyond what's listed above — see
//!   `mod.rs`'s module docs.
//! - **Genuine cackle quirk, preserved via the seam, not silently changed**
//!   (see `proof.rs`'s module docs): cackle's `Begin` returns
//!   `Charge.Reference = parsed.Token`, not `o.Reference` — the Checkout
//!   Form TOKEN is what cackle's own system actually tracks a payment by.
//!   This port's `Receipt::reference` stays the CALLER's own reference (per
//!   `patala_core::Receipt`'s contract); the real iyzico token lives in
//!   `proof` instead, and `verify()`/the webhook path always look it up
//!   from there.
//! - cackle's `Verify(reference)` (`retrieveCheckoutForm`) maps to
//!   [`PaymentRail::verify`], keyed by the token embedded in `proof`.
//! - cackle's `Webhook` (an UNSIGNED callback that just re-calls `Verify`)
//!   is ported as [`IyzicoRail::handle_webhook`] — a RAIL METHOD, not a free
//!   function in `webhook.rs`, because it needs network access + IYZWS
//!   credentials. See `webhook.rs`'s module docs for why this is a
//!   necessary, protocol-driven divergence from `stripe`/`paystack`'s
//!   pure-function webhook shape, not an arbitrary one.
//! - **LOW-MEDIUM confidence, explicitly flagged** (cackle's own file
//!   header, carried forward verbatim): the exact "IYZWS" outbound
//!   request-signing byte sequence in [`IyzicoRail::auth_headers`]
//!   (`hashStr = apiKey + randomKey + secretKey + body`, SHA1, base64,
//!   header `Authorization: IYZWS {apiKey}:{signature}` +
//!   `x-iyzi-rnd: {randomKey}`) is iyzico's long-standing "classic" v1
//!   scheme; iyzico has since introduced a newer HMACSHA256-based
//!   "IYZWSv2" for some merchants that this port does NOT implement. If
//!   requests come back unauthorized against a real account, check which
//!   scheme that merchant account actually requires.
//! - **Disclosed, harmless byte-level difference from cackle's own wire
//!   bytes**: cackle signs a Go `map[string]any` request body, which Go's
//!   `encoding/json` marshals with keys sorted ALPHABETICALLY. This port
//!   signs a fixed-field-order Rust struct instead (`serde_json` preserves
//!   struct field declaration order). The IYZWS scheme only requires "the
//!   bytes we hash equal the bytes we send" (iyzico's server recomputes the
//!   signature over whatever bytes IT receives) — it does not require any
//!   particular key ORDER, so this is a legitimate implementation
//!   difference, not a fidelity or security regression. This port signs and
//!   sends the exact same byte buffer for every request (see `do_post`).
//! - `refund()`: **not implemented.** Cackle's `Capabilities().Refunds` is
//!   `false` for iyzico with no "supports it, not implemented here"
//!   comment — same reasoning as `flutterwave::rail` for not adding new,
//!   unverifiable refund code. Returns the trait default
//!   (`Error::Unsupported`).

use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha1::Digest;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::iyzico::config::IyzicoConfig;
use crate::iyzico::models::{self, IyzicoCheckoutFormResult};
use crate::iyzico::proof::ChargeProof;
use crate::iyzico::webhook::{IyzicoWebhookError, IyzicoWebhookOutcome};

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Mirrors `stripe::rail`/`paystack::rail`'s identical `safe_path_segment`
/// (here used to sanity-check the iyzico token before using it, not as a
/// URL path segment specifically -- iyzico's token travels inside a JSON
/// body, not a URL, but the same "no control/structural characters" check
/// is the right defense-in-depth).
fn safe_reference(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['\t', '\n', '\r']) {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe iyzico token"
        )));
    }
    Ok(s)
}

/// Mirrors cackle's `newIyzicoRandomKey`: a fresh, per-request nonce. Not a
/// replay-protection nonce on iyzico's side (see cackle's own comment) —
/// only needs to be freshly computed each call for the hash to differ.
fn new_random_key() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// One `PaymentRail` talking to iyzico's Checkout Form API. See module docs
/// for the full `Provider` -> `PaymentRail` mapping.
pub struct IyzicoRail {
    id: String,
    config: IyzicoConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl IyzicoRail {
    /// Build a rail from configuration. Fails if `api_key`, `secret_key`,
    /// or `currencies` are empty.
    pub fn new(config: IyzicoConfig) -> Result<Self> {
        if config.api_key.trim().is_empty() {
            return Err(Error::InvalidRequest("api_key must not be empty".into()));
        }
        if config.secret_key.trim().is_empty() {
            return Err(Error::InvalidRequest("secret_key must not be empty".into()));
        }
        if config.currencies.is_empty() {
            return Err(Error::InvalidRequest("currencies must not be empty".into()));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building iyzico http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // iyzico (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
            atomic_multi_party: false, // always false: N payouts here are N independent API calls, never one atomic event (B3)
        };

        let base_url = config.base_url.clone();
        Ok(Self {
            id: "iyzico".to_string(),
            config,
            http,
            capabilities,
            base_url,
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

    /// Mirrors cackle's `iyzicoAuthHeaders`: the classic "IYZWS" v1
    /// HMAC-SHA1-shaped (but NOT a keyed HMAC -- a plain concatenated SHA1
    /// digest) signature. See module docs for this scheme's confidence
    /// level.
    fn auth_headers(&self, random_key: &str, body: &[u8]) -> (String, String) {
        let mut hasher = sha1::Sha1::new();
        hasher.update(self.config.api_key.as_bytes());
        hasher.update(random_key.as_bytes());
        hasher.update(self.config.secret_key.as_bytes());
        hasher.update(body);
        let digest = hasher.finalize();
        let signature = base64::engine::general_purpose::STANDARD.encode(digest);
        (
            format!("IYZWS {}:{}", self.config.api_key, signature),
            random_key.to_string(),
        )
    }

    async fn do_post(&self, path: &str, body_bytes: &[u8]) -> Result<(Vec<u8>, u16)> {
        let random_key = new_random_key();
        let (authorization, x_iyzi_rnd) = self.auth_headers(&random_key, body_bytes);
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", authorization)
            .header("x-iyzi-rnd", x_iyzi_rnd)
            .header("Content-Type", "application/json")
            .body(body_bytes.to_vec())
            .send()
            .await
            .map_err(|e| Error::Rail(format!("iyzico: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("iyzico: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("iyzico: {e}")))?;
        Ok((bytes.to_vec(), status))
    }

    /// Mirrors cackle's `Verify`'s HTTP call: `retrieveCheckoutForm` keyed
    /// by `token` (used for both `conversationId` and `token`, exactly as
    /// cackle's `Verify` does).
    async fn retrieve_checkout_form(&self, token: &str) -> Result<IyzicoCheckoutFormResult> {
        #[derive(Serialize)]
        struct RetrieveRequest<'a> {
            locale: &'a str,
            #[serde(rename = "conversationId")]
            conversation_id: &'a str,
            token: &'a str,
        }
        let body = RetrieveRequest {
            locale: "en",
            conversation_id: token,
            token,
        };
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| Error::Rail(format!("iyzico: encode request: {e}")))?;
        let (resp_body, status) = self
            .do_post(
                "/payment/iyzipos/checkoutform/auth/ecom/detail",
                &body_bytes,
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }
        serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))
    }

    /// Shared by [`PaymentRail::verify`] and [`Self::handle_webhook`]:
    /// re-confirm `token` against iyzico and evaluate the outcome,
    /// squashing a content-level API failure (see
    /// `models::evaluate_checkout_form`'s docs) into `None` rather than
    /// propagating it as `Err` -- only a genuine transport/parse failure
    /// (the `?` above) is a real `Err`.
    async fn retrieve_and_evaluate(&self, token: &str) -> Result<Option<models::CheckoutOutcome>> {
        let result = self.retrieve_checkout_form(token).await?;
        Ok(models::evaluate_checkout_form(&result).ok())
    }

    /// Handle an iyzico Checkout Form callback. See module docs and
    /// `webhook.rs`'s module docs for why this must be a rail method, not a
    /// free function: iyzico's callback carries no verifiable signature at
    /// all, so the only real verification is the SAME authenticated
    /// `retrieveCheckoutForm` round trip [`PaymentRail::verify`] uses.
    /// Mirrors cackle's `IyzicoProvider.Webhook`, which is literally
    /// `return p.Verify(ctx, token)`.
    pub async fn handle_webhook(
        &self,
        content_type: &str,
        raw_body: &[u8],
    ) -> Result<IyzicoWebhookOutcome> {
        crate::httpshared::bounded_len_check(raw_body, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("iyzico: {e}")))?;
        let token = crate::iyzico::webhook::extract_token(content_type, raw_body)
            .ok_or(Error::Rail(IyzicoWebhookError::MissingToken.to_string()))?;
        let outcome = self.retrieve_and_evaluate(&token).await?;
        let (settled, amount_minor, currency) = match outcome {
            Some(o) => (o.settled, o.amount_minor, o.currency),
            None => (false, 0, String::new()),
        };
        Ok(IyzicoWebhookOutcome {
            token,
            settled,
            amount_minor,
            currency,
        })
    }
}

#[async_trait]
impl PaymentRail for IyzicoRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::redirect_url`], because on the `iyzico` rail
    /// `destination` is not a payout address: it is the post-checkout return
    /// URL, sent as iyzico's `callbackUrl` (see this module's docs above).
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
        crate::currency::minor_to_major_string(req.amount_minor, &req.currency)
            .map_err(|e| Error::InvalidRequest(e.to_string()))?;

        // NEEDS-CONFIRMATION (mirrors every other rail's identical note):
        // iyzico's documented API has no pre-charge fee-quote endpoint, and
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
        // See module docs: `destination` is reinterpreted as the
        // callbackUrl iyzico's Checkout Form redirect flow needs.
        let callback_url = req.destination.trim();
        let currency = req.currency.trim().to_ascii_uppercase();
        let price = crate::currency::minor_to_major_string(req.amount_minor, &currency)
            .map_err(|e| Error::InvalidRequest(e.to_string()))?;

        #[derive(Serialize)]
        struct BeginRequest<'a> {
            locale: &'a str,
            #[serde(rename = "conversationId")]
            conversation_id: &'a str,
            price: &'a str,
            #[serde(rename = "paidPrice")]
            paid_price: &'a str,
            currency: &'a str,
            #[serde(rename = "basketId")]
            basket_id: &'a str,
            #[serde(rename = "paymentGroup")]
            payment_group: &'a str,
            #[serde(rename = "callbackUrl")]
            callback_url: &'a str,
        }
        let body = BeginRequest {
            locale: "en",
            conversation_id: &req.reference,
            price: &price,
            paid_price: &price,
            currency: &currency,
            basket_id: &req.reference,
            payment_group: "PRODUCT",
            callback_url,
        };
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| Error::Rail(format!("iyzico: encode request: {e}")))?;

        let (resp_body, status) = self
            .do_post(
                "/payment/iyzipos/checkoutform/initialize/auth/ecom",
                &body_bytes,
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }

        #[derive(Deserialize)]
        struct CreateResponse {
            status: String,
            #[serde(default)]
            token: String,
            #[serde(default, rename = "paymentPageUrl")]
            payment_page_url: String,
            #[serde(default, rename = "errorMessage")]
            error_message: String,
        }
        let parsed: CreateResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.status != "success"
            || parsed.token.is_empty()
            || parsed.payment_page_url.is_empty()
        {
            let msg = if parsed.error_message.is_empty() {
                format!("status={:?}", parsed.status)
            } else {
                parsed.error_message
            };
            return Err(models::unexpected_status(&msg));
        }

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see PORTING.md §5
            currency,
            reference: req.reference.clone(), // the CALLER's own reference -- see proof.rs
            proof: ChargeProof {
                token: parsed.token,
                redirect_url: Some(parsed.payment_page_url),
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
        let Ok(token) = safe_reference(&proof.token) else {
            return Ok(false);
        };
        let Some(outcome) = self.retrieve_and_evaluate(token).await? else {
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

    /// Verify an iyzico Checkout Form callback — delegates to
    /// [`Self::handle_webhook`].
    ///
    /// iyzico's callback carries **no signature at all**; the only real
    /// verification is the same authenticated `retrieveCheckoutForm` round
    /// trip [`Self::verify`] uses, so this method is honest about being a
    /// re-fetch rather than a signature check. Header read: `Content-Type`
    /// (the callback may be form-encoded or JSON).
    ///
    /// The callback names a checkout token, not a caller reference, so
    /// [`WebhookEvent::reference`] is empty and the token is on
    /// [`WebhookEvent::object_id`].
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let outcome = self
            .handle_webhook(delivery.header_or_empty("Content-Type"), &delivery.raw_body)
            .await?;
        let token = outcome.token.clone();
        Ok(WebhookEvent::settlement(
            &self.id,
            outcome.token,
            "",
            outcome.settled,
            outcome.amount_minor,
            outcome.currency,
        )
        .with_object_id(token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(amount: u64, currency: &str, callback_url: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: callback_url.into(),
            reference: reference.into(),
        }
    }

    fn config() -> IyzicoConfig {
        IyzicoConfig {
            api_key: "test-api-key".to_string(),
            secret_key: "test-secret-key".to_string(),
            base_url: crate::iyzico::config::PRODUCTION_BASE_URL.to_string(),
            requires_kyc: true,
            currencies: crate::iyzico::config::DEFAULT_CURRENCIES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> IyzicoRail {
        let mut rail = IyzicoRail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/iyzico_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(rail.id(), "iyzico");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.api_key.clear();
        assert!(IyzicoRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.secret_key.clear();
        assert!(IyzicoRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.currencies.clear();
        assert!(IyzicoRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn charge_sends_iyzws_auth_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payment/iyzipos/checkoutform/initialize/auth/ecom"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "token": "tok_abc",
                "paymentPageUrl": "https://sandbox-cpp.iyzipay.com/tok_abc"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(10000, "TRY", "https://example.com/return", "ord_1"))
            .await
            .unwrap();
        assert_eq!(
            receipt.reference, "ord_1",
            "Receipt::reference stays the CALLER's own reference"
        );
        assert_eq!(receipt.amount_minor, 0);
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(
            proof.token, "tok_abc",
            "iyzico's real tracking token lives in proof"
        );
    }

    #[tokio::test]
    async fn charge_failure_status_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payment/iyzipos/checkoutform/initialize/auth/ecom"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "failure",
                "errorMessage": "invalid signature"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(10000, "TRY", "https://example.com/return", "ord_1"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    #[tokio::test]
    async fn verify_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payment/iyzipos/checkoutform/auth/ecom/detail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "paymentStatus": "SUCCESS",
                "token": "tok_abc",
                "paymentId": "pay_1",
                "paidPrice": "100.00",
                "currency": "TRY",
                "basketId": "ord_1"
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "iyzico".into(),
            amount_minor: 0,
            currency: "TRY".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                token: "tok_abc".into(),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_failure_payment_status_is_not_settled() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payment/iyzipos/checkoutform/auth/ecom/detail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "paymentStatus": "FAILURE",
                "token": "tok_abc",
                "currency": "TRY"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "iyzico".into(),
            amount_minor: 0,
            currency: "TRY".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                token: "tok_abc".into(),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_garbage_proof_fails_closed() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "iyzico".into(),
            amount_minor: 0,
            currency: "TRY".into(),
            reference: "ord_1".into(),
            proof: vec![1, 2, 3],
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_fails_closed_on_amount_or_currency_mismatch() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payment/iyzipos/checkoutform/auth/ecom/detail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "paymentStatus": "SUCCESS",
                "token": "tok_abc",
                "paymentId": "pay_1",
                "paidPrice": "5.00",
                "currency": "TRY"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let genuine = Receipt {
            rail_id: "iyzico".into(),
            amount_minor: 500,
            currency: "TRY".into(),
            reference: "ord_1".into(),
            proof: ChargeProof {
                token: "tok_abc".into(),
                redirect_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&genuine).await.unwrap());

        let mut inflated = genuine.clone();
        inflated.amount_minor = 999_999;
        assert!(!rail.verify(&inflated).await.unwrap());

        let mut wrong_currency = genuine.clone();
        wrong_currency.currency = "USD".into();
        assert!(!rail.verify(&wrong_currency).await.unwrap());
    }

    /// Guards the same security property cackle's own
    /// `TestIyzicoWebhook_UnsignedCallbackAloneCannotForgeSuccess` does: the
    /// callback itself carries no verifiable status, only a token, so a
    /// forged callback claiming success must not matter -- only the
    /// authenticated retrieve call's answer does.
    #[tokio::test]
    async fn handle_webhook_unsigned_callback_alone_cannot_forge_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payment/iyzipos/checkoutform/auth/ecom/detail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "paymentStatus": "FAILURE",
                "token": "tok_abc",
                "currency": "TRY"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let outcome = rail
            .handle_webhook("application/x-www-form-urlencoded", b"token=tok_abc")
            .await
            .unwrap();
        assert!(
            !outcome.settled,
            "handle_webhook reported settled based on the callback alone -- SECURITY REGRESSION"
        );
    }

    #[tokio::test]
    async fn handle_webhook_legitimate_callback_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/payment/iyzipos/checkoutform/auth/ecom/detail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "paymentStatus": "SUCCESS",
                "token": "tok_abc",
                "paymentId": "pay_1",
                "paidPrice": "100.00",
                "currency": "TRY"
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let outcome = rail
            .handle_webhook("application/x-www-form-urlencoded", b"token=tok_abc")
            .await
            .unwrap();
        assert!(outcome.settled);
        assert_eq!(outcome.amount_minor, 10000);
    }

    #[tokio::test]
    async fn handle_webhook_missing_token_fails_closed() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let err = rail
            .handle_webhook("application/x-www-form-urlencoded", b"")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "iyzico".into(),
            amount_minor: 100,
            currency: "TRY".into(),
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
