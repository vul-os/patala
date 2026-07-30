//! [`PayPalRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/paypal.go` (`PayPalProvider`), PayPal's Orders v2 API.
//!
//! Reference: <https://developer.paypal.com/docs/api/orders/v2/>. See
//! `models.rs`'s module doc for cackle's own HONESTY notes on unconfirmed
//! parts of the status enum/webhook shape, and this crate's `PORTING.md`
//! "UNVERIFIED AGAINST LIVE" note.
//!
//! ## `RailClass`/`holds_funds`
//!
//! `RailClass::CustodialReversible`, `holds_funds: true`, `reversible: true`
//! — fiat, exactly as the task brief specifies: PayPal disputes/chargebacks
//! are real, PayPal (the PROCESSOR) custodies funds in flight, never
//! patala.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (creates a PayPal Order, `intent=CAPTURE`, returns the
//!   buyer approval redirect URL) maps to [`PaymentRail::charge`]. Cackle's
//!   `Order.CallbackURL` is REQUIRED here (`"payments: paypal: callback_url
//!   is required"`) — same shape as `stripe::rail`'s identical gap — so
//!   this port reinterprets `PayRequest::destination` AS that callback/
//!   return URL (used for BOTH `return_url` and `cancel_url`, matching
//!   cackle exactly). `PayRequest::validate()` already requires
//!   `destination` non-empty, so cackle's explicit `callback_url is
//!   required` check is structurally unreachable here — not re-implemented,
//!   same convention `paystack::rail`'s module docs establish.
//! - cackle's `Verify(reference)` — which cackle's own comment says
//!   PERFORMS THE CAPTURE STEP (moves an `APPROVED` order to `COMPLETED`,
//!   i.e. this is the point money actually moves — cackle: *"Verify is
//!   where this adapter performs the capture step a client-side PayPal
//!   Buttons integration would otherwise trigger"*) — maps to
//!   [`PaymentRail::verify`]. **This port preserves that exactly: calling
//!   `verify()` on an `APPROVED` order has the real side effect of
//!   capturing it.** This is unusual for a method whose trait doc comment
//!   says it "must fail closed" and callers may be tempted to call
//!   speculatively/repeatedly — cackle's own design already accepts this
//!   trade-off (capture is itself idempotent on PayPal's side for an
//!   already-captured order, and re-calling `verify()` on an already-
//!   `COMPLETED` order takes the read-only `COMPLETED` branch, not a second
//!   capture), so this port does too, rather than silently redesigning
//!   cackle's flow. See `proof::ChargeProof`'s module docs for how the
//!   caller-reference-vs-order-id structural gap is resolved.
//! - **HONESTY note 1 (from cackle's own file doc comment, `models.rs`)**:
//!   `CREATED`/`PAYER_ACTION_REQUIRED`/`VOIDED`/anything unrecognised all
//!   map to `Ok(false)` here (never an error) — an incomplete enum can only
//!   make this adapter too conservative, never wrongly permissive.
//! - **Reconciliation**: since this crate has no `OrderLookup`/`Reconcile`
//!   seam (cackle's is a layer above `Provider` itself, in
//!   `internal/payments/provider.go`), `verify()`/`refund()` perform the
//!   equivalent check inline: the CAPTURED purchase unit's own
//!   `custom_id`/`reference_id` must equal `receipt.reference`, and its
//!   settled amount/currency must satisfy the usual `>=`/exact-match rules
//!   (`PORTING.md` §6) before ever reporting success.
//! - cackle's `Webhook` — a genuine server-to-server signature verification
//!   round trip, not a local HMAC — is ported as
//!   [`PayPalRail::handle_webhook`], an inherent method rather than a free
//!   function in `webhook.rs`. See `webhook.rs`'s module docs for why this
//!   is the one deliberate exception to this crate's usual "free function"
//!   convention. After PayPal's own verification succeeds, this port trusts
//!   the (now-verified) event body's own capture fields directly — same as
//!   `stripe`/`paystack`'s webhook modules (NOT the refetch-then-trust-
//!   nothing pattern `btcpay`/`lnbits`/`opennode`/`coinbasecommerce` use) —
//!   because cackle's own `paypal.go` `Webhook` does the same: it never
//!   refetches the order after a successful signature verification.
//! - `refund()`: **NOT a cackle port** — cackle's `Provider` interface has
//!   no `Refund` method at all (`provider.go`). Unlike `btcpay`/`lnbits`/
//!   `opennode`/`coinbasecommerce` (where cackle's own `Capabilities.Refunds`
//!   is `false` with no "supports it" signal, so this crate leaves
//!   `refund()` `Unsupported`), cackle's PayPal `Capabilities.Refunds` is
//!   `true` — a stronger, explicit signal (per `PORTING.md` §7's exact
//!   rule) that this is a legitimate gap to fill with new code. Grounded
//!   directly in PayPal's own public Captures Refund API
//!   (<https://developer.paypal.com/docs/api/payments/v2/#captures_refund>),
//!   same honesty conventions as every ported method here (pending/async
//!   refund reports `amount_minor: 0` until PayPal confirms `COMPLETED`).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::paypal::config::PayPalConfig;
use crate::paypal::models::{self, Capture, OrderResponse, PurchaseUnit};
use crate::paypal::proof::{ChargeProof, RefundProof};
use crate::paypal::webhook::{self, PayPalWebhookError, PayPalWebhookEvent, PayPalWebhookHeaders};

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for a paypal id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` talking to PayPal's Orders v2 API. See module docs for
/// the full `Provider` -> `PaymentRail` mapping.
pub struct PayPalRail {
    id: String,
    config: PayPalConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // overridable in tests only
}

impl PayPalRail {
    /// Build a rail from configuration. Fails if any required field is
    /// empty.
    pub fn new(config: PayPalConfig) -> Result<Self> {
        if config.client_id.trim().is_empty() {
            return Err(Error::InvalidRequest("client_id must not be empty".into()));
        }
        if config.client_secret.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "client_secret must not be empty".into(),
            ));
        }
        if config.webhook_id.trim().is_empty() {
            return Err(Error::InvalidRequest("webhook_id must not be empty".into()));
        }
        if config.base_url.trim().is_empty() {
            return Err(Error::InvalidRequest("base_url must not be empty".into()));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building paypal http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true, // PayPal disputes/chargebacks are real.
            requires_kyc: config.requires_kyc,
            holds_funds: true, // PayPal (the PROCESSOR) custodies funds in flight -- never patala.
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
            atomic_multi_party: false, // always false: N payouts here are N independent API calls, never one atomic event (B3)
        };

        let base_url = config.base_url.clone();
        Ok(Self {
            id: "paypal".to_string(),
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

    /// Mirrors cackle's `fetchAccessToken`: a fresh OAuth2 client-credentials
    /// token per call, not cached — cackle's own comment: simpler and
    /// unambiguously correct; caching is a follow-up optimisation, not a
    /// correctness requirement.
    /// <https://developer.paypal.com/api/rest/authentication/>
    async fn fetch_access_token(&self) -> Result<String> {
        let url = format!("{}/v1/oauth2/token", self.base_url);
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.config.client_id, Some(&self.config.client_secret))
            .header("Accept", "application/json")
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
            .map_err(|e| Error::Rail(format!("paypal: token request failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("paypal: read token response: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("paypal: {e}")))?;
        if !(200..300).contains(&status) {
            return Err(Error::Rail(format!("paypal: token endpoint http {status}")));
        }
        #[derive(Deserialize, Default)]
        struct TokenResp {
            #[serde(default)]
            access_token: String,
        }
        let parsed: TokenResp =
            serde_json::from_slice(&bytes).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.access_token.is_empty() {
            return Err(models::malformed("empty access_token"));
        }
        Ok(parsed.access_token)
    }

    async fn do_json(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
        access_token: &str,
    ) -> Result<(Vec<u8>, u16)> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .http
            .request(method, &url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rail(format!("paypal: request to {path} failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("paypal: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("paypal: {e}")))?;
        Ok((bytes.to_vec(), status))
    }

    /// Finds the first `COMPLETED` capture across `order.purchase_units`,
    /// mirrors cackle's `paypalResultFromCaptures`'s search (it grabs the
    /// FIRST completed capture found, trusting a caller-side `Reconcile` to
    /// catch a reference mismatch — this crate performs that reconciliation
    /// itself, inline, see below).
    fn find_completed_capture(order: &OrderResponse) -> Option<(&PurchaseUnit, &Capture)> {
        for unit in &order.purchase_units {
            for capture in &unit.payments.captures {
                if capture.status == "COMPLETED" {
                    return Some((unit, capture));
                }
            }
        }
        None
    }

    /// Mirrors cackle's `ref := u.CustomID; if ref == "" { ref =
    /// u.ReferenceID }`.
    fn unit_reference(unit: &PurchaseUnit) -> &str {
        if !unit.custom_id.is_empty() {
            &unit.custom_id
        } else {
            &unit.reference_id
        }
    }

    /// Shared by `verify()` and `refund()`: given an Order already known to
    /// have reached `COMPLETED` (directly, or via a just-performed
    /// capture), find ITS completed capture, check it's for `receipt`
    /// (reference/currency/amount), and return `(capture_id, amount_minor)`
    /// on success. `Ok(None)` means "not a match for this receipt" (fail
    /// closed, not an error); `Err` means the order is internally
    /// inconsistent (cackle's own "order has no COMPLETED capture" error).
    fn match_capture_for_receipt(
        order: &OrderResponse,
        receipt: &Receipt,
    ) -> Result<Option<(String, u64)>> {
        let Some((unit, capture)) = Self::find_completed_capture(order) else {
            return Err(models::malformed(&format!(
                "order {} has no COMPLETED capture",
                order.id
            )));
        };
        if Self::unit_reference(unit) != receipt.reference {
            return Ok(None);
        }
        let currency = capture.amount.currency_code.to_ascii_uppercase();
        if !currency.eq_ignore_ascii_case(&receipt.currency) {
            return Ok(None);
        }
        let amount_minor = models::paypal_amount_value_to_minor(&capture.amount.value, &currency)?;
        if amount_minor == 0 {
            return Err(models::malformed(
                "completed capture with non-positive amount",
            ));
        }
        if amount_minor < receipt.amount_minor {
            return Ok(None);
        }
        Ok(Some((capture.id.clone(), amount_minor)))
    }

    /// Verify a webhook delivery. See module docs and `webhook.rs`'s module
    /// docs for why this is an inherent method, not a free function.
    pub async fn handle_webhook(
        &self,
        raw_body: &[u8],
        headers: PayPalWebhookHeaders<'_>,
    ) -> std::result::Result<PayPalWebhookEvent, PayPalWebhookError> {
        if !headers.all_present() {
            return Err(PayPalWebhookError::MissingSignatureHeaders);
        }
        let body_json: serde_json::Value = serde_json::from_slice(raw_body).map_err(|_| {
            PayPalWebhookError::MalformedResponse("webhook body is not valid JSON".to_string())
        })?;

        let token = self
            .fetch_access_token()
            .await
            .map_err(|e| PayPalWebhookError::UnexpectedStatus(e.to_string()))?;

        let verify_req = json!({
            "transmission_id": headers.transmission_id,
            "transmission_time": headers.transmission_time,
            "cert_url": headers.cert_url,
            "auth_algo": headers.auth_algo,
            "transmission_sig": headers.transmission_sig,
            "webhook_id": self.config.webhook_id,
            "webhook_event": body_json,
        });
        let (resp_body, status) = self
            .do_json(
                reqwest::Method::POST,
                "/v1/notifications/verify-webhook-signature",
                Some(&verify_req),
                &token,
            )
            .await
            .map_err(|e| PayPalWebhookError::UnexpectedStatus(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(PayPalWebhookError::UnexpectedStatus(format!(
                "verify-webhook-signature http {status}"
            )));
        }
        #[derive(Deserialize, Default)]
        struct VerifyResponse {
            #[serde(default)]
            verification_status: String,
        }
        let verify_resp: VerifyResponse = serde_json::from_slice(&resp_body)
            .map_err(|e| PayPalWebhookError::MalformedResponse(format!("verify response: {e}")))?;
        if verify_resp.verification_status != "SUCCESS" {
            return Err(PayPalWebhookError::InvalidSignature);
        }

        let parsed = webhook::parse_capture_completed(raw_body)?;
        let currency = parsed.currency_code.to_ascii_uppercase();
        let amount_minor = models::paypal_amount_value_to_minor(&parsed.value, &currency)
            .map_err(|e| PayPalWebhookError::MalformedResponse(e.to_string()))?;
        if amount_minor == 0 {
            return Err(PayPalWebhookError::MalformedResponse(
                "non-positive amount".to_string(),
            ));
        }
        Ok(PayPalWebhookEvent {
            event_id: parsed.event_id,
            reference: parsed.custom_id,
            amount_minor,
            currency,
        })
    }
}

#[async_trait]
impl PaymentRail for PayPalRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::redirect_url`], because on the `paypal` rail
    /// `destination` is not a payout address: it is the post-checkout return
    /// URL, sent as PayPal's `return_url`/`cancel_url` (see this module's docs above).
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
        // See module docs: `destination` is reinterpreted as the callback
        // (return/cancel) URL, which PayPal's Orders v2 API requires.
        let currency = req.currency.trim().to_ascii_uppercase();
        let value = models::paypal_amount_value(req.amount_minor, &currency)?;

        let token = self.fetch_access_token().await?;
        let body = json!({
            "intent": "CAPTURE",
            "purchase_units": [{
                "reference_id": req.reference,
                "custom_id": req.reference,
                "amount": {"currency_code": currency, "value": value},
            }],
            "application_context": {
                "return_url": req.destination,
                "cancel_url": req.destination,
            },
        });

        let (resp_body, status) = self
            .do_json(
                reqwest::Method::POST,
                "/v2/checkout/orders",
                Some(&body),
                &token,
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }
        #[derive(Deserialize, Default)]
        struct CreateResp {
            #[serde(default)]
            id: String,
            #[serde(default)]
            links: Vec<models::Link>,
        }
        let parsed: CreateResp =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if parsed.id.is_empty() {
            return Err(models::malformed("empty order id"));
        }
        let approve_url = parsed
            .links
            .iter()
            .find(|l| l.rel == "approve")
            .map(|l| l.href.clone());
        let Some(approve_url) = approve_url else {
            return Err(models::malformed("no approve link in order response"));
        };

        let proof = ChargeProof {
            paypal_order_id: parsed.id,
            approve_url: Some(approve_url),
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
        let order_id = safe_path_segment(&proof.paypal_order_id)?;

        let token = self.fetch_access_token().await?;
        let path = format!("/v2/checkout/orders/{order_id}");
        let (resp_body, status) = self
            .do_json(reqwest::Method::GET, &path, None, &token)
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }
        let order: OrderResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        if order.id.is_empty() {
            return Err(models::malformed("missing order id"));
        }

        match order.status.as_str() {
            "COMPLETED" => match Self::match_capture_for_receipt(&order, receipt)? {
                Some(_) => Ok(true),
                None => Ok(false),
            },
            "APPROVED" => {
                // Buyer has approved -- capture now. This is the point at
                // which money actually moves; see module docs.
                let cap_path = format!("/v2/checkout/orders/{order_id}/capture");
                let (cap_body, cap_status) = self
                    .do_json(reqwest::Method::POST, &cap_path, Some(&json!({})), &token)
                    .await?;
                if !(200..300).contains(&cap_status) {
                    return Err(models::classify_error(cap_status, &cap_body));
                }
                let captured: OrderResponse = serde_json::from_slice(&cap_body)
                    .map_err(|e| models::malformed(&format!("capture response: {e}")))?;
                if captured.status != "COMPLETED" {
                    // Fail closed: a capture call that didn't complete is
                    // not paid, whatever status it did come back with.
                    return Ok(false);
                }
                match Self::match_capture_for_receipt(&captured, receipt)? {
                    Some(_) => Ok(true),
                    None => Ok(false),
                }
            }
            // CREATED, PAYER_ACTION_REQUIRED, VOIDED, or anything
            // unrecognised: never paid. See HONESTY note 1 in module docs.
            _ => Ok(false),
        }
    }

    /// New code (see module docs): PayPal's Captures Refund API.
    /// <https://developer.paypal.com/docs/api/payments/v2/#captures_refund>
    async fn refund(&self, receipt: &Receipt) -> Result<Receipt> {
        if receipt.rail_id != self.id {
            return Err(Error::InvalidRequest(format!(
                "receipt names rail {:?}, not {:?}",
                receipt.rail_id, self.id
            )));
        }
        let proof = ChargeProof::from_bytes(&receipt.proof).ok_or_else(|| {
            Error::InvalidRequest("receipt proof is not a paypal charge proof".into())
        })?;
        let order_id = safe_path_segment(&proof.paypal_order_id)?;

        let token = self.fetch_access_token().await?;
        let path = format!("/v2/checkout/orders/{order_id}");
        let (resp_body, status) = self
            .do_json(reqwest::Method::GET, &path, None, &token)
            .await?;
        if !(200..300).contains(&status) {
            return Err(models::classify_error(status, &resp_body));
        }
        let order: OrderResponse =
            serde_json::from_slice(&resp_body).map_err(|e| models::malformed(&e.to_string()))?;
        let Some((unit, capture)) = Self::find_completed_capture(&order) else {
            return Err(Error::Rail(format!(
                "paypal: order {} has no COMPLETED capture to refund",
                order.id
            )));
        };
        if Self::unit_reference(unit) != receipt.reference {
            return Err(Error::Rail(
                "paypal: order's captured purchase unit does not match this receipt's reference"
                    .to_string(),
            ));
        }
        let capture_id = safe_path_segment(&capture.id)?;

        let value = models::paypal_amount_value(receipt.amount_minor, &receipt.currency)?;
        let refund_path = format!("/v2/payments/captures/{capture_id}/refund");
        let (refund_body, refund_status) = self
            .do_json(
                reqwest::Method::POST,
                &refund_path,
                Some(&json!({"amount": {"currency_code": receipt.currency, "value": value}})),
                &token,
            )
            .await?;
        if !(200..300).contains(&refund_status) {
            return Err(models::classify_error(refund_status, &refund_body));
        }

        #[derive(Deserialize, Default)]
        struct RefundResponse {
            #[serde(default)]
            id: String,
            #[serde(default)]
            status: String,
            #[serde(default)]
            amount: models::CaptureAmount,
        }
        let parsed: RefundResponse =
            serde_json::from_slice(&refund_body).map_err(|e| models::malformed(&e.to_string()))?;

        // PayPal's documented Refund object status values: "COMPLETED",
        // "PENDING", "CANCELLED", "FAILED". Only "COMPLETED" is ever
        // reported as money having actually moved back -- same fail-closed
        // convention as every other rail in this crate.
        let succeeded = parsed.status == "COMPLETED";
        let refund_currency = if parsed.amount.currency_code.is_empty() {
            receipt.currency.clone()
        } else {
            parsed.amount.currency_code.to_ascii_uppercase()
        };
        let amount_minor = if succeeded {
            models::paypal_amount_value_to_minor(&parsed.amount.value, &refund_currency)?
        } else {
            0
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor,
            currency: refund_currency,
            reference: receipt.reference.clone(),
            proof: RefundProof {
                refund_id: parsed.id,
                status_at_refund: parsed.status,
            }
            .to_bytes(),
            settled_at_unix: now_unix(),
        })
    }

    /// Verify a PayPal webhook delivery — delegates to
    /// [`Self::handle_webhook`], which calls PayPal's own
    /// `/v1/notifications/verify-webhook-signature` endpoint (PayPal signs
    /// with a rotating certificate chain, so unlike every other adapter here
    /// verification is a network call, not a shared-secret HMAC).
    ///
    /// Headers: the five `PAYPAL-TRANSMISSION-ID`,
    /// `PAYPAL-TRANSMISSION-TIME`, `PAYPAL-TRANSMISSION-SIG`,
    /// `PAYPAL-CERT-URL`, `PAYPAL-AUTH-ALGO`. The parser only produces an
    /// event for `PAYMENT.CAPTURE.COMPLETED`, so a delivery that reaches
    /// this point is settled.
    ///
    /// A transport/endpoint failure surfaces as [`Error::Rail`] (this rail
    /// could not perform the check) and a rejected or malformed delivery as
    /// [`Error::InvalidRequest`] — the same split the trait draws between an
    /// operational failure and a bad request.
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let headers = crate::paypal::webhook::PayPalWebhookHeaders {
            transmission_id: delivery.header_or_empty("PAYPAL-TRANSMISSION-ID"),
            transmission_time: delivery.header_or_empty("PAYPAL-TRANSMISSION-TIME"),
            cert_url: delivery.header_or_empty("PAYPAL-CERT-URL"),
            auth_algo: delivery.header_or_empty("PAYPAL-AUTH-ALGO"),
            transmission_sig: delivery.header_or_empty("PAYPAL-TRANSMISSION-SIG"),
        };
        let event = self
            .handle_webhook(&delivery.raw_body, headers)
            .await
            .map_err(|e| match e {
                crate::paypal::PayPalWebhookError::UnexpectedStatus(_) => {
                    Error::Rail(e.to_string())
                }
                other => Error::InvalidRequest(other.to_string()),
            })?;
        Ok(WebhookEvent::settlement(
            &self.id,
            event.event_id,
            event.reference,
            true,
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

    fn config(base_url: String) -> PayPalConfig {
        PayPalConfig {
            client_id: "test-client-id".to_string(),
            client_secret: "test-client-secret".to_string(),
            webhook_id: "WH-TEST-1".to_string(),
            base_url,
            requires_kyc: true,
            currencies: Vec::new(),
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> PayPalRail {
        PayPalRail::new(config(base_url)).unwrap()
    }

    async fn mount_token(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/v1/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "A21AAtest", "token_type": "Bearer", "expires_in": 32400
            })))
            .mount(server)
            .await;
    }

    fn req(amount: u64, currency: &str, reference: &str, callback: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: callback.into(),
            reference: reference.into(),
        }
    }

    // Ported from cackle's internal/payments/paypal_test.go.

    #[test]
    fn capabilities_are_fiat_custodial_reversible() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds);
        assert!(caps.reversible);
    }

    #[tokio::test]
    async fn charge_success() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("POST"))
            .and(path("/v2/checkout/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "5O190127TN364715T",
                "status": "CREATED",
                "links": [{"rel": "approve", "href": "https://www.paypal.com/checkoutnow?token=5O190127TN364715T"}]
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = rail
            .charge(&req(5000, "USD", "ord_1", "https://example.com/return"))
            .await
            .unwrap();
        assert_eq!(receipt.reference, "ord_1");
        assert_eq!(receipt.amount_minor, 0);
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.paypal_order_id, "5O190127TN364715T");
        assert_eq!(
            proof.approve_url.as_deref(),
            Some("https://www.paypal.com/checkoutnow?token=5O190127TN364715T")
        );
    }

    #[tokio::test]
    async fn charge_no_approve_link_fails_closed() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("POST"))
            .and(path("/v2/checkout/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ORDER1", "status": "CREATED",
                "links": [{"rel": "self", "href": "https://api.paypal.com/v2/checkout/orders/ORDER1"}]
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        assert!(rail
            .charge(&req(5000, "USD", "ord_1", "https://example.com"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn charge_refuses_three_decimal_currency_without_calling_server() {
        // No Mock registered for /v2/checkout/orders or /v1/oauth2/token --
        // wiremock panics on an unmatched request, proving the adapter
        // short-circuits before touching the network.
        let server = MockServer::start().await;
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(1000, "KWD", "ord_1", "https://example.com"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn charge_http_500_fails_closed() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("POST"))
            .and(path("/v2/checkout/orders"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(json!({"name": "INTERNAL_SERVER_ERROR", "message": "boom"})),
            )
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(5000, "USD", "ord_1", "https://example.com"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Rail(msg) if msg.contains("boom")));
    }

    #[tokio::test]
    async fn charge_malformed_json_fails_closed() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("POST"))
            .and(path("/v2/checkout/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        assert!(rail
            .charge(&req(5000, "USD", "ord_1", "https://example.com"))
            .await
            .is_err());
    }

    fn receipt_with(amount_minor: u64, currency: &str, order_id: &str, reference: &str) -> Receipt {
        Receipt {
            rail_id: "paypal".into(),
            amount_minor,
            currency: currency.into(),
            reference: reference.into(),
            proof: ChargeProof {
                paypal_order_id: order_id.into(),
                approve_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        }
    }

    #[tokio::test]
    async fn verify_approved_then_captured() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/ORDER1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ORDER1", "status": "APPROVED", "purchase_units": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v2/checkout/orders/ORDER1/capture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ORDER1", "status": "COMPLETED",
                "purchase_units": [{
                    "reference_id": "ord_1", "custom_id": "ord_1",
                    "payments": {"captures": [{"id": "CAP1", "status": "COMPLETED",
                                                "amount": {"currency_code": "USD", "value": "50.00"}}]}
                }]
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = receipt_with(5000, "USD", "ORDER1", "ord_1");
        assert!(rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_created_is_not_paid() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/ORDER1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ORDER1", "status": "CREATED", "purchase_units": []
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(5000, "USD", "ORDER1", "ord_1");
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_capture_not_completed_fails_closed() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/ORDER1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ORDER1", "status": "APPROVED", "purchase_units": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v2/checkout/orders/ORDER1/capture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ORDER1", "status": "PENDING", "purchase_units": []
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(5000, "USD", "ORDER1", "ord_1");
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_http_500_fails_closed() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/ORDER1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(5000, "USD", "ORDER1", "ord_1");
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_malformed_json_fails_closed() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/ORDER1"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(5000, "USD", "ORDER1", "ord_1");
        assert!(rail.verify(&receipt).await.is_err());
    }

    #[tokio::test]
    async fn verify_reference_mismatch_fails_closed() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/ORDER1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ORDER1", "status": "COMPLETED",
                "purchase_units": [{
                    "reference_id": "some_other_order", "custom_id": "some_other_order",
                    "payments": {"captures": [{"id": "CAP1", "status": "COMPLETED",
                                                "amount": {"currency_code": "USD", "value": "50.00"}}]}
                }]
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(5000, "USD", "ORDER1", "ord_1");
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn verify_amount_mismatch_fails_closed() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/ORDER1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ORDER1", "status": "COMPLETED",
                "purchase_units": [{
                    "reference_id": "ord_1", "custom_id": "ord_1",
                    "payments": {"captures": [{"id": "CAP1", "status": "COMPLETED",
                                                "amount": {"currency_code": "USD", "value": "50.00"}}]}
                }]
            })))
            .mount(&server)
            .await;
        let rail = rail_for(server.uri());
        let receipt = receipt_with(999_999, "USD", "ORDER1", "ord_1");
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn refund_success() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/ORDER1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ORDER1", "status": "COMPLETED",
                "purchase_units": [{
                    "reference_id": "ord_1", "custom_id": "ord_1",
                    "payments": {"captures": [{"id": "CAP1", "status": "COMPLETED",
                                                "amount": {"currency_code": "USD", "value": "50.00"}}]}
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v2/payments/captures/CAP1/refund"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "REFUND1", "status": "COMPLETED",
                "amount": {"currency_code": "USD", "value": "50.00"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = receipt_with(5000, "USD", "ORDER1", "ord_1");
        let refunded = rail.refund(&receipt).await.unwrap();
        assert_eq!(refunded.amount_minor, 5000);
        assert_eq!(refunded.currency, "USD");
    }

    #[tokio::test]
    async fn refund_pending_is_not_yet_moved() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/ORDER1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "ORDER1", "status": "COMPLETED",
                "purchase_units": [{
                    "reference_id": "ord_1", "custom_id": "ord_1",
                    "payments": {"captures": [{"id": "CAP1", "status": "COMPLETED",
                                                "amount": {"currency_code": "USD", "value": "50.00"}}]}
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v2/payments/captures/CAP1/refund"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "REFUND1", "status": "PENDING",
                "amount": {"currency_code": "USD", "value": "50.00"}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = receipt_with(5000, "USD", "ORDER1", "ord_1");
        let refunded = rail.refund(&receipt).await.unwrap();
        assert_eq!(refunded.amount_minor, 0);
    }

    #[tokio::test]
    async fn refund_rejects_a_receipt_from_a_different_rail() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let mut foreign = receipt_with(100, "USD", "ORDER1", "ord_1");
        foreign.rail_id = "stripe".into();
        assert!(rail.refund(&foreign).await.is_err());
    }

    // --- Webhook ---------------------------------------------------------

    fn webhook_headers(valid: bool) -> PayPalWebhookHeaders<'static> {
        if valid {
            PayPalWebhookHeaders {
                transmission_id: "tx-1",
                transmission_time: "2026-07-20T10:00:00Z",
                cert_url: "https://api.paypal.com/cert.pem",
                auth_algo: "SHA256withRSA",
                transmission_sig: "fake-sig",
            }
        } else {
            PayPalWebhookHeaders {
                transmission_id: "",
                transmission_time: "",
                cert_url: "",
                auth_algo: "",
                transmission_sig: "",
            }
        }
    }

    fn capture_completed_body(id: &str, capture_id: &str, reference: &str) -> Vec<u8> {
        format!(
            r#"{{"id":{id:?},"event_type":"PAYMENT.CAPTURE.COMPLETED","resource":{{"id":{capture_id:?},"status":"COMPLETED","custom_id":{reference:?},"amount":{{"currency_code":"USD","value":"50.00"}}}}}}"#
        )
        .into_bytes()
    }

    async fn mount_verify(server: &MockServer, verification_status: &str) {
        Mock::given(method("POST"))
            .and(path("/v1/notifications/verify-webhook-signature"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "verification_status": verification_status
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn webhook_success() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        mount_verify(&server, "SUCCESS").await;

        let rail = rail_for(server.uri());
        let body = capture_completed_body("WH-EVT-1", "CAP1", "ord_1");
        let event = rail
            .handle_webhook(&body, webhook_headers(true))
            .await
            .unwrap();
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.amount_minor, 5000);
        assert_eq!(event.currency, "USD");
        assert_eq!(event.event_id, "WH-EVT-1");
    }

    #[tokio::test]
    async fn webhook_missing_headers_fails_closed() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let body = capture_completed_body("WH-EVT-1", "CAP1", "ord_1");
        let err = rail
            .handle_webhook(&body, webhook_headers(false))
            .await
            .unwrap_err();
        assert_eq!(err, PayPalWebhookError::MissingSignatureHeaders);
    }

    #[tokio::test]
    async fn webhook_verification_failure_fails_closed() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        mount_verify(&server, "FAILURE").await;

        let rail = rail_for(server.uri());
        let body = capture_completed_body("WH-EVT-1", "CAP1", "ord_1");
        let err = rail
            .handle_webhook(&body, webhook_headers(true))
            .await
            .unwrap_err();
        assert_eq!(err, PayPalWebhookError::InvalidSignature);
    }

    #[tokio::test]
    async fn webhook_verify_endpoint_500_fails_closed() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/notifications/verify-webhook-signature"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let body = capture_completed_body("WH-EVT-1", "CAP1", "ord_1");
        let err = rail
            .handle_webhook(&body, webhook_headers(true))
            .await
            .unwrap_err();
        assert!(matches!(err, PayPalWebhookError::UnexpectedStatus(_)));
    }

    #[tokio::test]
    async fn webhook_malformed_json_fails_closed() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let err = rail
            .handle_webhook(b"not json", webhook_headers(true))
            .await
            .unwrap_err();
        assert!(matches!(err, PayPalWebhookError::MalformedResponse(_)));
    }

    #[tokio::test]
    async fn webhook_unhandled_event_type() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        mount_verify(&server, "SUCCESS").await;

        let rail = rail_for(server.uri());
        let body = br#"{"id":"WH-EVT-1","event_type":"PAYMENT.CAPTURE.REFUNDED","resource":{}}"#;
        let err = rail
            .handle_webhook(body, webhook_headers(true))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            PayPalWebhookError::UnhandledEvent("PAYMENT.CAPTURE.REFUNDED".to_string())
        );
    }

    #[tokio::test]
    async fn webhook_replayed_event_produces_stable_event_id() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        mount_verify(&server, "SUCCESS").await;

        let rail = rail_for(server.uri());
        let body = capture_completed_body("WH-EVT-1", "CAP1", "ord_1");
        let mut ids = Vec::new();
        for _ in 0..2 {
            let event = rail
                .handle_webhook(&body, webhook_headers(true))
                .await
                .unwrap();
            ids.push(event.event_id);
        }
        assert!(!ids[0].is_empty());
        assert_eq!(ids[0], ids[1]);
    }
}
