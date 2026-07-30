//! [`HyperswitchRail`] -- the `PaymentRail` implementation.
//!
//! A thin HTTP client to a **self-hosted** Hyperswitch instance. It does not
//! vendor a single processor SDK: Hyperswitch (Apache-2.0, Rust,
//! self-hostable, 100+ connectors -- see `PATALA.md` §2, §4) already fronts
//! Stripe/Paystack/Xendit/etc. behind one API, and this rail simply talks to
//! that API and reshapes its responses into `patala-core`'s seam. See the
//! crate `README.md` for exactly which Hyperswitch endpoints/fields this
//! relies on, and its "Sources" section for where each fact came from.
//!
//! **Non-custodial invariant.** This rail sets `holds_funds: true` on its
//! [`RailCapabilities`] -- that describes **Hyperswitch's connector's**
//! custody of funds in flight (Stripe/Paystack/etc. actually hold the money
//! momentarily), never this crate's or patala's. No function in this file
//! receives, stores, or transmits funds; it only ever moves JSON describing a
//! request to move funds. See `PATALA.md` §1, §8 and
//! `patala-core/src/capabilities.rs`'s doc on `holds_funds`.

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};

use crate::config::HyperswitchConfig;
use crate::models::{
    ErrorResponse, PaymentsCreateRequest, PaymentsResponse, RefundRequest, RefundResponse,
    RefundStatus,
};
use crate::proof::{ChargeProof, RefundProof};

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A path segment (a `payment_id` or a caller's `reference` used as one)
/// must not itself carry URL structure. Rejecting those up front is a
/// simpler, dependency-free, fail-closed alternative to pulling in a
/// percent-encoding crate for a value that should always be an opaque
/// alphanumeric-ish token in practice.
fn safe_path_segment(s: &str) -> Result<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', ' ', '\t', '\n', '\r']) || !s.is_ascii() {
        return Err(Error::InvalidRequest(format!(
            "value {s:?} is not a safe URL path segment for a hyperswitch id"
        )));
    }
    Ok(s)
}

/// One `PaymentRail` presenting a whole self-hosted Hyperswitch instance --
/// and therefore Hyperswitch's whole connector set -- as a single
/// `CustodialReversible` rail. See the module docs and crate `README.md`.
pub struct HyperswitchRail {
    id: String,
    config: HyperswitchConfig,
    http: reqwest::Client,
    capabilities: RailCapabilities,
}

impl HyperswitchRail {
    /// Build a rail from configuration. Fails (never hardcodes a fallback)
    /// if `base_url`, `api_key`, or `currencies` are empty.
    pub fn new(config: HyperswitchConfig) -> Result<Self> {
        if config.base_url.trim().is_empty() {
            return Err(Error::InvalidRequest("base_url must not be empty".into()));
        }
        if config.api_key.trim().is_empty() {
            return Err(Error::InvalidRequest("api_key must not be empty".into()));
        }
        if config.currencies.is_empty() {
            return Err(Error::InvalidRequest("currencies must not be empty".into()));
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| Error::Rail(format!("failed building hyperswitch http client: {e}")))?;

        let capabilities = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: config.requires_kyc,
            // The Hyperswitch-fronted PROCESSOR (Stripe/Paystack/...) custodies
            // funds in flight -- patala itself never does. See module docs.
            holds_funds: true,
            currencies: config.currencies.clone(),
            settlement: Settlement::Days(config.settlement_days),
        };

