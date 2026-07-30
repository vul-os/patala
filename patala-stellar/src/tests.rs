//! Offline tests for the Stellar rail. **No network.** Horizon is a fake that
//! decodes/re-hashes exactly what `charge` submitted, so the whole
//! charge → submit → fetch → verify loop is exercised deterministically and
//! round-trips through `stellar-xdr`'s own (spec-generated) decoder — real
//! evidence of wire-format self-consistency, not a claim of live-network
//! confirmation. See `README.md` and `src/lib.rs`'s module docs.
//!
//! An opt-in live smoke test against a real Horizon instance lives at the
//! bottom, gated on `PATALA_STELLAR_LIVE` and skipped by default.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use patala_core::{Error, PayRequest, PaymentRail, Receipt};

use super::keys::Keypair;
use super::rpc::{AssetHolding, HorizonRpc, StellarRpc, SubmitResult, TxRecord};
use super::{tx, Network, StellarBinding, StellarConfig, StellarError, StellarRail};

/// A scripted Horizon: `submit_transaction` decodes+re-hashes whatever
/// `charge` gave it (using the *same* pure functions `charge` used, so this
/// is a faithful in-memory "chain", not a hand-typed JSON fixture) and
/// `get_transaction` serves it back by hash. Failure modes are opt-in flags.
struct FakeRpc {
    passphrase: String,
    seq: i64,
    fail_load: bool,
    fail_submit: bool,
    fail_get: bool,
    lie_about_submit_hash: bool,
    force_unsuccessful_on_get: bool,
    tamper_envelope_on_get: Option<String>,
    stored: Mutex<Option<(String, String, u32)>>, // (hash_hex, envelope_b64, ledger)
    submits: AtomicBool,
}

impl FakeRpc {
    fn new(passphrase: &str, seq: i64) -> Arc<Self> {
        Arc::new(Self {
            passphrase: passphrase.to_string(),
            seq,
            fail_load: false,
            fail_submit: false,
            fail_get: false,
            lie_about_submit_hash: false,
            force_unsuccessful_on_get: false,
            tamper_envelope_on_get: None,
            stored: Mutex::new(None),
            submits: AtomicBool::new(false),
        })
    }

    fn failing_load(passphrase: &str) -> Arc<Self> {
        let mut r = Self::new(passphrase, 0);
        Arc::get_mut(&mut r).unwrap().fail_load = true;
        r
    }

    fn failing_submit(passphrase: &str, seq: i64) -> Arc<Self> {
        let mut r = Self::new(passphrase, seq);
        Arc::get_mut(&mut r).unwrap().fail_submit = true;
        r
    }

    fn lying_about_hash(passphrase: &str, seq: i64) -> Arc<Self> {
        let mut r = Self::new(passphrase, seq);
        Arc::get_mut(&mut r).unwrap().lie_about_submit_hash = true;
        r
    }

    /// Directly seed the "chain" state, bypassing `submit_transaction`, so a
    /// separate verify-time fake can be told what to answer for a given hash.
    fn seed(&self, hash_hex: &str, envelope_b64: &str, ledger: u32) {
        *self.stored.lock().unwrap() =
            Some((hash_hex.to_string(), envelope_b64.to_string(), ledger));
    }
}

#[async_trait::async_trait]
impl StellarRpc for FakeRpc {
    async fn load_sequence(&self, _account_strkey: &str) -> Result<i64, StellarError> {
        if self.fail_load {
            return Err(StellarError::Rpc("connection refused".into()));
        }
        Ok(self.seq)
    }

    async fn load_account_holdings(
        &self,
        _account_strkey: &str,
    ) -> Result<Option<Vec<AssetHolding>>, StellarError> {
        // `fail_load` is the existing opt-in "Horizon is unreachable" flag, and
        // it means the same thing here: could-not-check, which the trait doc
        // requires the call site to treat as fail-closed rather than as "the
        // payee is not ready". The funded case returns the native balance only,
        // which is what an account with no trustlines actually looks like.
        if self.fail_load {
            return Err(StellarError::Rpc("connection refused".into()));
        }
        Ok(Some(vec![AssetHolding {
            is_native: true,
            asset_code: String::new(),
            asset_issuer: String::new(),
            balance_stroops: 100_000_000,
            limit_stroops: i64::MAX,
            is_authorized: true,
        }]))
    }

    async fn submit_transaction(
        &self,
        envelope_xdr_b64: &str,
    ) -> Result<SubmitResult, StellarError> {
        if self.fail_submit {
            return Err(StellarError::Rpc("connection refused".into()));
        }
        self.submits.store(true, Ordering::SeqCst);
        // Independently decode + re-hash exactly like a real Horizon/stellar-
        // core would derive the canonical transaction hash from the envelope
        // it was handed — this is not just echoing back what `charge` computed.
        let env = tx::envelope_from_xdr_base64(envelope_xdr_b64)?;
        let decoded = tx::decode_single_payment(&env)?;
        let rebuilt = tx::build_transaction(
            decoded.source_pk,
            decoded.dest_pk,
            decoded.asset.clone(),
            decoded.amount,
            decoded.seq_num,
            decoded.fee,
            decoded.memo,
        );
        let net_id = tx::network_id(&self.passphrase);
        let hash = tx::tx_hash(net_id, &rebuilt)?;
        let hash_hex = if self.lie_about_submit_hash {
            hex::encode([0xEEu8; 32])
        } else {
            hex::encode(hash)
        };
        *self.stored.lock().unwrap() = Some((hex::encode(hash), envelope_xdr_b64.to_string(), 100));
        Ok(SubmitResult {
            hash: hash_hex,
            ledger: 100,
            successful: true,
        })
    }

