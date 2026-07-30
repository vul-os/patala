//! [`PayURail`] — the `PaymentRail` implementation. Ported from cackle's
//! `internal/payments/payu.go` (`PayUProvider`).
//!
//! Built against PayU India's DOCUMENTED public API, exactly as cackle's
//! own adapter is — see this crate's `PORTING.md` "UNVERIFIED AGAINST LIVE"
//! disclosure every rail beyond `manual` in this crate carries (`PATALA.md`
//! §8). Cackle's own file header rates its confidence MEDIUM: the hash
//! sequences are corroborated across PayU India's docs and third-party
//! integration guides, but the Verify Payment API's exact field
//! completeness is less certain.
//!
//! ## `Provider` -> `PaymentRail` mapping
//!
//! - cackle's `Begin` (computes a request hash and a hosted-checkout form
//!   field set, making NO network call at all -- PayU India's checkout is
//!   an HTML form POST the BUYER's browser submits, not a server-to-server
//!   call) maps to [`PaymentRail::charge`]. **Gap vs cackle** (flagged in
//!   `PORTING.md`): PayU's Begin REQUIRES a buyer email
//!   (cackle: `"payments: payu: buyer email is required"`), and
//!   `patala_core::PayRequest` has no email field -- this port reinterprets
//!   `PayRequest::destination` (documented as "an opaque processor-side
//!   destination token") AS the buyer's email for this rail specifically,
//!   the SAME resolution `paystack::rail::PaystackRail` already uses for
//!   the identical problem. Callers of `PayURail::charge` must pass the
//!   buyer's email as `destination`.
//! - `PayRequest` also has no buyer-name field. Cackle's own fallback
//!   `if firstname == "" { firstname = "Customer" }` IS reachable here
//!   (unlike `paystack::rail`'s dead currency-fallback, see that module's
//!   docs for the contrast) since there is genuinely no name field to check
//!   at all -- this port always uses the literal `"Customer"` value cackle
//!   falls back to.
//! - cackle's `Order.CallbackURL` (`surl`/`furl`, sent only "if
//!   `o.CallbackURL != \"\"`") has no `PayRequest` equivalent either and is
//!   simply never sent -- documented info-loss; PayU tolerates their
//!   absence by falling back to its own default post-payment page, matching
//!   cackle's own conditional.
//! - cackle's hardcoded `if !strings.EqualFold(o.Currency, "INR")` check in
//!   `Begin` maps onto this crate's usual `check_currency` helper (driven by
//!   `capabilities.currencies`, same pattern `stripe::rail`/
//!   `paystack::rail` already use) -- see [`PayURail::new`]'s doc comment
//!   for why `capabilities.currencies` is hardcoded to `["INR"]` here,
//!   unconditionally, with no config/env override at all (unlike
//!   Paystack's/Square's/Xendit's configurable currency lists in this
//!   crate): cackle's own INR-only check is a real FUNCTIONAL restriction
//!   baked into `Begin`, not just an advertised capability, so there is
//!   nothing to make configurable here.
//! - cackle's `Verify(reference)` maps directly to [`PaymentRail::verify`].
//!   Like `paystack::rail`'s identical reasoning: PayU's own Verify Payment
//!   API takes the caller's own `txnid` (= `Receipt::reference`) DIRECTLY as
//!   its lookup key -- no separate provider-assigned id exists, so this
//!   `proof` (see `proof.rs`) is not load-bearing for `verify()`.
//!   **Deliberate, disclosed divergence from cackle** (`PORTING.md` §6):
//!   cackle's `Verify` returns `Err(ErrPayUTransactionNotFound)` when the
//!   requested reference is absent from `transaction_details`, and also
//!   errors on a txnid mismatch -- but `patala_core::PaymentRail::verify`
//!   must return `Ok(false)` (never `Err`) for "not (yet) settled",
//!   reserving `Err` for a genuine operational failure to even perform the
//!   check. Both of those cackle error paths are mapped to `Ok(false)`
//!   here instead of propagated as `Err`.
//! - cackle's `Webhook` maps to [`PaymentRail::verify_webhook`], which
//!   delegates to the free function
//!   [`crate::payu::webhook::verify_and_parse`]. The function keeps the
//!   pure, directly-testable shape; the trait method is what a consumer
//!   dispatching through `dyn PaymentRail` — the UniFFI binding, the
//!   sidecar — can actually reach.
//! - `refund()`: trait default (`Err(Error::Unsupported("refund"))`).
//!   Cackle's `PayUProvider.Capabilities().Refunds` is `false` with NO
//!   revealing "supports it, not implemented here"-style comment (unlike
//!   Paystack's, which explicitly hinted at real, unimplemented support) --
//!   so per `PORTING.md` §7's last bullet, this port does not fabricate a
//!   refund implementation cackle gives no evidence for.
//! - **Not ported**: cackle's `constantTimeEqualString` is ported as
//!   [`crate::httpshared::constant_time_eq`] (shared, not PayU-specific --
//!   `xendit`'s webhook token compare needs the identical primitive).