        Ok(Self {
            id: "hyperswitch".to_string(),
            config,
            http,
            capabilities,
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

    async fn post<B: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R> {
        let url = format!("{}{}", self.config.base_url, path);
        let resp = self
            .http
            .post(&url)
            .header("api-key", &self.config.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Rail(format!("hyperswitch request to {path} failed: {e}")))?;
        Self::parse_response(resp).await
    }

    async fn get<R: DeserializeOwned>(&self, path: &str) -> Result<R> {
        let url = format!("{}{}", self.config.base_url, path);
        let resp = self
            .http
            .get(&url)
            .header("api-key", &self.config.api_key)
            .send()
            .await
            .map_err(|e| Error::Rail(format!("hyperswitch request to {path} failed: {e}")))?;
        Self::parse_response(resp).await
    }

    async fn parse_response<R: DeserializeOwned>(resp: reqwest::Response) -> Result<R> {
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Rail(format!("failed reading hyperswitch response body: {e}")))?;

        if !status.is_success() {
            let msg = serde_json::from_slice::<ErrorResponse>(&bytes)
                .ok()
                .and_then(|e| e.message.or(e.error_type))
                .unwrap_or_else(|| String::from_utf8_lossy(&bytes).chars().take(300).collect());
            return Err(Error::Rail(format!(
                "hyperswitch returned HTTP {status}: {msg}"
            )));
        }

        serde_json::from_slice(&bytes).map_err(|e| {
            Error::Rail(format!(
                "failed decoding hyperswitch response ({} bytes): {e}",
                bytes.len()
            ))
        })
    }
}

#[async_trait]
impl PaymentRail for HyperswitchRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    /// Check a destination offline — and on this rail that honestly means
    /// answering [`patala_core::DestinationStatus::Unknown`] and saying why,
    /// rather than inventing a check.
    ///
    /// `patala-core` documents `PayRequest::destination` on a fiat rail as "an
    /// opaque processor-side destination token", and here it is exactly that:
    /// this crate passes it through as Hyperswitch's `payment_token`, a
    /// reference to a payment method the caller tokenized out of band via
    /// Hyperswitch's own client-side SDK (see `models::PaymentsCreateRequest`,
    /// where that mapping is itself flagged NEEDS-CONFIRMATION). Two
    /// consequences, both of which point the same way:
    ///
    /// * **It is not a payout address.** Nothing is sent *to* a
    ///   `payment_token`; it names the payment method money is taken *from*.
    ///   So `StructurallyValid` — "a well-formed address for the network this
    ///   rail pays on" — is not a status this rail could ever truthfully
    ///   report, and it never does.
    /// * **Its format is Hyperswitch's to define, not patala's.**
    ///   Hyperswitch's OpenAPI spec types `payment_token` as a plain string
    ///   with no documented grammar, and a self-hosted instance may be running
    ///   any version against any connector. There is therefore nothing here
    ///   that could be checked without guessing, and a guess that rejected a
    ///   token a live instance would have accepted is worse than no check —
    ///   so, deliberately, none is made. Unlike the rails in `patala-fiat`,
    ///   this one cannot even refuse a pasted wallet address: it has no
    ///   documented format to say that address fails to match.
    ///
    /// A blank destination is still [`patala_core::DestinationStatus::Malformed`]
    /// — undeliverable on every rail, and `PayRequest::validate()` rejects it
    /// too, so accepting it here would make the two disagree.
    ///
    /// Giving a customer their money back on this `CustodialReversible` rail
    /// is [`PaymentRail::refund`], which goes back the way it came and needs
    /// no destination at all.
    fn validate_destination(&self, dest: &str) -> patala_core::DestinationVerdict {
        if dest.trim().is_empty() {
            return patala_core::DestinationVerdict::malformed(
                self.id(),
                "an empty destination is not a usable Hyperswitch payment_token, and \
                 `PayRequest::validate()` refuses one on every rail",
            );
        }
        patala_core::DestinationVerdict::unknown(
            self.id(),
            format!(
                "the {} rail passes `destination` through as Hyperswitch's `payment_token` — a \
                 reference to a payment method tokenized out of band, NOT an address money is \
                 sent to. Its format is defined by the self-hosted Hyperswitch instance and its \
                 connector, not by patala, and Hyperswitch's own OpenAPI spec gives it no \
                 grammar to check against; so this rail deliberately makes no format check \
                 rather than guess one and refuse a token a live instance would have accepted. \
                 A human, or the instance itself, must confirm this token. Note also that money \
                 does not travel to this string: to give a customer their money back on this \
                 rail, call `refund`.",
                self.id()
            ),
        )
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        self.check_currency(&req.currency)?;

        // NEEDS-CONFIRMATION: Hyperswitch's published OpenAPI spec (see
        // README "Sources") has no pre-charge fee-quote endpoint -- this
        // crate never fabricates a fee it cannot obtain, so `fee_minor` is
        // honestly `0` rather than a guessed number. A deployment that needs
        // real pre-charge fee estimates must get them from its own
        // connector-specific rate sheet, out of band.
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
        safe_path_segment(&req.reference)?;

        let body = PaymentsCreateRequest {
            amount: req.amount_minor,
            currency: req.currency.clone(),
            confirm: Some(true),
            // DESIGN CHOICE (see src/models.rs doc on this field): `destination`
            // is `patala-core`'s opaque "processor-side destination token" for
            // a fiat rail; here it is passed through as Hyperswitch's
            // `payment_token`, a reference to a payment method the caller
            // already tokenized out-of-band. This crate never sees raw card
            // data.
            payment_token: Some(req.destination.clone()),
            payment_id: Some(req.reference.clone()),
            connector: self.config.connector.clone().map(|c| vec![c]),
        };

        let parsed: PaymentsResponse = self.post("/payments", &body).await?;

        let settled = parsed.status.is_settled_success();
        // Honest lifecycle mapping: a card/redirect payment that comes back
        // `requires_customer_action` (or any non-`succeeded` status) has NOT
        // moved money yet. `amount_minor` on the returned `Receipt` reports
        // what has actually moved -- `0` for anything pending, never the
        // requested amount. See this crate's `proof` module docs and
        // `patala-core/src/rail.rs`'s own `Receipt` doc: callers must gate on
        // `verify()`, never on `charge()` merely returning `Ok`.
        let amount_minor = if settled {
            parsed.amount_received.unwrap_or(parsed.amount)
        } else {
            0
        };

        let proof = ChargeProof {
            payment_id: parsed.payment_id,
            status_at_charge: parsed.status,
            redirect_to_url: parsed.next_action.and_then(|n| n.redirect_to_url),
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            amount_minor,
            currency: parsed.currency,
            reference: req.reference.clone(),
            proof: proof.to_bytes(),
            // "As-of" timestamp for this snapshot, not proof of settlement --
            // see the module docs. Only `verify()` re-derives whether this
            // payment is actually settled.
            settled_at_unix: now_unix(),
        })
    }

    async fn verify(&self, receipt: &Receipt) -> Result<bool> {
        // Fail closed on anything that doesn't even look like a receipt this
        // rail issued -- never assume valid.
        if receipt.rail_id != self.id {
            return Ok(false);
        }
        let Some(proof) = ChargeProof::from_bytes(&receipt.proof) else {
            return Ok(false);
        };
        let Ok(payment_id) = safe_path_segment(&proof.payment_id) else {
            return Ok(false);
        };

        // `force_sync=true` (a real Hyperswitch query parameter, per its
        // OpenAPI spec: "Decider to enable or disable the connector call for
        // retrieve request") asks Hyperswitch to check with the connector
        // rather than return a possibly-stale cached status -- verification
        // must reflect reality, not a cache.
        let path = format!("/payments/{payment_id}?force_sync=true");
        let parsed: PaymentsResponse = self.get(&path).await?;

        if parsed.currency != receipt.currency {
            return Ok(false);
        }
        if !parsed.status.is_settled_success() {
            return Ok(false);
        }
        let moved = parsed.amount_received.unwrap_or(0);
        if moved < receipt.amount_minor {
            return Ok(false);
        }

        // Fail closed if this payment has since been (fully) refunded --
        // `PaymentsResponse::refunds` is Hyperswitch's own "array of refund
        // objects associated with this payment" (see models.rs doc). A
        // receipt whose money has been returned to the payer no longer
        // holds.
        let refunded: u64 = parsed
            .refunds
            .unwrap_or_default()
            .iter()
            .filter(|r| r.status == RefundStatus::Succeeded)
            .map(|r| r.amount)
            .sum();
        if refunded >= receipt.amount_minor {
            return Ok(false);
        }

        Ok(true)
    }

    async fn refund(&self, receipt: &Receipt) -> Result<Receipt> {
        if receipt.rail_id != self.id {
            return Err(Error::InvalidRequest(format!(
                "receipt names rail {:?}, not {:?}",
                receipt.rail_id, self.id
            )));
        }
        let proof = ChargeProof::from_bytes(&receipt.proof).ok_or_else(|| {
            Error::InvalidRequest("receipt proof is not a hyperswitch charge proof".into())
        })?;
        safe_path_segment(&proof.payment_id)?;

        let body = RefundRequest {
            payment_id: proof.payment_id,
            amount: Some(receipt.amount_minor),
            reason: None,
            refund_id: None,
        };

        let parsed: RefundResponse = self.post("/refunds", &body).await?;

        let succeeded = parsed.status == RefundStatus::Succeeded;
        let refund_proof = RefundProof {
            refund_id: parsed.refund_id,
            payment_id: parsed.payment_id,
            status_at_refund: parsed.status,
        };

        Ok(Receipt {
            rail_id: self.id.clone(),
            // A refund itself can be async (Hyperswitch's `RefundStatus` has
            // `pending`/`review` states, not just `succeeded`/`failed`) -- so,
            // exactly as in `charge()`, only report money as having moved
            // (back to the payer) once Hyperswitch says `succeeded`.
            amount_minor: if succeeded { parsed.amount } else { 0 },
            currency: parsed.currency,
            reference: receipt.reference.clone(),
            proof: refund_proof.to_bytes(),
            settled_at_unix: now_unix(),
        })
    }

    /// Authenticate a Hyperswitch outgoing webhook — the HMAC-SHA512 check in
    /// [`crate::webhook::verify_webhook_signature`], reached through the
    /// trait so a consumer on the far side of the UniFFI binding or the
    /// sidecar can use it at all.
    ///
    /// Header: `X-Webhook-Signature-512`. Requires
    /// [`crate::HyperswitchConfig::webhook_secret`] to be configured — with
    /// no secret there is nothing to verify against, and this returns
    /// [`Error::Unsupported`] rather than accepting an unauthenticated
    /// delivery.
    ///
    /// **Always [`patala_core::WebhookStatus::Unconfirmed`], deliberately.**
    /// This crate has never seen a live Hyperswitch instance's outgoing
    /// webhook body (see the crate docs' UNVERIFIED AGAINST LIVE section), so
    /// it authenticates the delivery and parses **nothing** out of it. Doing
    /// otherwise would mean shipping a payload parser derived from a shape
    /// no test here has ever observed, and reporting settlement from it.
    /// Take the delivery as a signal to poll: load your stored [`Receipt`]
    /// and call [`Self::verify`], which is the path this crate does exercise.
    ///
    /// [`WebhookEvent::event_id`] is the signature itself. It is an
    /// HMAC-SHA512 over the exact body, so a redelivery of the same event
    /// yields the same key (which is what replay suppression needs) and two
    /// different events cannot collide on it — and unlike a field read out
    /// of the body, it does not depend on a payload shape this crate has not
    /// confirmed.
    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        let Some(secret) = self
            .config
            .webhook_secret
            .as_deref()
            .filter(|s| !s.is_empty())
        else {
            return Err(Error::Unsupported(
                "verify_webhook (no HYPERSWITCH_WEBHOOK_SECRET configured)",
            ));
        };
        let signature = delivery.header_or_empty("X-Webhook-Signature-512").trim();
        if !crate::webhook::verify_webhook_signature(
            secret.as_bytes(),
            &delivery.raw_body,
            signature,
        ) {
            return Err(Error::InvalidRequest(
                "hyperswitch: webhook signature did not verify".to_string(),
            ));
        }
        Ok(WebhookEvent::unconfirmed(&self.id, signature, ""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req(amount: u64, currency: &str, destination: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: destination.into(),
            reference: reference.into(),
        }
    }

    /// `validate_destination` here is almost entirely a refusal to guess, and
    /// these tests pin that it stays one — in both directions. Claiming a
    /// check would be dishonest; quietly dropping the blank-destination
    /// refusal, or ever reporting `StructurallyValid`, would be unsafe.
    #[test]
    fn validate_destination_is_honestly_unknown_and_never_claims_a_check() {
        use patala_core::DestinationStatus;

        let rail = HyperswitchRail::new(config_for("http://127.0.0.1:1".into())).unwrap();

        // A payment_token is opaque, so anything non-empty is Unknown — and
        // the reason has to say WHY nothing was checked, not just that
        // nothing was.
        let v = rail.validate_destination("tok_1a2b3c4d5e6f");
        assert_eq!(v.status, DestinationStatus::Unknown);
        assert!(!v.is_refusal());
        assert!(v.human_must_confirm);
        assert_eq!(v.rail_id, rail.id());
        assert!(v.reason.contains("payment_token"), "{}", v.reason);
        assert!(v.reason.contains("NOT an address"), "{}", v.reason);
        assert!(
            v.reason.contains("no grammar to check against"),
            "{}",
            v.reason
        );
        assert!(v.reason.contains("refund"), "{}", v.reason);

        // Blank fails closed — `PayRequest::validate()` refuses one too, so
        // accepting it here would put the two checks in disagreement.
        for blank in ["", " ", "\t\n"] {
            let v = rail.validate_destination(blank);
            assert_eq!(v.status, DestinationStatus::Malformed, "{blank:?}");
            assert!(v.is_refusal(), "{blank:?}");
        }
    }

    #[test]
    fn validate_destination_never_reports_structurally_valid() {
        use patala_core::DestinationStatus;

        // A custodial fiat rail has no network of its own and no address to
        // call well-formed, so there is no input for which that status is a
        // true statement.
        let rail = HyperswitchRail::new(config_for("http://127.0.0.1:1".into())).unwrap();
        for dest in [
            "tok_1a2b3c4d5e6f",
            "6dNVeXf5rQrTVAvpjTv2oyeHiWMCGSCUuUkxYCK6bZTs",
            "GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7",
            "https://example.com/return",
            "",
        ] {
            let v = rail.validate_destination(dest);
            assert_ne!(v.status, DestinationStatus::StructurallyValid, "{dest:?}");
            assert!(v.human_must_confirm, "{dest:?}");
            assert_eq!(
                v.exchange_deposit_caveat,
                patala_core::EXCHANGE_DEPOSIT_CAVEAT,
                "{dest:?}"
            );
            assert!(!v.reason.trim().is_empty(), "{dest:?}");
        }
    }

    #[test]
    fn validate_destination_does_not_pretend_to_recognize_a_wallet_address() {
        use patala_core::DestinationStatus;

        // Deliberate difference from every rail in patala-fiat, and the
        // honest one. Those rails refuse a pasted wallet address because their
        // `destination` has a documented format (a URL, an email) that the
        // address demonstrably is not. Hyperswitch's `payment_token` has NO
        // documented grammar, so this rail has no ground to stand on to call
        // any string wrong — and inventing one could refuse a token a live
        // instance would have accepted.
        let rail = HyperswitchRail::new(config_for("http://127.0.0.1:1".into())).unwrap();
        let v = rail.validate_destination("6dNVeXf5rQrTVAvpjTv2oyeHiWMCGSCUuUkxYCK6bZTs");
        assert_eq!(
            v.status,
            DestinationStatus::Unknown,
            "this rail must not claim a format check it cannot justify"
        );
        assert!(!v.is_refusal());
    }

    fn config_for(base_url: String) -> HyperswitchConfig {
        HyperswitchConfig {
            base_url,
            api_key: "snd_test_key".to_string(),
            connector: None,
            webhook_secret: None,
            requires_kyc: true,
            currencies: vec!["USD".to_string(), "NGN".to_string()],
            settlement_days: 2,
            timeout_secs: 5,
        }
    }

    fn sign_512(secret: &[u8], body: &[u8]) -> String {
        use hmac::Mac;
        let mut mac = hmac::Hmac::<sha2::Sha512>::new_from_slice(secret).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[tokio::test]
    async fn verify_webhook_authenticates_through_the_trait() {
        let mut cfg = config_for("http://127.0.0.1:1".into());
        cfg.webhook_secret = Some("payment_response_hash_key_example".to_string());
        let rail = HyperswitchRail::new(cfg).unwrap();

        let body = br#"{"event_type":"payment_succeeded","payment_id":"pay_abc"}"#.to_vec();
        let sig = sign_512(b"payment_response_hash_key_example", &body);

        let delivery = patala_core::WebhookDelivery::new(body.clone(), 1_700_000_000)
            .with_header("X-Webhook-Signature-512", &sig);
        let event = rail.verify_webhook(&delivery).await.unwrap();

        assert_eq!(event.rail_id, "hyperswitch");
        assert_eq!(
            event.status,
            patala_core::WebhookStatus::Unconfirmed,
            "this crate has never observed a live outgoing-webhook body, so it \
             authenticates the delivery and claims nothing about settlement"
        );
        assert!(!event.is_settled());
        assert_eq!(event.amount_minor, 0);
        assert!(!event.event_id.is_empty(), "dedup needs a stable key");

        // Redelivery of the identical event yields the identical dedup key.
        let again = patala_core::WebhookDelivery::new(body.clone(), 1_700_009_999)
            .with_header("X-Webhook-Signature-512", &sig);
        assert_eq!(
            rail.verify_webhook(&again).await.unwrap().event_id,
            event.event_id
        );

        // A tampered body no longer matches the signature.
        let tampered = patala_core::WebhookDelivery::new(
            br#"{"event_type":"payment_succeeded","payment_id":"pay_XXX"}"#.to_vec(),
            1_700_000_000,
        )
        .with_header("X-Webhook-Signature-512", &sig);
        assert!(matches!(
            rail.verify_webhook(&tampered).await,
            Err(Error::InvalidRequest(_))
        ));

        // A missing header is a rejection, never a skipped check.
        let unsigned = patala_core::WebhookDelivery::new(body, 1_700_000_000);
        assert!(matches!(
            rail.verify_webhook(&unsigned).await,
            Err(Error::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn verify_webhook_without_a_configured_secret_is_unsupported_not_accepted() {
        // `webhook_secret` is optional config. With nothing to verify
        // against, the only honest answers are "unsupported" -- never
        // "verified".
        let rail = HyperswitchRail::new(config_for("http://127.0.0.1:1".into())).unwrap();
        let delivery = patala_core::WebhookDelivery::new(b"{}".to_vec(), 1_700_000_000)
            .with_header("X-Webhook-Signature-512", "aabb");
        assert!(matches!(
            rail.verify_webhook(&delivery).await,
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn capabilities_are_honest_about_processor_custody() {
        let rail = HyperswitchRail::new(config_for("http://127.0.0.1:1".into())).unwrap();
        let caps = rail.capabilities();
        assert_eq!(caps.class, RailClass::CustodialReversible);
        assert!(caps.reversible);
        assert!(caps.holds_funds, "the PROCESSOR custodies -- not patala");
        assert_eq!(caps.settlement, Settlement::Days(2));
        assert_eq!(rail.id(), "hyperswitch");
    }

    #[test]
    fn new_rejects_empty_config() {
        let mut cfg = config_for("http://127.0.0.1:1".into());
        cfg.base_url = String::new();
        assert!(HyperswitchRail::new(cfg).is_err());

        let mut cfg = config_for("http://127.0.0.1:1".into());
        cfg.api_key = String::new();
        assert!(HyperswitchRail::new(cfg).is_err());

        let mut cfg = config_for("http://127.0.0.1:1".into());
        cfg.currencies.clear();
        assert!(HyperswitchRail::new(cfg).is_err());
    }

    #[tokio::test]
    async fn quote_never_fabricates_a_fee_and_rejects_unsupported_currency() {
        let rail = HyperswitchRail::new(config_for("http://127.0.0.1:1".into())).unwrap();
        let q = rail
            .quote(&req(6540, "USD", "tok_abc", "order-1"))
            .await
            .unwrap();
        assert_eq!(q.fee_minor, 0);
        assert_eq!(q.total_minor, 6540);

        assert!(rail
            .quote(&req(6540, "EUR", "tok_abc", "order-1"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn charge_posts_expected_shape_and_maps_succeeded_status() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/payments"))
            .and(header("api-key", "snd_test_key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "payment_id": "pay_abc123",
                "status": "succeeded",
                "amount": 6540,
                "amount_received": 6540,
                "currency": "USD",
                "connector": "paystack"
            })))
            .mount(&server)
            .await;

        let rail = HyperswitchRail::new(config_for(server.uri())).unwrap();
        let receipt = rail
            .charge(&req(6540, "USD", "pm_tok_1", "order-succeeded"))
            .await
            .unwrap();

        assert_eq!(receipt.rail_id, "hyperswitch");
        assert_eq!(receipt.amount_minor, 6540, "settled charge moved the money");
        assert_eq!(receipt.currency, "USD");
        assert_eq!(receipt.reference, "order-succeeded");

        // The receipt's proof is genuinely re-derivable, not fabricated: the
        // embedded payment_id must be exactly what Hyperswitch returned.
        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(proof.payment_id, "pay_abc123");
    }

    #[tokio::test]
    async fn charge_reports_pending_redirect_status_truthfully_not_as_settled() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/payments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "payment_id": "pay_pending1",
                "status": "requires_customer_action",
                "amount": 1000,
                "currency": "NGN",
                "next_action": { "redirect_to_url": "https://hs.example/redirect/abc" }
            })))
            .mount(&server)
            .await;

        let rail = HyperswitchRail::new(config_for(server.uri())).unwrap();
        let receipt = rail
            .charge(&req(1000, "NGN", "pm_tok_2", "order-pending"))
            .await
            .unwrap();

        assert_eq!(
            receipt.amount_minor, 0,
            "a requires_customer_action payment has not moved any money yet"
        );

        let proof = ChargeProof::from_bytes(&receipt.proof).unwrap();
        assert_eq!(
            proof.redirect_to_url.as_deref(),
            Some("https://hs.example/redirect/abc")
        );

        // And it must not verify as settled either, without a second call.
        Mock::given(method("GET"))
            .and(path("/payments/pay_pending1"))
            .and(query_param("force_sync", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "payment_id": "pay_pending1",
                "status": "requires_customer_action",
                "amount": 1000,
                "currency": "NGN"
            })))
            .mount(&server)
            .await;

        assert!(
            !rail.verify(&receipt).await.unwrap(),
            "still-pending payment must not verify as settled"
        );
    }

    #[tokio::test]
    async fn verify_fails_closed_on_amount_currency_or_status_mismatch() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/payments/pay_ok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "payment_id": "pay_ok",
                "status": "succeeded",
                "amount": 500,
                "amount_received": 500,
                "currency": "USD"
            })))
            .mount(&server)
            .await;

        let rail = HyperswitchRail::new(config_for(server.uri())).unwrap();

        let genuine = Receipt {
            rail_id: "hyperswitch".into(),
            amount_minor: 500,
            currency: "USD".into(),
            reference: "order-x".into(),
            proof: ChargeProof {
                payment_id: "pay_ok".into(),
                status_at_charge: crate::models::IntentStatus::Succeeded,
                redirect_to_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };
        assert!(rail.verify(&genuine).await.unwrap());

        let mut inflated = genuine.clone();
        inflated.amount_minor = 999_999;
        assert!(
            !rail.verify(&inflated).await.unwrap(),
            "receipt claiming more than hyperswitch actually received must not verify"
        );

        let mut wrong_currency = genuine.clone();
        wrong_currency.currency = "NGN".into();
        assert!(!rail.verify(&wrong_currency).await.unwrap());

        let mut wrong_rail = genuine.clone();
        wrong_rail.rail_id = "some-other-rail".into();
        assert!(!rail.verify(&wrong_rail).await.unwrap());

        let mut garbage_proof = genuine.clone();
        garbage_proof.proof = vec![1, 2, 3, 4];
        assert!(!rail.verify(&garbage_proof).await.unwrap());
    }

    #[tokio::test]
    async fn verify_fails_closed_once_fully_refunded() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/payments/pay_refunded"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "payment_id": "pay_refunded",
                "status": "succeeded",
                "amount": 500,
                "amount_received": 500,
                "currency": "USD",
                "refunds": [
                    {
                        "refund_id": "re_1",
                        "payment_id": "pay_refunded",
                        "amount": 500,
                        "currency": "USD",
                        "status": "succeeded"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let rail = HyperswitchRail::new(config_for(server.uri())).unwrap();
        let receipt = Receipt {
            rail_id: "hyperswitch".into(),
            amount_minor: 500,
            currency: "USD".into(),
            reference: "order-refunded".into(),
            proof: ChargeProof {
                payment_id: "pay_refunded".into(),
                status_at_charge: crate::models::IntentStatus::Succeeded,
                redirect_to_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };

        assert!(
            !rail.verify(&receipt).await.unwrap(),
            "a fully-refunded payment's original receipt must no longer verify as holding"
        );
    }

    #[tokio::test]
    async fn refund_posts_expected_shape_and_returns_new_receipt() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/refunds"))
            .and(header("api-key", "snd_test_key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "refund_id": "re_xyz",
                "payment_id": "pay_abc123",
                "amount": 6540,
                "currency": "USD",
                "status": "succeeded"
            })))
            .mount(&server)
            .await;

        let rail = HyperswitchRail::new(config_for(server.uri())).unwrap();

        let original = Receipt {
            rail_id: "hyperswitch".into(),
            amount_minor: 6540,
            currency: "USD".into(),
            reference: "order-succeeded".into(),
            proof: ChargeProof {
                payment_id: "pay_abc123".into(),
                status_at_charge: crate::models::IntentStatus::Succeeded,
                redirect_to_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };

        let refund_receipt = rail.refund(&original).await.unwrap();
        assert_eq!(refund_receipt.amount_minor, 6540);
        assert_eq!(refund_receipt.currency, "USD");

        let proof = RefundProof::from_bytes(&refund_receipt.proof).unwrap();
        assert_eq!(proof.refund_id, "re_xyz");
        assert_eq!(proof.payment_id, "pay_abc123");
    }

    #[tokio::test]
    async fn refund_reports_a_pending_refund_as_not_yet_moved() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/refunds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "refund_id": "re_pending",
                "payment_id": "pay_abc123",
                "amount": 6540,
                "currency": "USD",
                "status": "pending"
            })))
            .mount(&server)
            .await;

        let rail = HyperswitchRail::new(config_for(server.uri())).unwrap();
        let original = Receipt {
            rail_id: "hyperswitch".into(),
            amount_minor: 6540,
            currency: "USD".into(),
            reference: "order-succeeded".into(),
            proof: ChargeProof {
                payment_id: "pay_abc123".into(),
                status_at_charge: crate::models::IntentStatus::Succeeded,
                redirect_to_url: None,
            }
            .to_bytes(),
            settled_at_unix: 0,
        };

        let refund_receipt = rail.refund(&original).await.unwrap();
        assert_eq!(
            refund_receipt.amount_minor, 0,
            "a pending refund has not moved money back yet"
        );
    }

    #[tokio::test]
    async fn a_non_2xx_response_becomes_a_rail_error_never_a_fabricated_success() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/payments"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error_type": "invalid_request",
                "message": "Missing required param: currency",
                "code": "IR_04"
            })))
            .mount(&server)
            .await;

        let rail = HyperswitchRail::new(config_for(server.uri())).unwrap();
        let err = rail
            .charge(&req(100, "USD", "tok", "order-err"))
            .await
            .expect_err("a 400 must surface as an error, never Ok");
        match err {
            Error::Rail(msg) => assert!(msg.contains("Missing required param")),
            other => panic!("expected Error::Rail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn refund_rejects_a_receipt_from_a_different_rail() {
        let rail = HyperswitchRail::new(config_for("http://127.0.0.1:1".into())).unwrap();
        let foreign = Receipt {
            rail_id: "mock".into(),
            amount_minor: 100,
            currency: "USD".into(),
            reference: "order-1".into(),
            proof: vec![],
            settled_at_unix: 0,
        };
        assert!(rail.refund(&foreign).await.is_err());
    }
}
