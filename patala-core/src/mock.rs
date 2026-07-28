//! [`MockRail`] — the offline default (`PATALA.md` §3, §8).
//!
//! Deterministic, no network, no external crypto dependency. This is what
//! keeps the default `patala-core` build — and every consumer's CI — able to
//! run with no chain and no processor reachable.
//!
//! `MockRail`'s "signature" is a small keyed digest built in this module using
//! only `std`. **It is not a cryptographic primitive** and is not trying to be
//! one — it exists only to be deterministic and tamper-evident enough that
//! [`PaymentRail::verify`] round-trips a genuine receipt and visibly rejects a
//! mutated one. A real rail (Solana, Stellar, Hyperswitch, ...) proves its
//! receipts with the actual signature scheme of its chain or processor;
//! nothing downstream should ever mistake this mock's digest for that.

use async_trait::async_trait;

use crate::capabilities::{RailCapabilities, RailClass, Settlement};
use crate::error::{Error, Result};
use crate::rail::{PayRequest, PaymentRail, Quote, Receipt};

const DIGEST_LEN: usize = 32;
const KEY_LEN: usize = 16;

/// FNV-1a extended to 32 bytes via four independent domain-salted rounds.
/// Deterministic and dependency-free. See the module docs for what this is
/// (and is not) a substitute for.
fn keyed_digest(key: &[u8], msg: &[u8]) -> [u8; DIGEST_LEN] {
    fn fnv1a_round(salt: u8, key: &[u8], msg: &[u8]) -> u64 {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h = OFFSET ^ (salt as u64);
        for &b in key.iter().chain(msg.iter()) {
            h ^= b as u64;
            h = h.wrapping_mul(PRIME);
        }
        h
    }
    let mut out = [0u8; DIGEST_LEN];
    for (round, chunk) in out.chunks_mut(8).enumerate() {
        chunk.copy_from_slice(&fnv1a_round(round as u8, key, msg).to_le_bytes());
    }
    out
}

/// Canonical, field-by-field bytes covered by a mock receipt's `proof`. Built
/// from the fields directly (never from JSON) so the digest does not depend
/// on serde's output shape.
fn signing_bytes(rail_id: &str, r: &Receipt) -> Vec<u8> {
    let mut b = Vec::with_capacity(rail_id.len() + r.currency.len() + r.reference.len() + 24);
    b.extend_from_slice(rail_id.as_bytes());
    b.extend_from_slice(&r.amount_minor.to_le_bytes());
    b.extend_from_slice(r.currency.as_bytes());
    b.extend_from_slice(r.reference.as_bytes());
    b.extend_from_slice(&r.settled_at_unix.to_le_bytes());
    b
}

/// The offline default rail. Deterministic, dependency-free, and — unlike a
/// real rail — configurable to any [`RailClass`], so it can stand in for
/// either side of the settlement boundary in tests without a second rail
/// implementation existing yet.
pub struct MockRail {
    id: String,
    capabilities: RailCapabilities,
    key: [u8; KEY_LEN],
    fee_minor: u64,
    fail: bool,
}

impl MockRail {
    /// A rail named `id`, of the given `class`, accepting `currencies`. No
    /// fee, never fails, and — like every rail beyond it — deterministic:
    /// two `MockRail`s built with the same `id` sign with the same key.
    pub fn new(id: impl Into<String>, class: RailClass, currencies: Vec<String>) -> Self {
        let id = id.into();
        let digest = keyed_digest(b"patala-core/mock-rail-key/v1", id.as_bytes());
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&digest[..KEY_LEN]);

        // A mock's class-driven defaults track what real rails of each class
        // actually look like (`PATALA.md` §3), so a test built on `MockRail`
        // exercises the same shape a real rail will.
        let reversible = matches!(class, RailClass::CustodialReversible);
        Self {
            id,
            capabilities: RailCapabilities {
                class,
                reversible,
                requires_kyc: reversible,
                holds_funds: reversible,
                currencies,
                settlement: if reversible {
                    Settlement::Days(2)
                } else {
                    Settlement::Instant
                },
            },
            key,
            fee_minor: 0,
            fail: false,
        }
    }

    /// A flat fee (minor units) added to `amount_minor` by `quote`/`charge`.
    pub fn with_fee_minor(mut self, fee_minor: u64) -> Self {
        self.fee_minor = fee_minor;
        self
    }

    /// Make every `quote`/`charge` on this instance fail. Exists so a test can
    /// exercise [`crate::FailoverRail`] falling through without needing a
    /// second `PaymentRail` implementation to already exist.
    pub fn failing(mut self) -> Self {
        self.fail = true;
        self
    }

    fn currency_supported(&self, currency: &str) -> bool {
        self.capabilities.currencies.iter().any(|c| c == currency)
    }
}