use async_trait::async_trait;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::payu::config::PayUConfig;
use crate::payu::models::{self, PayUVerifyResponse};
use crate::payu::proof::ChargeProof;

const PAYU_CHECKOUT_URL: &str = "https://secure.payu.in/_payment";
const PAYU_VERIFY_URL: &str = "https://info.payu.in/merchant/postservice?form=2";

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One `PaymentRail` talking to PayU India's hosted checkout + Verify
/// Payment API. See module docs for the full `Provider` -> `PaymentRail`
/// mapping.
pub struct PayURail {
    id: String,
    config: PayUConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
    base_url: String, // Verify Payment API URL, overridable in tests only
}

impl PayURail {
    /// Build a rail from configuration. Fails if `merchant_key` or `salt`
    /// are empty.
    ///
    /// `capabilities.currencies` is hardcoded to `["INR"]` here,
    /// unconditionally -- see module docs for why this rail (unlike
    /// Paystack/Square/Xendit in this crate) has no currency-list
    /// config/env override at all.
    pub fn new(config: PayUConfig) -> Result<Self> {
        if config.merchant_key.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "merchant_key must not be empty".into(),
            ));
        }
        if config.salt.trim().is_empty() {
            return Err(Error::InvalidRequest("salt must not be empty".into()));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building payu http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: false, // mirrors cackle's Capabilities.Refunds: false
            requires_kyc: config.requires_kyc,
            holds_funds: true, // PayU (the PROCESSOR) custodies funds in flight -- never patala. See PATALA.md §1, §8.
            currencies: vec!["INR".to_string()], // hardcoded -- see module docs
            settlement: Settlement::Days(config.settlement_days),
            atomic_multi_party: false, // always false: N payouts here are N independent API calls, never one atomic event (B3)
        };

        Ok(Self {
            id: "payu".to_string(),
            config,
            http,
            capabilities,
            base_url: PAYU_VERIFY_URL.to_string(),
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

    async fn do_verify_call(&self, form: &[(String, String)]) -> Result<(Vec<u8>, u16)> {
        let resp = self
            .http
            .post(&self.base_url)
            .form(form)
            .send()
            .await
            .map_err(|e| Error::Rail(format!("payu: verify request failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("payu: failed reading response body: {e}")))?;
        crate::httpshared::bounded_len_check(&bytes, crate::httpshared::DEFAULT_MAX_BODY_BYTES)
            .map_err(|e| Error::Rail(format!("payu: {e}")))?;
        Ok((bytes.to_vec(), status))
    }
}

#[async_trait]
impl PaymentRail for PayURail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check this rail's `destination` offline — delegated to
    /// [`crate::destination::buyer_email`], because on the `payu` rail
    /// `destination` is not a payout address: it is the **buyer's** email
    /// address, sent as PayU's buyer `email` (see this module's docs above).
    ///
    /// So the honest ceiling here is
    /// [`patala_core::DestinationStatus::Unknown`], never
    /// `StructurallyValid` — an email identifies a person, not a place money
    /// goes, and whether the mailbox exists or is the right one is not
    /// decidable offline. What *is* decided: a string that is plainly not an
    /// email address is refused, and a blockchain address or a private key
    /// pasted here is refused **by name**.
    ///
    /// Giving a customer their money back on this `CustodialReversible` rail
    /// is [`PaymentRail::refund`] — back the way it came, no destination
    /// involved — not a charge to a customer-supplied address.
    fn validate_destination(&self, dest: &str) -> patala_core::DestinationVerdict {
        crate::destination::buyer_email(self.id(), dest)
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;
        // Route the amount through the wire-boundary conversion so an
        // invalid amount/currency combination for PayU's decimal-string
        // wire format is caught early, before charge() -- mirrors this
        // crate's existing quote()-validates-early convention (see
        // stripe::rail::quote calling stripe_amount).
        crate::currency::minor_to_major_string(req.amount_minor, "INR")
            .map_err(|e| Error::InvalidRequest(format!("payu: {e}")))?;

        // NEEDS-CONFIRMATION (mirrors every other rail's identical note):
        // PayU's documented API has no pre-charge fee-quote endpoint, and
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
        // See module docs: `destination` is reinterpreted as the buyer's
        // email address, which PayU's Begin requires.
        let email = req.destination.trim();
        if email.is_empty() {
            return Err(Error::InvalidRequest(
                "payu: destination (used as the buyer email) is required".into(),
            ));
        }

        let amount = crate::currency::minor_to_major_string(req.amount_minor, "INR")
            .map_err(|e| Error::InvalidRequest(format!("payu: {e}")))?;
        let productinfo = format!("Order {}", req.reference);
        // PayRequest has no buyer-name field -- cackle's own
        // `if firstname == "" { firstname = "Customer" }` fallback is
        // reachable here (see module docs) since there is nothing to read a
        // real name from at all.
        let firstname = "Customer";

        let hash = models::request_hash(
            &self.config.merchant_key,
            &self.config.salt,
            &req.reference,
            &amount,
            &productinfo,
            firstname,
            email,
        );

        let fields = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("key", &self.config.merchant_key)
            .append_pair("txnid", &req.reference)
            .append_pair("amount", &amount)
            .append_pair("productinfo", &productinfo)
            .append_pair("firstname", firstname)
            .append_pair("email", email)
            .append_pair("hash", &hash)
            .finish();

        let proof = ChargeProof {
            fields,
            checkout_url: PAYU_CHECKOUT_URL.to_string(),
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor: 0, // nothing has settled yet -- Begin makes no network call at all
            currency: "INR".to_string(),
            reference: req.reference.clone(),
            proof: proof.to_bytes(),
            settled_at_unix: 0,
        })
    }

    async fn verify(&self, receipt: &Receipt) -> Result<bool> {
        if receipt.rail_id != self.id {
            return Ok(false);
        }
        let reference = receipt.reference.trim();
        if reference.is_empty() {
            return Ok(false);
        }

        // The Verify Payment API's own hash sequence is DIFFERENT from
        // Begin's -- mirrors cackle's `Verify`:
        // sha512(key|verify_payment|reference|salt). `models::request_hash`'s
        // general 17-field sequence does not apply here; compute this one
        // directly.
        let verify_hash = {
            use sha2::Digest;
            let joined = format!(
                "{}|verify_payment|{}|{}",
                self.config.merchant_key, reference, self.config.salt
            );
            let mut hasher = sha2::Sha512::new();
            hasher.update(joined.as_bytes());
            hex::encode(hasher.finalize())
        };

        let form = vec![
            ("key".to_string(), self.config.merchant_key.clone()),
            ("command".to_string(), "verify_payment".to_string()),
            ("var1".to_string(), reference.to_string()),
            ("hash".to_string(), verify_hash),
        ];

        let (body, status) = self.do_verify_call(&form).await?;
        if !(200..300).contains(&status) {
            return Err(models::unexpected_status(status));
        }
        let parsed: PayUVerifyResponse =
            serde_json::from_slice(&body).map_err(|e| models::malformed(&e.to_string()))?;

        // Deliberate divergence from cackle (see module docs): a missing
        // reference or a txnid mismatch fails CLOSED as Ok(false), never as
        // an Err -- cackle's ErrPayUTransactionNotFound/mismatch errors do
        // not survive this seam's fail-closed verify() contract.
        let Some(detail) = parsed.transaction_details.get(reference) else {
            return Ok(false);
        };
        if !detail.txn_id.is_empty() && detail.txn_id != reference {
            return Ok(false);
        }
        if !models::is_settled_success_ci(&detail.status) {
            return Ok(false);
        }
        if detail.mihpayid.is_empty() {
            return Ok(false);
        }
        let Ok(amount_minor) = crate::currency::major_string_to_minor(&detail.amount, "INR") else {
            return Ok(false);
        };
        if amount_minor == 0 {
            return Ok(false);
        }
        if !receipt.currency.eq_ignore_ascii_case("INR") {
            return Ok(false);
        }
        if amount_minor < receipt.amount_minor {
            return Ok(false);
        }
        Ok(true)
    }

    // refund(): trait default `Err(Error::Unsupported("refund"))` -- see
    // module docs.

    /// Verify a PayU response POST — see
    /// [`crate::payu::webhook::verify_and_parse`]. PayU signs with its
    /// reverse response hash over form fields in the body; there is no
    /// signature header.
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let event = crate::payu::webhook::verify_and_parse(
            &self.config.merchant_key,
            &self.config.salt,
            &delivery.raw_body,
        )
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
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(amount: u64, currency: &str, email: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: email.into(),
            reference: reference.into(),
        }
    }

    fn config() -> PayUConfig {
        PayUConfig {
            merchant_key: "gtKFFx".to_string(),
            salt: "eCwWELxi".to_string(),
            requires_kyc: true,
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn rail_for(base_url: String) -> PayURail {
        let mut rail = PayURail::new(config()).unwrap();
        rail.base_url = base_url;
        rail
    }

    // Ported from cackle's internal/payments/payu_test.go.

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(caps.currencies, vec!["INR".to_string()]);
        assert_eq!(rail.id(), "payu");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config();
        cfg.merchant_key.clear();
        assert!(PayURail::new(cfg).is_err());

        let mut cfg = config();
        cfg.salt.clear();
        assert!(PayURail::new(cfg).is_err());
    }

    // TestPayUBegin_RejectsNonINR
    #[tokio::test]
    async fn charge_refuses_non_inr_without_calling_server() {
        let server = MockServer::start().await;
        // No Mock registered -- charge() never calls the network at all for
        // PayU, so this also proves that structurally.
        let rail = rail_for(server.uri());
        let err = rail
            .charge(&req(1000, "USD", "a@b.com", "txn_1"))
            .await
            .expect_err("non-INR currency must be refused");
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    // TestPayUBegin_Success
    #[tokio::test]
    async fn charge_success() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = rail
            .charge(&req(10000, "INR", "a@b.com", "txn_1"))
            .await
            .unwrap();
        assert_eq!(receipt.reference, "txn_1");
        assert_eq!(
            receipt.amount_minor, 0,
            "Begin makes no network call and nothing has settled yet"
        );
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.checkout_url, PAYU_CHECKOUT_URL);
        let fields: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(proof.fields.as_bytes())
                .into_owned()
                .collect();
        assert!(!fields.get("hash").unwrap().is_empty());
        assert_eq!(fields.get("amount").unwrap(), "100.00");
        assert_eq!(fields.get("firstname").unwrap(), "Customer");
        assert_eq!(fields.get("email").unwrap(), "a@b.com");
    }

    #[tokio::test]
    async fn charge_requires_email() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let err = rail
            .charge(&req(10000, "INR", "", "txn_1"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    // TestPayUVerify_Success
    #[tokio::test]
    async fn verify_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": 1,
                "transaction_details": {
                    "txn_1": {"mihpayid": "mihpay123", "status": "success", "txnid": "txn_1", "amt": "100.00", "addedon": "2026-07-20 10:00:00"}
                }
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "payu".into(),
            amount_minor: 0,
            currency: "INR".into(),
            reference: "txn_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&receipt).await.unwrap());
    }

    // TestPayUVerify_NotFoundFailsClosed -- Ok(false), not an Err (see
    // module docs' deliberate divergence from cackle's ErrPayUTransactionNotFound).
    #[tokio::test]
    async fn verify_not_found_fails_closed_as_ok_false() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": 1,
                "transaction_details": {}
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "payu".into(),
            amount_minor: 0,
            currency: "INR".into(),
            reference: "txn_missing".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    // TestPayUVerify_MalformedJSONFailsClosed
    #[tokio::test]
    async fn verify_malformed_json_fails_closed_as_err() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "payu".into(),
            amount_minor: 0,
            currency: "INR".into(),
            reference: "txn_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    // TestPayUVerify_Provider500FailsClosed
    #[tokio::test]
    async fn verify_provider_500_fails_closed_as_err() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "payu".into(),
            amount_minor: 0,
            currency: "INR".into(),
            reference: "txn_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        let err = rail.verify(&receipt).await.unwrap_err();
        assert!(matches!(err, Error::Rail(_)));
    }

    #[tokio::test]
    async fn verify_amount_mismatch_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": 1,
                "transaction_details": {
                    "txn_1": {"mihpayid": "mihpay123", "status": "success", "txnid": "txn_1", "amt": "1.00", "addedon": ""}
                }
            })))
            .mount(&server)
            .await;

        let rail = rail_for(server.uri());
        let receipt = Receipt {
            rail_id: "payu".into(),
            amount_minor: 999_999,
            currency: "INR".into(),
            reference: "txn_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(!rail.verify(&receipt).await.unwrap());
    }

    #[tokio::test]
    async fn refund_is_unsupported() {
        let rail = rail_for("http://127.0.0.1:1".into());
        let receipt = Receipt {
            rail_id: "payu".into(),
            amount_minor: 100,
            currency: "INR".into(),
            reference: "txn_1".into(),
            proof: Vec::new(),
            settled_at_unix: 0,
        };
        assert!(matches!(
            rail.refund(&receipt).await.unwrap_err(),
            Error::Unsupported(_)
        ));
    }
}
