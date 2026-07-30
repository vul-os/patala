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
//!
//! The same applies to this rail's *addresses*:
//! [`MockRail::validate_destination`] decides a small synthetic grammar
//! (`"<network>:<kind>:<label>"`) which is not any real chain's format, and
//! exists so that every [`crate::DestinationStatus`] a UI has to render can be
//! produced offline, with no chain and no processor reachable.

use async_trait::async_trait;

use crate::capabilities::{RailCapabilities, RailClass, Settlement};
use crate::destination::DestinationVerdict;
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
    checks_destinations: bool,
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
                atomic_multi_party: false, // MockRail has no atomic multi-party operation (B3)
            },
            key,
            fee_minor: 0,
            fail: false,
            checks_destinations: true,
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

    /// Stand in for a rail that cannot check a destination offline at all — a
    /// fiat rail, whose `destination` is an opaque processor-side token that
    /// means nothing outside that processor.
    ///
    /// [`PaymentRail::validate_destination`] then answers
    /// [`crate::DestinationStatus::Unknown`] for anything non-empty, exactly as
    /// the trait's default does, so the "a human must confirm" path a real
    /// deployment depends on is exercised by the offline default rail rather
    /// than only by rails that are not in the default build.
    pub fn without_destination_checks(mut self) -> Self {
        self.checks_destinations = false;
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

    /// Every [`crate::DestinationStatus`], offline, against a synthetic address
    /// grammar — so a consumer can build and test its whole
    /// customer-supplies-an-address payout flow (`patala-core`'s
    /// [`crate::destination`] docs) before a real rail is compiled in.
    ///
    /// The grammar is `"<network>:<kind>:<label>"`, with `<kind>` one of
    /// `wallet` or `program`:
    ///
    /// | Destination (on a rail whose `id` is `mock`) | Verdict |
    /// |---|---|
    /// | `mock:wallet:alice` | [`crate::DestinationStatus::StructurallyValid`] |
    /// | `mock:program:vault` | [`crate::DestinationStatus::NotAWallet`] |
    /// | `stellar:wallet:alice` | [`crate::DestinationStatus::WrongNetwork`] |
    /// | `nonsense`, `mock:frog:x`, `""` | [`crate::DestinationStatus::Malformed`] |
    /// | anything, on [`MockRail::without_destination_checks`] | [`crate::DestinationStatus::Unknown`] |
    ///
    /// **It is not any real chain's address format**, in the same way and for
    /// the same reason this rail's `proof` is not a real signature (see the
    /// module docs): it is deterministic and total, so a caller's rendering of
    /// each verdict can be tested with no chain reachable. A real rail decodes
    /// its own alphabet, length and checksum instead —
    /// `pubkey_from_base58`/`is_on_curve` for Solana, StrKey for Stellar.
    ///
    /// `charge` deliberately does **not** enforce this grammar: `MockRail`
    /// stands in for both settlement classes and every existing consumer hands
    /// it arbitrary destination strings, so tying settlement to a made-up
    /// address format would make the mock less general without making anything
    /// safer. On a real rail the parser is the same code path in both places.
    fn validate_destination(&self, dest: &str) -> DestinationVerdict {
        let dest = dest.trim();
        if dest.is_empty() {
            return DestinationVerdict::malformed(
                &self.id,
                "an empty destination is not an address on any rail",
            );
        }
        if !self.checks_destinations {
            return DestinationVerdict::unknown(
                &self.id,
                format!(
                    "mock rail {} is configured without destination checks, standing in for a \
                     rail whose destination is an opaque processor-side token; a human who \
                     controls the receiving account must confirm this one",
                    self.id
                ),
            );
        }

        let parts: Vec<&str> = dest.split(':').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.trim().is_empty()) {
            return DestinationVerdict::malformed(
                &self.id,
                format!(
                    "{dest:?} is not a mock address: expected exactly \
                     \"<network>:<kind>:<label>\" with no empty part, e.g. \"{}:wallet:alice\"",
                    self.id
                ),
            );
        }
        let (network, kind, label) = (parts[0], parts[1], parts[2]);

        if network != self.id {
            return DestinationVerdict::wrong_network(
                &self.id,
                format!(
                    "this is a well-formed address for network {network:?}, but this rail pays on \
                     {:?}; money sent to it would land on the wrong network and would not be \
                     recoverable",
                    self.id
                ),
            );
        }

        match kind {
            "wallet" => DestinationVerdict::structurally_valid(
                &self.id,
                format!(
                    "{label:?} is a well-formed {} wallet address; every check this rail can make \
                     offline passed, which is not the same as knowing the recipient can receive \
                     on it",
                    self.id
                ),
            ),
            "program" => DestinationVerdict::not_a_wallet(
                &self.id,
                format!(
                    "{label:?} is a valid {} address but a program account, not a wallet — nobody \
                     holds a key for it, so funds sent there are typically unrecoverable",
                    self.id
                ),
            ),
            other => DestinationVerdict::malformed(
                &self.id,
                format!(
                    "{other:?} is not a mock address kind: expected \"wallet\" or \"program\", \
                     as in \"{}:wallet:alice\"",
                    self.id
                ),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::DestinationStatus;

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

    // ── validate_destination: every verdict, offline ─────────────────────────

    fn mock() -> MockRail {
        MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USDC".into()])
    }

    #[test]
    fn every_verdict_variant_is_reachable_offline() {
        let rail = mock();

        // The five states a UI has to render differently, each from a real call
        // rather than a hand-built verdict.
        assert_eq!(
            rail.validate_destination("mock:wallet:alice").status,
            DestinationStatus::StructurallyValid
        );
        assert_eq!(
            rail.validate_destination("mock:program:vault").status,
            DestinationStatus::NotAWallet
        );
        assert_eq!(
            rail.validate_destination("stellar:wallet:alice").status,
            DestinationStatus::WrongNetwork
        );
        assert_eq!(
            rail.validate_destination("definitely-not-an-address")
                .status,
            DestinationStatus::Malformed
        );
        assert_eq!(
            mock()
                .without_destination_checks()
                .validate_destination("cus_opaque_processor_token")
                .status,
            DestinationStatus::Unknown
        );
    }

    #[test]
    fn structural_defects_are_refusals_and_the_other_two_are_not() {
        let rail = mock();

        // Guards fail closed: each of these is something the rail *knows*, so
        // it is a refusal a caller must not let a human click past.
        for bad in [
            "",
            "   ",
            "definitely-not-an-address",
            "mock:wallet",             // too few parts
            "mock:wallet:alice:extra", // too many
            "mock::alice",             // empty kind
            ":wallet:alice",           // empty network
            "mock:frog:alice",         // unknown kind
            "stellar:wallet:alice",    // right shape, wrong network
            "mock:program:vault",      // valid, but nobody holds a key
        ] {
            let v = rail.validate_destination(bad);
            assert!(v.is_refusal(), "{bad:?} must be refused, got {v:?}");
        }

        // Neither of the non-refusals is a green light either — both still
        // demand a human, which is what the next test pins.
        assert!(!rail.validate_destination("mock:wallet:alice").is_refusal());
        assert!(!mock()
            .without_destination_checks()
            .validate_destination("mock:wallet:alice")
            .is_refusal());
    }

    #[test]
    fn no_destination_this_rail_accepts_is_ever_marked_safe_to_send_to() {
        // The property that matters most: even the most positive verdict this
        // rail can produce carries the exchange-deposit caveat and demands a
        // human confirmation, so a caller cannot read "structurally valid" as
        // "safe".
        let rail = mock();
        for dest in [
            "mock:wallet:alice",
            "mock:program:vault",
            "stellar:wallet:alice",
            "junk",
            "",
        ] {
            let v = rail.validate_destination(dest);
            assert!(v.human_must_confirm, "{dest:?}");
            assert!(v.requires_human_confirmation(), "{dest:?}");
            assert_eq!(
                v.exchange_deposit_caveat,
                crate::EXCHANGE_DEPOSIT_CAVEAT,
                "{dest:?}"
            );
            assert!(!v.reason.trim().is_empty(), "{dest:?}");
            assert_eq!(v.rail_id, "mock", "{dest:?}");
        }
    }

    #[test]
    fn wrong_network_is_decided_against_this_rails_own_id() {
        // The Stellar-address-into-a-Solana-payout mistake, in miniature: the
        // same string is fine on one rail and a refusal on the other, and
        // neither rail speaks for the other.
        let a = MockRail::new("mock-a", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let b = MockRail::new("mock-b", RailClass::NonCustodialFinal, vec!["USDC".into()]);

        let dest = "mock-a:wallet:alice";
        assert_eq!(
            a.validate_destination(dest).status,
            DestinationStatus::StructurallyValid
        );
        let cross = b.validate_destination(dest);
        assert_eq!(cross.status, DestinationStatus::WrongNetwork);
        assert!(
            cross.reason.contains("mock-a") && cross.reason.contains("mock-b"),
            "the reason must name both networks so a person can see the mix-up: {}",
            cross.reason
        );
    }

    #[test]
    fn validate_destination_is_pure_and_ignores_surrounding_whitespace() {
        // Pure: same input, same verdict, no clock and no I/O anywhere in it —
        // this has to hold in a browser and on an offline gate device.
        let rail = mock();
        let once = rail.validate_destination(" mock:wallet:alice ");
        let twice = rail.validate_destination("mock:wallet:alice");
        assert_eq!(once, twice);
        assert_eq!(once.status, DestinationStatus::StructurallyValid);
    }

    #[tokio::test]
    async fn charge_does_not_enforce_the_mock_address_grammar() {
        // Documented on `validate_destination`: MockRail stands in for both
        // settlement classes and consumers hand it arbitrary destination
        // strings, so the synthetic grammar governs the verdict only. A real
        // rail parses in both places.
        let rail = mock();
        let r = rail.charge(&req(100, "USDC", "order-9")).await.unwrap();
        assert!(rail.verify(&r).await.unwrap());
        assert!(rail.validate_destination("dest-anything").is_refusal());
    }
}