#[async_trait]
impl PaymentRail for MockRail {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        if !self.currency_supported(&req.currency) {
            return Err(Error::InvalidRequest(format!(
                "rail {} does not support currency {}",
                self.id, req.currency
            )));
        }
        Ok(Quote {
            rail_id: self.id.clone(),
            amount_minor: req.amount_minor,
            currency: req.currency.clone(),
            fee_minor: self.fee_minor,
            total_minor: req.amount_minor.saturating_add(self.fee_minor),
            settlement: self.capabilities.settlement,
            expires_at_unix: crate::now_unix().saturating_add(300),
        })
    }

    async fn charge(&self, req: &PayRequest) -> Result<Receipt> {
        req.validate()?;
        if self.fail {
            return Err(Error::Rail(format!(
                "mock rail {} is configured to fail",
                self.id
            )));
        }
        if !self.currency_supported(&req.currency) {
            return Err(Error::InvalidRequest(format!(
                "rail {} does not support currency {}",
                self.id, req.currency
            )));
        }

        let mut r = Receipt {
            rail_id: self.id.clone(),
            amount_minor: req.amount_minor,
            currency: req.currency.clone(),
            reference: req.reference.clone(),
            proof: Vec::new(),
            settled_at_unix: crate::now_unix(),
        };
        r.proof = keyed_digest(&self.key, &signing_bytes(&self.id, &r)).to_vec();
        Ok(r)
    }

    async fn verify(&self, receipt: &Receipt) -> Result<bool> {
        // Fail closed: a receipt naming a different rail, or carrying a
        // malformed proof, is never assumed valid.
        if receipt.rail_id != self.id {
            return Ok(false);
        }
        if receipt.proof.len() != DIGEST_LEN {
            return Ok(false);
        }
        let expected = keyed_digest(&self.key, &signing_bytes(&self.id, receipt));
        Ok(expected.as_slice() == receipt.proof.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(amount: u64, currency: &str, reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: amount,
            currency: currency.into(),
            destination: "dest-anything".into(),
            reference: reference.into(),
        }
    }

    #[tokio::test]
    async fn charge_then_verify_round_trips() {
        let rail = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let r = rail.charge(&req(500, "USDC", "order-1")).await.unwrap();

        assert_eq!(r.rail_id, "mock");
        assert_eq!(r.amount_minor, 500);
        assert_eq!(r.currency, "USDC");
        assert!(
            rail.verify(&r).await.unwrap(),
            "a genuine receipt must verify"
        );
    }

    #[tokio::test]
    async fn tampered_receipt_fails_verify_closed() {
        let rail = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let mut r = rail.charge(&req(500, "USDC", "order-2")).await.unwrap();
        assert!(rail.verify(&r).await.unwrap());

        // Inflate the amount after the fact — the signature no longer covers
        // what the receipt now claims.
        r.amount_minor = 999_999;
        assert!(
            !rail.verify(&r).await.unwrap(),
            "a tampered receipt must never verify"
        );
    }

    #[tokio::test]
    async fn receipt_naming_a_different_rail_fails_verify() {
        let a = MockRail::new("mock-a", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let b = MockRail::new("mock-b", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let r = a.charge(&req(100, "USDC", "order-3")).await.unwrap();

        assert!(a.verify(&r).await.unwrap());
        assert!(
            !b.verify(&r).await.unwrap(),
            "another rail must never vouch for a receipt it did not issue"
        );
    }

    #[tokio::test]
    async fn charges_are_deterministic() {
        let a = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let b = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let ra = a.charge(&req(250, "USDC", "order-4")).await.unwrap();
        let rb = b.charge(&req(250, "USDC", "order-4")).await.unwrap();
        assert_eq!(ra.proof, rb.proof, "same id + same request => same proof");
    }

    #[tokio::test]
    async fn refund_is_unsupported_by_default() {
        let rail = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let r = rail.charge(&req(100, "USDC", "order-5")).await.unwrap();
        let err = rail
            .refund(&r)
            .await
            .expect_err("mock never overrides refund");
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[tokio::test]
    async fn verify_webhook_is_unsupported_by_default() {
        // MockRail has no processor and therefore no push delivery. The
        // required answer is `Unsupported` — never an `Ok` event, which a
        // caller could mistake for "this delivery was authenticated".
        let rail = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let delivery = crate::WebhookDelivery::new(b"{}".to_vec(), 1_700_000_000)
            .with_header("X-Anything", "whatever");
        let err = rail
            .verify_webhook(&delivery)
            .await
            .expect_err("mock never overrides verify_webhook");
        assert!(matches!(err, Error::Unsupported("verify_webhook")));
    }

    #[tokio::test]
    async fn unsupported_currency_and_zero_amount_are_rejected() {
        let rail = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        assert!(rail.charge(&req(100, "EUR", "order-6")).await.is_err());
        assert!(rail.charge(&req(0, "USDC", "order-7")).await.is_err());
    }

    #[tokio::test]
    async fn failing_configuration_always_errors() {
        let rail =
            MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()]).failing();
        assert!(rail.charge(&req(100, "USDC", "order-8")).await.is_err());
    }
}