    async fn get_transaction(&self, tx_hash_hex: &str) -> Result<Option<TxRecord>, StellarError> {
        if self.fail_get {
            return Err(StellarError::Rpc("connection refused".into()));
        }
        let stored = self.stored.lock().unwrap().clone();
        let Some((hash, envelope_b64, ledger)) = stored else {
            return Ok(None);
        };
        if hash != tx_hash_hex {
            return Ok(None);
        }
        Ok(Some(TxRecord {
            successful: !self.force_unsuccessful_on_get,
            ledger,
            envelope_xdr: self.tamper_envelope_on_get.clone().unwrap_or(envelope_b64),
        }))
    }
}

fn rail(fake: Arc<FakeRpc>, signer_seed: [u8; 32], issuer_seed: [u8; 32]) -> StellarRail {
    let issuer = Keypair::from_seed(issuer_seed).pubkey();
    let cfg = StellarConfig {
        network: Network::Testnet,
        usdc_issuer: issuer.to_strkey(),
        base_fee_stroops: 100,
    };
    StellarRail::new(cfg, fake).with_signer(Keypair::from_seed(signer_seed))
}

fn req(amount: u64, dest: &str, reference: &str) -> PayRequest {
    PayRequest {
        amount_minor: amount,
        currency: "USDC".into(),
        destination: dest.to_string(),
        reference: reference.to_string(),
    }
}

fn binding_of(r: &Receipt) -> StellarBinding {
    serde_json::from_slice(&r.proof)
        .expect("a receipt this crate issued always has a parseable proof")
}

fn with_binding(r: &Receipt, f: impl FnOnce(&mut StellarBinding)) -> Receipt {
    let mut b = binding_of(r);
    f(&mut b);
    let mut r2 = r.clone();
    r2.proof = serde_json::to_vec(&b).unwrap();
    r2
}

// ── Happy path ────────────────────────────────────────────────────────────

#[tokio::test]
async fn charge_then_verify_round_trips() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 41);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();

    let receipt = rail
        .charge(&req(10_000_000, &dest, "order-1"))
        .await
        .unwrap();
    assert_eq!(receipt.rail_id, "stellar");
    assert_eq!(receipt.amount_minor, 10_000_000);
    assert_eq!(receipt.currency, "USDC");
    assert!(
        rail.verify(&receipt).await.unwrap(),
        "a genuine receipt must verify"
    );
}

#[tokio::test]
async fn quote_reports_no_in_currency_fee_and_does_not_move_money() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake.clone(), [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let q = rail.quote(&req(500, &dest, "order-q")).await.unwrap();
    assert_eq!(q.fee_minor, 0);
    assert_eq!(q.total_minor, 500);
    assert!(
        !fake.submits.load(Ordering::SeqCst),
        "quote must never submit"
    );
}

// ── Charge-time refusals ─────────────────────────────────────────────────

#[tokio::test]
async fn charge_rejects_non_usdc_currency() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let mut r = req(1, &dest, "order-x");
    r.currency = "XLM".into();
    let err = rail.charge(&r).await;
    assert!(matches!(err, Err(Error::InvalidRequest(_))));
}

#[tokio::test]
async fn charge_rejects_bad_destination_address() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let err = rail
        .charge(&req(1, "not-a-strkey-address", "order-y"))
        .await;
    assert!(matches!(err, Err(Error::InvalidRequest(_))));
}

#[tokio::test]
async fn charge_without_a_signer_is_verify_only() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let issuer = Keypair::from_seed([2u8; 32]).pubkey();
    let cfg = StellarConfig {
        network: Network::Testnet,
        usdc_issuer: issuer.to_strkey(),
        base_fee_stroops: 100,
    };
    let rail = StellarRail::new(cfg, fake); // no with_signer
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let err = rail.charge(&req(1, &dest, "order-z")).await;
    assert!(matches!(err, Err(Error::Rail(_))));
}

#[tokio::test]
async fn charge_propagates_sequence_load_failure() {
    let fake = FakeRpc::failing_load(Network::Testnet.passphrase());
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    assert!(rail.charge(&req(1, &dest, "order-a")).await.is_err());
}

