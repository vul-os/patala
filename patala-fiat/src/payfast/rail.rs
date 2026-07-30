//! [`PayFastRail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/payfast.go` (`PayFastProvider`).
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (builds PayFast's signed Onsite/Redirect field set)
//!   maps to [`PaymentRail::charge`]. **Gap vs cackle**: PayFast's
//!   `return_url`/`cancel_url` are optional but structurally important for
//!   a redirect flow; `PayRequest::destination` is reinterpreted as that
//!   callback/return URL, same choice as `stripe`/`yoco`.
//! - **`notify_url` is a disclosed, un-fillable gap, exactly as in
//!   cackle**: cackle's own `Begin` sends `notify_url: ""` with the comment
//!   *"caller's own wiring; filled in by the caller's own wiring, not this
//!   package"* — `patala_core::PayRequest` has no webhook-URL field either,
//!   so this port sends the identical empty value. A production integrator
//!   MUST fill this in themselves (e.g. by re-signing the field set with
//!   their own `notify_url` before rendering the form) — this crate does
//!   not have enough information to do it.
//! - **Bigger `patala_core::Receipt` gap than any other rail in this
//!   crate**: PayFast's canonical integration is an HTML form auto-POST,
//!   not a redirect link — see `proof.rs`'s module docs (mirrors cackle's
//!   own loud comment on `Charge.RedirectURL`/`Charge.Instructions`
//!   verbatim).
//! - cackle's ITN `Webhook` (signature check + mandatory
//!   `confirmWithPayFast` validate round trip + field mapping) is ported as
//!   [`PayFastRail::handle_itn`] — a RAIL METHOD, not a free function,
//!   because the validate round trip needs an HTTP client. See
//!   `webhook.rs`'s module docs for why this is a necessary, protocol-driven
//!   divergence (same class as `iyzico::rail::handle_webhook`), not an
//!   arbitrary one.
//! - **`Verify` is NOT implemented — a genuine, disclosed structural gap,
//!   not an oversight.** Cackle's own `Verify` returns a hard error: PayFast
//!   has no documented "fetch a transaction by reference" polling endpoint
//!   the way Paystack/Flutterwave do. `patala_core::PaymentRail::verify`
//!   has NO default implementation (unlike `refund`) — every rail MUST
//!   answer it — so this port's [`PaymentRail::verify`] always returns
//!   `Err(Error::Unsupported(...))`. This is NOT the same as "in doubt,
//!   return `Ok(false)`" (`PORTING.md` §6): PayFast genuinely cannot answer
//!   this question via polling AT ALL, which is itself an operational
//!   limitation (the class of thing `Err` is for), not a content-level
//!   "not settled yet" verdict. Callers integrating this rail MUST rely on
//!   [`PayFastRail::handle_itn`] instead, which already re-confirms
//!   server-side via PayFast's own `validate` endpoint.
//! - `refund()`: **not implemented.** Cackle's `Capabilities().Refunds` is
//!   `false` for PayFast, and PayFast has no publicly documented merchant
//!   Refund API (refunds are handled via the PayFast dashboard/support, not
//!   an API) — same reasoning as the other regional adapters in this
//!   batch, stated more strongly here since there is genuinely nothing to
//!   cite. Returns the trait default (`Error::Unsupported`).

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::payfast::config::PayFastConfig;
use crate::payfast::models::{self, Kv};
use crate::payfast::proof::ChargeProof;
use crate::payfast::webhook::PayFastNotificationEvent;

/// Mirrors cackle's `payFastProcessURL`.
const PAYFAST_PROCESS_URL: &str = "https://www.payfast.co.za/eng/process";
/// Mirrors cackle's `payFastValidateURL`.
const PAYFAST_VALIDATE_URL: &str = "https://www.payfast.co.za/eng/query/validate";

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One `PaymentRail` talking to PayFast's Onsite payment flow and ITN
/// webhook. See module docs for the full `Provider` -> `PaymentRail`
/// mapping.
pub struct PayFastRail {
    id: String,
    config: PayFastConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    process_url: String,
    validate_url: String, // overridable in tests only
}

impl PayFastRail {
    /// Build a rail from configuration. Fails if `merchant_id` or
    /// `merchant_key` are empty.
    pub fn new(config: PayFastConfig) -> Result<Self> {
        if config.merchant_id.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "merchant_id must not be empty".into(),
            ));
        }
        if config.merchant_key.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "merchant_key must not be empty".into(),
            ));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building payfast http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            holds_funds: true, // PayFast (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: vec!["ZAR".to_string()], // hardcoded, matches cackle -- see config.rs
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "payfast".to_string(),
            config,
            http,
            capabilities,
            process_url: PAYFAST_PROCESS_URL.to_string(),
            validate_url: PAYFAST_VALIDATE_URL.to_string(),
        })
    }

    /// Handle a PayFast ITN. First verifies the signature and parses the
    /// settlement outcome (pure, [`crate::payfast::webhook::verify_and_parse`]),
    /// then performs cackle's mandatory `confirmWithPayFast` server-to-
    /// server round trip, requiring the literal response `"VALID"` before
    /// returning ANY outcome — never a signature-only verdict. See module
    /// docs for why this is a rail method, not a free function.
    pub async fn handle_itn(&self, raw_body: &[u8]) -> Result<PayFastNotificationEvent> {
        crate::httpshared::bounded_len_check(raw_body, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("payfast: {e}")))?;
        let event = crate::payfast::webhook::verify_and_parse(&self.config.passphrase, raw_body)
            .map_err(|e| Error::Rail(e.to_string()))?;
        self.confirm_with_payfast(raw_body).await?;
        Ok(event)
    }

    /// Mirrors cackle's `confirmWithPayFast`: POST the exact same raw ITN
    /// payload back to PayFast's `validate` endpoint and require the
    /// literal response body `"VALID"`.
    async fn confirm_with_payfast(&self, raw_body: &[u8]) -> Result<()> {
        let resp = self
            .http
            .post(&self.validate_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(raw_body.to_vec())
            .send()
            .await
            .map_err(|e| Error::Rail(format!("payfast: validate request failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("payfast: failed reading validate response: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("payfast: {e}")))?;
        if !(200..300).contains(&status) {
            return Err(models::unexpected_status(&format!(
                "validate endpoint returned http {status}"
            )));
        }
        let body_str = String::from_utf8_lossy(&bytes);
        if body_str.trim() != "VALID" {
            return Err(Error::Rail(
                "payfast: server-side validate confirmation did not return VALID".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl PaymentRail for PayFastRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::redirect_url`], because on the `payfast` rail
    /// `destination` is not a payout address: it is the post-checkout return
    /// URL, sent as PayFast's `return_url`/`cancel_url` (see this module's docs above).
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
        if !req.currency.eq_ignore_ascii_case("ZAR") {
            return Err(models::unsupported_currency(&req.currency));
        }
        crate::currency::minor_to_major_string(req.amount_minor, "ZAR")
            .map_err(|e| Error::InvalidRequest(e.to_string()))?;

        // NEEDS-CONFIRMATION (mirrors every other rail's identical note):
        // PayFast's documented API has no pre-charge fee-quote endpoint,
        // and cackle's own adapter has no Quote-equivalent method either.
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
        let amount_str = crate::currency::minor_to_major_string(req.amount_minor, "ZAR")
            .map_err(|e| Error::InvalidRequest(e.to_string()))?;
        // See module docs: `destination` is reinterpreted as the
        // return_url/cancel_url PayFast's redirect flow uses.
        let callback = req.destination.trim().to_string();

        let fields = vec![
            Kv {
                key: "merchant_id".into(),
                value: self.config.merchant_id.clone(),
            },
            Kv {
                key: "merchant_key".into(),
                value: self.config.merchant_key.clone(),
            },
            Kv {
                key: "return_url".into(),
                value: callback.clone(),
            },
            Kv {
                key: "cancel_url".into(),
                value: callback,
            },
            // Caller's own webhook route; not filled in by this package --
            // see module docs.
            Kv {
                key: "notify_url".into(),
                value: String::new(),
            },
            Kv {
                key: "m_payment_id".into(),
                value: req.reference.clone(),
            },
            Kv {
                key: "amount".into(),
                value: amount_str,
            },
            Kv {
                key: "item_name".into(),
                value: format!("Order {}", req.reference),
            },
        ];
        let signature = models::compute_signature(&fields, &self.config.passphrase);

        let mut qs = String::new();
        for kv in &fields {
            if kv.value.is_empty() {
                continue;
            }
            if !qs.is_empty() {
                qs.push('&');
            }
            qs.push_str(&kv.key);
            qs.push('=');
            qs.push_str(&models::query_escape(&kv.value));
        }
        qs.push_str("&signature=");
        qs.push_str(&signature);

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- see PORTING.md §5
            currency: "ZAR".to_string(),
            reference: req.reference.clone(),
            proof: ChargeProof {
                process_url: self.process_url.clone(),
                signed_fields_query: qs,
            }
            .to_bytes(),
            settled_at_unix: 0,
        })
    }

    /// **Always `Err(Unsupported)`** — see module docs: PayFast genuinely
    /// has no polling verify endpoint, mirroring cackle's own `Verify`,
    /// which returns a hard error rather than a `Result`.
    async fn verify(&self, _receipt: &Receipt) -> Result<bool> {
        Err(Error::Unsupported(
            "verify (payfast has no polling endpoint; use handle_itn instead)",
        ))
    }

    /// Verify a PayFast ITN — delegates to [`Self::handle_itn`], which
    /// performs the signature check AND PayFast's mandatory
    /// server-to-server `validate` round trip. A signature-only verdict is
    /// never returned: if the confirmation call does not come back `VALID`,
    /// this is an `Err`.
    ///
    /// PayFast signs form fields in the body (MD5 over the ordered,
    /// url-encoded set plus the optional passphrase) — there is no signature
    /// header. This rail is ZAR-only, so a settled ITN is reported in ZAR.
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event = self.handle_itn(&delivery.raw_body).await?;
        Ok(WebhookEvent::settlement(
            &self.id,
            event.event_id,
            event.reference,
            event.settled,
            event.amount_minor,
            "ZAR",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PayFastConfig {
        PayFastConfig {
            merchant_id: "10000100".to_string(),
            merchant_key: "46f0cd694581a".to_string(),
            passphrase: "test-passphrase".to_string(),
            requires_kyc: true,
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(validate_url: String) -> PayFastRail {
        let mut rail = PayFastRail::new(config()).unwrap();
        rail.validate_url = validate_url;
        rail
    }

    fn req(amount: u64, currency: &str, callback: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: callback.into(),
            reference: reference.into(),
        }
    }

    // Ported from cackle's internal/payments/payfast_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(caps.currencies, vec!["ZAR".to_string()]);
        assert_eq!(rail.id(), "payfast");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.merchant_id.clear();
        assert!(PayFastRail::new(cfg).is_err());

        let mut cfg = config();
        cfg.merchant_key.clear();
        assert!(PayFastRail::new(cfg).is_err());
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
    async fn charge_success_carries_signed_form_fields() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = rail
            .charge(&req(10000, "ZAR", "https://example.com/return", "ord_1"))
            .await
            .unwrap();
        assert_eq!(receipt.amount_minor, 0);
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert!(proof.signed_fields_query.contains("signature="));
        assert!(proof.signed_fields_query.contains("amount=100.00"));
        assert_eq!(proof.process_url, PAYFAST_PROCESS_URL);
    }

    #[tokio::test]
    async fn verify_is_always_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "payfast".into(),
            amount_minor: 0,
            currency: "ZAR".into(),
            reference: "ord_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(matches!(
            rail.verify(&receipt).await,
            Err(Error::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "payfast".into(),
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

    #[tokio::test]
    async fn handle_itn_valid_signature_and_validate_succeeds() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let validate_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("VALID"))
            .mount(&validate_server)
            .await;
        let rail = rail_for(validate_server.uri());

        let fields = vec![
            Kv {
                key: "m_payment_id".into(),
                value: "ord_1".into(),
            },
            Kv {
                key: "pf_payment_id".into(),
                value: "pf_123".into(),
            },
            Kv {
                key: "payment_status".into(),
                value: "COMPLETE".into(),
            },
            Kv {
                key: "amount_gross".into(),
                value: "100.00".into(),
            },
        ];
        let sig = models::compute_signature(&fields, &rail.config.passphrase);
        let body = format!(
            "m_payment_id=ord_1&pf_payment_id=pf_123&payment_status=COMPLETE&amount_gross=100.00&signature={sig}"
        );

        let event = rail.handle_itn(body.as_bytes()).await.unwrap();
        assert!(event.settled);
        assert_eq!(event.reference, "ord_1");
        assert_eq!(event.amount_minor, 10000);
    }

    #[tokio::test]
    async fn handle_itn_validate_not_valid_fails_closed() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let validate_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("INVALID"))
            .mount(&validate_server)
            .await;
        let rail = rail_for(validate_server.uri());

        let fields = vec![
            Kv {
                key: "m_payment_id".into(),
                value: "ord_1".into(),
            },
            Kv {
                key: "payment_status".into(),
                value: "COMPLETE".into(),
            },
            Kv {
                key: "amount_gross".into(),
                value: "100.00".into(),
            },
        ];
        let sig = models::compute_signature(&fields, &rail.config.passphrase);
        let body = format!(
            "m_payment_id=ord_1&payment_status=COMPLETE&amount_gross=100.00&signature={sig}"
        );

        let err = rail.handle_itn(body.as_bytes()).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    #[tokio::test]
    async fn handle_itn_missing_signature_fails_closed_without_calling_validate() {
        // No Mock registered on this server -- if the adapter called
        // validate anyway, wiremock would panic on an unexpected request.
        use wiremock::MockServer;
        let validate_server = MockServer::start().await;
        let rail = rail_for(validate_server.uri());

        let body = b"m_payment_id=ord_1&payment_status=COMPLETE&amount_gross=100.00";
        let err = rail.handle_itn(body).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }
}