#[tokio::test]
async fn charge_propagates_submit_failure() {
    let fake = FakeRpc::failing_submit(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    assert!(rail.charge(&req(1, &dest, "order-b")).await.is_err());
}

#[tokio::test]
async fn charge_refuses_to_issue_a_receipt_when_horizon_lies_about_the_hash() {
    let fake = FakeRpc::lying_about_hash(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let err = rail.charge(&req(1, &dest, "order-c")).await;
    assert!(
        err.is_err(),
        "a horizon-reported hash that disagrees with the locally computed one must never yield a receipt"
    );
}

// ── Verify: fail-closed on tampering ─────────────────────────────────────

#[tokio::test]
async fn rejects_receipt_naming_a_different_rail() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let mut r = rail.charge(&req(1, &dest, "order-d")).await.unwrap();
    r.rail_id = "not-stellar".into();
    assert!(!rail.verify(&r).await.unwrap());
}

#[tokio::test]
async fn rejects_tampered_amount() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let mut r = rail
        .charge(&req(1_000_000, &dest, "order-e"))
        .await
        .unwrap();
    r.amount_minor = 999;
    assert!(
        !rail.verify(&r).await.unwrap(),
        "outer amount and the proof's amount_stroops must agree"
    );
}

#[tokio::test]
async fn rejects_tampered_reference() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let mut r = rail.charge(&req(1, &dest, "order-f")).await.unwrap();
    r.reference = "order-forged".into();
    assert!(!rail.verify(&r).await.unwrap());
}

#[tokio::test]
async fn rejects_wrong_configured_issuer() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let receipt = rail.charge(&req(1, &dest, "order-g")).await.unwrap();

    // A DIFFERENT rail, configured with a different issuer, must not vouch
    // for this receipt even though the tx hash and signature are genuine.
    let other_issuer = Keypair::from_seed([99u8; 32]).pubkey();
    let other_cfg = StellarConfig {
        network: Network::Testnet,
        usdc_issuer: other_issuer.to_strkey(),
        base_fee_stroops: 100,
    };
    let fake2 = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let other_rail = StellarRail::new(other_cfg, fake2);
    assert!(!other_rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_tampered_signature() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let receipt = rail.charge(&req(1, &dest, "order-h")).await.unwrap();
    let tampered = with_binding(&receipt, |b| b.signature = hex::encode([0u8; 64]));
    assert!(
        !rail.verify(&tampered).await.unwrap(),
        "a bit-flipped signature must never verify, entirely offline"
    );
}

#[tokio::test]
async fn rejects_self_inconsistent_hash() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let receipt = rail.charge(&req(1, &dest, "order-i")).await.unwrap();
    let tampered = with_binding(&receipt, |b| b.tx_hash = hex::encode([0xAAu8; 32]));
    assert!(
        !rail.verify(&tampered).await.unwrap(),
        "a claimed hash that does not match the rebuilt transaction is caught offline"
    );
}

#[tokio::test]
async fn rejects_malformed_proof() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let mut receipt = rail.charge(&req(1, &dest, "order-j")).await.unwrap();
    receipt.proof = b"not json at all".to_vec();
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_receipt_for_the_wrong_network() {
    let fake = FakeRpc::new(Network::Public.passphrase(), 1);
    let issuer_seed = [2u8; 32];
    let issuer = Keypair::from_seed(issuer_seed).pubkey();
    let cfg = StellarConfig {
        network: Network::Public,
        usdc_issuer: issuer.to_strkey(),
        base_fee_stroops: 100,
    };
    let mainnet_rail = StellarRail::new(cfg, fake).with_signer(Keypair::from_seed([1u8; 32]));
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let receipt = mainnet_rail
        .charge(&req(1, &dest, "order-k"))
        .await
        .unwrap();

    // A testnet-configured rail must never honour a mainnet receipt.
    let fake2 = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let cfg2 = StellarConfig {
        network: Network::Testnet,
        usdc_issuer: issuer.to_strkey(),
        base_fee_stroops: 100,
    };
    let testnet_rail = StellarRail::new(cfg2, fake2);
    assert!(!testnet_rail.verify(&receipt).await.unwrap());
}

// ── Verify: the online (Horizon) leg ─────────────────────────────────────

#[tokio::test]
async fn rejects_when_horizon_has_never_heard_of_the_hash() {
    // A separate, empty FakeRpc: never actually submitted anything, so
    // `get_transaction` always answers `None`.
    let empty = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let issuer_seed = [2u8; 32];
    let issuer = Keypair::from_seed(issuer_seed).pubkey();
    let cfg = StellarConfig {
        network: Network::Testnet,
        usdc_issuer: issuer.to_strkey(),
        base_fee_stroops: 100,
    };
    let verify_only_rail = StellarRail::new(cfg.clone(), empty);

    let submit_fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let charging_rail =
        StellarRail::new(cfg, submit_fake).with_signer(Keypair::from_seed([1u8; 32]));
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let receipt = charging_rail
        .charge(&req(1, &dest, "order-l"))
        .await
        .unwrap();

    assert!(
        !verify_only_rail.verify(&receipt).await.unwrap(),
        "a hash horizon has never indexed must deny, not be assumed settled"
    );
}

#[tokio::test]
async fn rejects_when_horizon_reports_unsuccessful() {
    let fake_charge = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let charging_rail = rail(fake_charge, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let receipt = charging_rail
        .charge(&req(1, &dest, "order-n"))
        .await
        .unwrap();
    let binding = binding_of(&receipt);

    let mut verify_fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    Arc::get_mut(&mut verify_fake)
        .unwrap()
        .force_unsuccessful_on_get = true;
    verify_fake.seed(
        &binding.tx_hash,
        "unused-because-unsuccessful-short-circuits",
        100,
    );

    let issuer = Keypair::from_seed([2u8; 32]).pubkey();
    let cfg = StellarConfig {
        network: Network::Testnet,
        usdc_issuer: issuer.to_strkey(),
        base_fee_stroops: 100,
    };
    let verify_rail = StellarRail::new(cfg, verify_fake);
    assert!(
        !verify_rail.verify(&receipt).await.unwrap(),
        "a landed-but-failed transaction moved nothing and must not verify"
    );
}

#[tokio::test]
async fn rejects_when_horizons_envelope_disagrees_with_the_binding() {
    let fake_charge = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let charging_rail = rail(fake_charge, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let receipt = charging_rail
        .charge(&req(1_000, &dest, "order-o"))
        .await
        .unwrap();
    let binding = binding_of(&receipt);

    // A DIFFERENT, but validly signed and encoded, transaction — paying a
    // different destination — that "horizon" will serve back for the same hash.
    let signer = Keypair::from_seed([1u8; 32]);
    let other_dest = Keypair::from_seed([123u8; 32]).pubkey();
    let issuer = Keypair::from_seed([2u8; 32]).pubkey();
    let asset = tx::usdc_asset("USDC", issuer.0).unwrap();
    let memo_bytes: [u8; 32] = hex::decode(&binding.memo_hash).unwrap().try_into().unwrap();
    let other_txn = tx::build_transaction(
        signer.pubkey().0,
        other_dest.0,
        asset,
        1_000,
        binding.seq_num,
        binding.fee,
        memo_bytes,
    );
    let net_id = tx::network_id(Network::Testnet.passphrase());
    let other_hash = tx::tx_hash(net_id, &other_txn).unwrap();
    let other_sig = signer.sign(&other_hash);
    let other_env = tx::envelope(other_txn, signer.pubkey().0, other_sig.0).unwrap();
    let other_b64 = tx::envelope_to_xdr_base64(&other_env).unwrap();

    let mut verify_fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    Arc::get_mut(&mut verify_fake)
        .unwrap()
        .tamper_envelope_on_get = Some(other_b64);
    verify_fake.seed(
        &binding.tx_hash,
        "unused-because-tampered-envelope-overrides",
        100,
    );

    let cfg = StellarConfig {
        network: Network::Testnet,
        usdc_issuer: issuer.to_strkey(),
        base_fee_stroops: 100,
    };
    let verify_rail = StellarRail::new(cfg, verify_fake);
    assert!(
        !verify_rail.verify(&receipt).await.unwrap(),
        "horizon reporting a different destination for the same hash must never verify"
    );
}

#[tokio::test]
async fn rejects_rpc_error_on_verify() {
    let fake = FakeRpc::new(Network::Testnet.passphrase(), 1);
    let rail = rail(fake, [1u8; 32], [2u8; 32]);
    let dest = Keypair::from_seed([5u8; 32]).pubkey().to_strkey();
    let receipt = rail.charge(&req(1, &dest, "order-m")).await.unwrap();

    // Same chain state, but a *different* rail instance whose RPC is down —
    // an unreachable Horizon must never be treated as "verified".
    let broken = Arc::new(super::rpc::HorizonRpc::new("http://127.0.0.1:1")); // nothing listens here
    let issuer = Keypair::from_seed([2u8; 32]).pubkey();
    let cfg = StellarConfig {
        network: Network::Testnet,
        usdc_issuer: issuer.to_strkey(),
        base_fee_stroops: 100,
    };
    let broken_rail = StellarRail::new(cfg, broken);
    let result = broken_rail.verify(&receipt).await;
    assert!(
        result.is_err(),
        "an unreachable horizon must propagate as an operational Err, never Ok(true)"
    );
}

// ── KAT: fixed inputs → deterministic tx hash + signature ────────────────
//
// Self-generated known-answer values (NOT sourced from an official Stellar
// test vector or a live network — see README.md's honesty section). Their
// purpose is regression-testing this exact encoder + signer pairing: if
// `stellar-xdr`'s wire layout, the signing-payload construction, or the
// signing key handling ever changes underneath this crate, this test catches
// it. It does NOT independently confirm the values match stellar-core's own
// output.

#[test]
fn kat_fixed_inputs_produce_a_deterministic_hash_and_signature() {
    let source = Keypair::from_seed([7u8; 32]);
    let source_pk = source.pubkey();
    let dest_pk = super::keys::PubKey([9u8; 32]);
    let issuer_pk = super::keys::PubKey([3u8; 32]);
    let asset = tx::usdc_asset("USDC", issuer_pk.0).unwrap();
    let memo = tx::memo_hash(
        "stellar",
        &source_pk.to_strkey(),
        &dest_pk.to_strkey(),
        "kat-reference-1",
    );
    let txn = tx::build_transaction(
        source_pk.0,
        dest_pk.0,
        asset,
        1_234_567_890i64,
        42,
        100,
        memo,
    );
    let net_id = tx::network_id(Network::Testnet.passphrase());
    let hash = tx::tx_hash(net_id, &txn).unwrap();
    let sig = source.sign(&hash);

    // Determinism: recomputing from the same fixed inputs gives byte-
    // identical results.
    let hash2 = tx::tx_hash(net_id, &txn).unwrap();
    assert_eq!(hash, hash2);
    let sig2 = source.sign(&hash);
    assert_eq!(sig.0, sig2.0, "ed25519 signing is deterministic (RFC 8032)");

    // Pinned regression values — see this test's module doc for what they
    // are (and are not) evidence of.
    assert_eq!(
        hex::encode(hash),
        "01ad1be9019f78eda9232c83bcb022b4c3f7afe19c65c8b3596a8451eb2dad4a",
        "tx hash regressed — the signing-payload/XDR encoding changed"
    );
    assert_eq!(
        hex::encode(sig.0),
        "c76868d12ddd51ec7e9e1164fd5e83530dca019be22835682a4cccac3ae94b4\
         7841e7939f464108eafc9f810b03ea683e159e2b84ffc1050bc9c70d3f573af01",
        "signature regressed — the signing key handling or hash changed"
    );

    // The signature must verify against the source's own public key...
    assert!(Keypair::verify(&source_pk, &hash, &sig));
    // ...and MUST NOT verify against a different key or a different hash.
    assert!(!Keypair::verify(&dest_pk, &hash, &sig));
    let mut other_hash = hash;
    other_hash[0] ^= 0xFF;
    assert!(!Keypair::verify(&source_pk, &other_hash, &sig));
}

/// Strong correctness check independent of "does this match real Stellar":
/// encode via our own builder, then decode via `stellar-xdr`'s own
/// spec-generated decoder, and confirm every field survives byte-for-byte.
/// A bug in field order, union discriminants, or variable-length encoding
/// would make this fail even though it never touches a network.
#[test]
fn envelope_round_trips_through_the_official_xdr_decoder() {
    let source = Keypair::from_seed([11u8; 32]);
    let dest_pk = super::keys::PubKey([13u8; 32]);
    let issuer_pk = super::keys::PubKey([17u8; 32]);
    let asset = tx::usdc_asset("USDC", issuer_pk.0).unwrap();
    let memo = tx::memo_hash(
        "stellar",
        &source.pubkey().to_strkey(),
        &dest_pk.to_strkey(),
        "rt-1",
    );
    let txn = tx::build_transaction(
        source.pubkey().0,
        dest_pk.0,
        asset,
        420_000_000,
        7,
        100,
        memo,
    );
    let net_id = tx::network_id(Network::Public.passphrase());
    let hash = tx::tx_hash(net_id, &txn).unwrap();
    let sig = source.sign(&hash);
    let env = tx::envelope(txn, source.pubkey().0, sig.0).unwrap();

    let b64 = tx::envelope_to_xdr_base64(&env).unwrap();
    let decoded_env = tx::envelope_from_xdr_base64(&b64).unwrap();
    let decoded = tx::decode_single_payment(&decoded_env).unwrap();

    assert_eq!(decoded.source_pk, source.pubkey().0);
    assert_eq!(decoded.dest_pk, dest_pk.0);
    assert_eq!(decoded.amount, 420_000_000);
    assert_eq!(decoded.seq_num, 7);
    assert_eq!(decoded.fee, 100);
    assert_eq!(decoded.memo, memo);
    assert!(tx::asset_is(&decoded.asset, "USDC", issuer_pk.0));

    let (hint, sig_bytes) = tx::single_signature(&decoded_env).unwrap();
    assert_eq!(hint.0, source.pubkey().0[28..32]);
    assert_eq!(sig_bytes, sig.0);
}

// ── Opt-in live test ──────────────────────────────────────────────────────

/// Live smoke test against a real Horizon instance. **Skipped unless**
/// `PATALA_STELLAR_LIVE` is set (e.g. `https://horizon-testnet.stellar.org`).
///
/// This crate has **not** been run against a live Horizon from the
/// environment that built it — see `README.md`. Running this test is exactly
/// how someone would close that gap.
///
/// ```sh
/// PATALA_STELLAR_LIVE=https://horizon-testnet.stellar.org \
///   cargo test -p patala-stellar live_horizon -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires a live Horizon instance; set PATALA_STELLAR_LIVE"]
async fn live_horizon_reachable_and_unknown_hash_denies() {
    let Ok(url) = std::env::var("PATALA_STELLAR_LIVE") else {
        eprintln!("PATALA_STELLAR_LIVE not set — skipping");
        return;
    };
    let horizon: Arc<dyn StellarRpc> = Arc::new(super::rpc::HorizonRpc::new(url));

    let impossible_hash = "0".repeat(64);
    let result = horizon.get_transaction(&impossible_hash).await;
    assert!(
        matches!(result, Ok(None)),
        "an all-zero hash cannot exist and must deny cleanly, not error into a receipt"
    );

    // A receipt claiming this impossible hash must never verify, live or not.
    let issuer = Keypair::from_seed([3u8; 32]).pubkey();
    let cfg = StellarConfig::testnet(issuer.to_strkey());
    let signer = Keypair::from_seed([1u8; 32]);
    let dest = Keypair::from_seed([2u8; 32]).pubkey();
    let reference = "live-smoke-1";
    let memo = tx::memo_hash(
        "stellar",
        &signer.pubkey().to_strkey(),
        &dest.to_strkey(),
        reference,
    );
    let binding = StellarBinding {
        chain: "stellar".into(),
        network_passphrase: Network::Testnet.passphrase().into(),
        tx_hash: impossible_hash,
        source: signer.pubkey().to_strkey(),
        destination: dest.to_strkey(),
        asset_code: "USDC".into(),
        asset_issuer: issuer.to_strkey(),
        amount_stroops: 1,
        seq_num: 1,
        fee: 100,
        memo_hash: hex::encode(memo),
        signature: hex::encode([0u8; 64]),
    };
    let receipt = Receipt {
        rail_id: "stellar".into(),
        amount_minor: 1,
        currency: "USDC".into(),
        reference: reference.to_string(),
        proof: serde_json::to_vec(&binding).unwrap(),
        settled_at_unix: 0,
    };
    let rail = StellarRail::new(cfg, horizon);
    assert!(!rail.verify(&receipt).await.unwrap());
}

// ── B7: real Stellar TESTNET round trip ─────────────────────────────────
//
// Everything above this line is offline. This is the ONE test in the whole
// `patala` workspace that actually moves money on a real network. It is
// deliberately expensive, deliberately network-dependent, and deliberately
// NOT part of any default `cargo test` run — see the gate below.
//
// What it proves, precisely: throwaway Ed25519 keypairs generated at
// runtime (never committed, never mainnet) are funded for free by Stellar's
// testnet Friendbot; two of them establish a trustline in a throwaway
// self-issued "USDC"-code `CreditAlphanum4` asset (the same wire shape
// `PATALA.md` §6 describes — testnet has no durable free official Circle
// USDC to acquire without first holding a trustline, which is exactly the
// chicken-and-egg this fixture breaks by issuing its own); the issuer seeds
// the payer; and then `StellarRail::charge` — the actual public API every
// consumer calls, not a bypassed internal helper — builds, signs and
// submits a real `PAYMENT` operation through `HorizonRpc`, and
// `StellarRail::verify` reads it back from Horizon and re-checks it,
// online, exactly as it would for any other receipt.
//
// What it does NOT prove: that Circle's real USDC issuer behaves
// identically (only the wire shape is shared, not the issuer identity);
// that mainnet works (a structurally different, real-money network); or
// that any multi-party/split flow works (single leg only, `patala-stellar`
// still builds one `Payment` per `StellarRail::charge` call).

use stellar_xdr::curr::{
    Memo as XdrMemo, MuxedAccount, Operation, OperationBody, Preconditions, SequenceNumber,
    Transaction as XdrTransaction, TransactionExt, Uint256, VecM,
};

/// Poll `Horizon` for an account's sequence number, tolerating the brief
/// propagation delay right after Friendbot funds it. Panics (loudly, with
/// the address) rather than silently proceeding with a stale/zero sequence
/// if the account never becomes visible.
async fn wait_for_sequence(rpc: &HorizonRpc, addr: &str) -> i64 {
    for attempt in 1..=10 {
        match rpc.load_sequence(addr).await {
            Ok(seq) => return seq,
            Err(e) if attempt < 10 => {
                eprintln!(
                    "  (attempt {attempt}/10) {addr} not yet visible on Horizon: {e}; retrying"
                );
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => panic!("account {addr} never became visible on Horizon: {e}"),
        }
    }
    unreachable!()
}

/// Build one `ops`-operation transaction from `signer`, sign it, and submit
/// it to `rpc`. Used only to set up the live fixture (trustlines + the
/// issuer's seed payment) — the actual code under test is
/// `StellarRail::charge`/`verify`, called separately, below.
async fn submit_ops(
    rpc: &HorizonRpc,
    net_id: [u8; 32],
    signer: &Keypair,
    ops: Vec<Operation>,
    seq: i64,
    fee: u32,
) -> SubmitResult {
    let txn = XdrTransaction {
        source_account: MuxedAccount::Ed25519(Uint256(signer.pubkey().0)),
        fee,
        seq_num: SequenceNumber(seq),
        cond: Preconditions::None,
        memo: XdrMemo::None,
        operations: VecM::try_from(ops).expect("well under the 100-op limit"),
        ext: TransactionExt::V0,
    };
    let hash = tx::tx_hash(net_id, &txn).expect("encode signature payload");
    let sig = signer.sign(&hash);
    let env = tx::envelope(txn, signer.pubkey().0, sig.0).expect("build envelope");
    let b64 = tx::envelope_to_xdr_base64(&env).expect("encode envelope");
    rpc.submit_transaction(&b64)
        .await
        .unwrap_or_else(|e| panic!("submit failed: {e}"))
}

/// The number of live-network tests this file is supposed to contain. If
/// this constant and the test below ever disagree, something deleted or
/// renamed the one test that has ever proven a `patala` rail settles
/// anything on a real network — see the coverage-count assertion in
/// `the_live_testnet_round_trip_test_exists_and_is_gated_correctly` below,
/// which runs in every default `cargo test` and does NOT require network
/// access, so its failure is never silent.
const LIVE_TESTNET_TEST_COUNT: usize = 1;

/// Real Stellar TESTNET round trip. **Opt-in AND network-gated**: skipped
/// unless `PATALA_LIVE_TESTNET=1` is set, on top of `#[ignore]` — belt and
/// braces, so this never runs by accident in a CI environment with no
/// network, and running it always requires an explicit, deliberate choice.
///
/// ```sh
/// PATALA_LIVE_TESTNET=1 cargo test -p patala-stellar live_testnet_round_trip \
///   -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "moves real (testnet) money over the real network; set PATALA_LIVE_TESTNET=1"]
async fn live_testnet_round_trip_settles_a_real_payment() {
    if std::env::var("PATALA_LIVE_TESTNET").as_deref() != Ok("1") {
        eprintln!(
            "SKIPPING live_testnet_round_trip_settles_a_real_payment: \
             PATALA_LIVE_TESTNET is not \"1\". No network round trip was attempted — \
             this is NOT evidence that anything settled. Run with:\n\
             PATALA_LIVE_TESTNET=1 cargo test -p patala-stellar live_testnet_round_trip \
             -- --ignored --nocapture"
        );
        return;
    }

    let horizon = HorizonRpc::new("https://horizon-testnet.stellar.org");
    let net_id = tx::network_id(Network::Testnet.passphrase());

    // 1. Three throwaway keypairs, generated fresh from the OS CSPRNG this
    //    run. None is ever written to disk, logged, or reused.
    let issuer = Keypair::generate();
    let source = Keypair::generate();
    let dest = Keypair::generate();
    let issuer_addr = issuer.pubkey().to_strkey();
    let source_addr = source.pubkey().to_strkey();
    let dest_addr = dest.pubkey().to_strkey();
    eprintln!("=== B7 live Stellar testnet round trip ===");
    eprintln!("issuer (throwaway, testnet-only): {issuer_addr}");
    eprintln!("source (throwaway, testnet-only): {source_addr}");
    eprintln!("dest   (throwaway, testnet-only): {dest_addr}");

    // 2. Fund all three via Friendbot (free testnet XLM; no secret ever
    //    leaves this process).
    let http = reqwest::Client::new();
    for (name, addr) in [
        ("issuer", &issuer_addr),
        ("source", &source_addr),
        ("dest", &dest_addr),
    ] {
        let url = format!("https://friendbot.stellar.org/?addr={addr}");
        let resp = http
            .get(&url)
            .send()
            .await
            .unwrap_or_else(|e| panic!("friendbot request for {name} failed: {e}"));
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "friendbot did not fund {name} ({addr}): HTTP {status}: {body}"
        );
        eprintln!("friendbot funded {name}: HTTP {status}");
    }

    // 3. The throwaway USDC-shaped asset: code "USDC", issued by `issuer`.
    //    Same CreditAlphanum4 wire shape `PATALA.md` §6 describes for real
    //    Circle USDC — just not Circle's own issuer, because testnet has no
    //    durable free official USDC to acquire without a trustline already
    //    existing, which is exactly this chicken-and-egg problem.
    let asset = tx::usdc_asset("USDC", issuer.pubkey().0).expect("valid asset code");
    let stellar_xdr::curr::Asset::CreditAlphanum4(alpha) = asset.clone() else {
        unreachable!("usdc_asset always returns CreditAlphanum4")
    };
    let change_trust_asset = stellar_xdr::curr::ChangeTrustAsset::CreditAlphanum4(alpha);

    // 4. `source` and `dest` each establish a trustline — required before
    //    either can hold a non-native asset at all (a PAYMENT of an asset
    //    into an account with no trustline fails `op_no_trust`).
    for (name, kp, addr) in [("source", &source, &source_addr), ("dest", &dest, &dest_addr)] {
        let seq = wait_for_sequence(&horizon, addr).await;
        let op = Operation {
            source_account: None,
            body: OperationBody::ChangeTrust(stellar_xdr::curr::ChangeTrustOp {
                line: change_trust_asset.clone(),
                limit: i64::MAX,
            }),
        };
        let result = submit_ops(&horizon, net_id, kp, vec![op], seq + 1, 100).await;
        assert!(
            result.successful,
            "{name} trustline transaction did not succeed: {result:?}"
        );
        eprintln!("{name} trustline established: tx {} (ledger {})", result.hash, result.ledger);
    }

    // 5. The issuer seeds `source` with enough of the asset to make one
    //    payment to `dest` below.
    let issuer_seq = wait_for_sequence(&horizon, &issuer_addr).await;
    let seed_amount: i64 = 500_000_000; // 5.0 units, ten-millionths scale
    let seed_op = Operation {
        source_account: None,
        body: OperationBody::Payment(stellar_xdr::curr::PaymentOp {
            destination: MuxedAccount::Ed25519(Uint256(source.pubkey().0)),
            asset: asset.clone(),
            amount: seed_amount,
        }),
    };
    let seed_result = submit_ops(&horizon, net_id, &issuer, vec![seed_op], issuer_seq + 1, 100).await;
    assert!(
        seed_result.successful,
        "issuer seed payment did not succeed: {seed_result:?}"
    );
    eprintln!(
        "issuer seeded source with {seed_amount} stroops of USDC: tx {} (ledger {})",
        seed_result.hash, seed_result.ledger
    );

    // 6. THE ACTUAL TEST: `StellarRail::charge`, the real public entry
    //    point every consumer calls — not a bypassed internal helper — pays
    //    `dest` from `source` over the live network.
    let cfg = StellarConfig::testnet(issuer_addr.clone());
    let rpc_dyn: std::sync::Arc<dyn StellarRpc> = std::sync::Arc::new(horizon);
    let rail = StellarRail::new(cfg, std::sync::Arc::clone(&rpc_dyn)).with_signer(source);

    let pay_amount: i64 = 10_000_000; // 1.0 unit
    let request = patala_core::PayRequest {
        amount_minor: pay_amount as u64,
        currency: "USDC".to_string(),
        destination: dest_addr.clone(),
        reference: "patala-stellar-b7-live-round-trip".to_string(),
    };
    let receipt = rail
        .charge(&request)
        .await
        .unwrap_or_else(|e| panic!("StellarRail::charge failed against live testnet: {e}"));

    let binding: StellarBinding =
        serde_json::from_slice(&receipt.proof).expect("receipt proof must decode");
    eprintln!("--- SETTLED ---");
    eprintln!("tx_hash: {}", binding.tx_hash);

    // 7. Read the same transaction back from Horizon directly (independent
    //    of `verify`, purely to print the raw response into this report)
    //    and print its ledger sequence + envelope.
    let record = rpc_dyn
        .get_transaction(&binding.tx_hash)
        .await
        .unwrap_or_else(|e| panic!("re-fetching the settled tx from Horizon failed: {e}"))
        .unwrap_or_else(|| {
            panic!(
                "Horizon has no record of tx {} moments after it reported success",
                binding.tx_hash
            )
        });
    eprintln!("ledger: {}", record.ledger);
    eprintln!("horizon successful: {}", record.successful);
    eprintln!("horizon envelope_xdr: {}", record.envelope_xdr);
    assert!(record.successful, "Horizon's own re-fetch reports failure");

    // 8. `StellarRail::verify` — the actual entitlement check every
    //    consumer gates on — must independently confirm this receipt,
    //    online, against Horizon.
    let verified = rail
        .verify(&receipt)
        .await
        .unwrap_or_else(|e| panic!("verify errored instead of answering: {e}"));
    assert!(
        verified,
        "StellarRail::verify rejected a receipt for a payment Horizon itself reports as \
         successful and settled"
    );

    eprintln!(
        "=== B7 PASSED: one real payment settled on Stellar testnet and StellarRail::verify \
         confirmed it ==="
    );
}

/// Always-on (no network, no `#[ignore]`) guard against the live testnet
/// test silently vanishing or being weakened. `#[ignore]`d tests are
/// invisible in a default `cargo test` summary — this is the "skip loudly"
/// half of that guard: it runs every time and fails if the live test's name,
/// its `#[ignore]` gate, or its `PATALA_LIVE_TESTNET` gate ever disappear.
#[test]
fn the_live_testnet_round_trip_test_exists_and_is_gated_correctly() {
    let src = include_str!("tests.rs");
    // Built by concatenation, not a literal, so THIS line does not itself
    // count as an occurrence of the needle it searches for.
    let needle = format!("async fn {}(", "live_testnet_round_trip_settles_a_real_payment");
    let occurrences = src.matches(&needle).count();
    assert_eq!(
        occurrences, LIVE_TESTNET_TEST_COUNT,
        "the live Stellar testnet round-trip test must exist exactly {LIVE_TESTNET_TEST_COUNT} \
         time(s) — if this fails, someone deleted, renamed, or duplicated the one test that has \
         ever proven a patala rail settles anything on a real network (B7)"
    );
    assert!(
        src.contains("#[ignore = \"moves real (testnet) money over the real network"),
        "the live test must stay opt-in via #[ignore] so it never runs by accident in CI"
    );
    assert!(
        src.contains("PATALA_LIVE_TESTNET"),
        "the live test must stay gated on PATALA_LIVE_TESTNET so CI without network access \
         is never made flaky by it"
    );
}
