//! Offline tests for the Solana rail. **No network.** The RPC is a fake, so
//! CI exercises every accept/reject path deterministically. Adapted from
//! `magnetite-seams::solana::tests` — see this crate's module docs
//! (`src/lib.rs`) for exactly what changed and why: no multi-party split (one
//! `(amount, destination)` per `PayRequest`), no rail self-signature, no
//! sync-over-async `block_on` (verify is `async` already), and RPC failures
//! propagate as `Err` rather than being folded into `Ok(false)` — matching
//! `patala_core::PaymentRail::verify`'s own documented contract.
//!
//! An opt-in live test against devnet / `solana-test-validator` lives at the
//! bottom, gated on `PATALA_SOLANA_LIVE_RPC` and skipped by default. It has
//! **not** been run from this environment — see the crate README:
//! UNVERIFIED AGAINST LIVE.

use super::*;
use serde_json::json;
use std::sync::Mutex;

const MINT: &str = tx::USDC_DEVNET_MINT;
const OTHER_MINT: &str = "So11111111111111111111111111111111111111112";

/// Scripted RPC: returns whatever the test put in it, or an error.
struct FakeRpc {
    tx: Mutex<Option<serde_json::Value>>,
    fail: bool,
    blockhash: String,
    sent: Mutex<Vec<String>>,
}

impl FakeRpc {
    fn with(txn: serde_json::Value) -> Arc<Self> {
        Arc::new(Self {
            tx: Mutex::new(Some(txn)),
            fail: false,
            blockhash: bs58::encode([9u8; 32]).into_string(),
            sent: Mutex::new(Vec::new()),
        })
    }
    fn unconfirmed() -> Arc<Self> {
        Arc::new(Self {
            tx: Mutex::new(None),
            fail: false,
            blockhash: bs58::encode([9u8; 32]).into_string(),
            sent: Mutex::new(Vec::new()),
        })
    }
    fn broken() -> Arc<Self> {
        Arc::new(Self {
            tx: Mutex::new(None),
            fail: true,
            blockhash: bs58::encode([9u8; 32]).into_string(),
            sent: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl SolanaRpc for FakeRpc {
    async fn get_transaction(
        &self,
        _signature: &str,
        _commitment: &str,
    ) -> std::result::Result<Option<serde_json::Value>, SolanaError> {
        if self.fail {
            return Err(SolanaError::Rpc("connection refused".into()));
        }
        Ok(self.tx.lock().unwrap().clone())
    }
    async fn get_latest_blockhash(&self, _c: &str) -> std::result::Result<String, SolanaError> {
        if self.fail {
            return Err(SolanaError::Rpc("connection refused".into()));
        }
        Ok(self.blockhash.clone())
    }
    async fn send_transaction(
        &self,
        wire_base64: &str,
    ) -> std::result::Result<String, SolanaError> {
        if self.fail {
            return Err(SolanaError::Rpc("connection refused".into()));
        }
        self.sent.lock().unwrap().push(wire_base64.to_string());
        Ok("5".repeat(64))
    }
}

fn cfg() -> SolanaConfig {
    SolanaConfig {
        rpc_url: "http://127.0.0.1:8899".into(),
        cluster: Cluster::Devnet,
        commitment: Commitment::Confirmed,
        usdc_mint: tx::pubkey_from_base58(MINT).unwrap(),
    }
}

fn key(seed: u8) -> Keypair {
    Keypair::from_seed([seed; 32])
}

fn req(amount: u64, currency: &str, destination: &str, reference: &str) -> PayRequest {
    PayRequest {
        amount_minor: amount,
        currency: currency.into(),
        destination: destination.into(),
        reference: reference.into(),
    }
}

/// Build a jsonParsed transaction that satisfies every check.
fn good_txn(
    payer: &PubKey,
    memo: &str,
    moves: &[(&PubKey, i128)],
    mint: &str,
) -> serde_json::Value {
    // pre = 1_000_000_000 for everyone; post = pre + delta.
    const BASE: i128 = 1_000_000_000;
    let mut pre = Vec::new();
    let mut post = Vec::new();
    for (i, (who, delta)) in moves.iter().enumerate() {
        let owner = tx::pubkey_to_base58(who);
        pre.push(json!({
            "accountIndex": i, "mint": mint, "owner": owner,
            "uiTokenAmount": { "amount": BASE.to_string(), "decimals": 6 }
        }));
        post.push(json!({
            "accountIndex": i, "mint": mint, "owner": owner,
            "uiTokenAmount": { "amount": (BASE + delta).to_string(), "decimals": 6 }
        }));
    }
    json!({
        "slot": 1234,
        "confirmationStatus": "confirmed",
        "transaction": {
            "message": {
                "accountKeys": [
                    { "pubkey": tx::pubkey_to_base58(payer), "signer": true, "writable": true }
                ],
                "instructions": [
                    { "program": "spl-memo", "programId": tx::MEMO_PROGRAM_ID, "parsed": memo }
                ]
            }
        },
        "meta": { "err": null, "preTokenBalances": pre, "postTokenBalances": post }
    })
}

fn solana_binding(payer: &PubKey, dest: &PubKey, reference: &str, mint: String) -> SolanaBinding {
    SolanaBinding {
        chain: "solana".to_string(),
        tx_signature: "5".repeat(64),
        mint,
        payer: tx::pubkey_to_base58(payer),
        destination: tx::pubkey_to_base58(dest),
        reference: hex::encode(binding_reference(payer, reference)),
    }
}

fn receipt_with(binding: &SolanaBinding, reference: &str, amount: u64) -> Receipt {
    Receipt {
        rail_id: "solana".to_string(),
        amount_minor: amount,
        currency: "USDC".to_string(),
        reference: reference.to_string(),
        proof: serde_json::to_vec(binding).unwrap(),
        settled_at_unix: now_unix(),
    }
}

/// A valid (rail, receipt) pair for `reference`, plus the payer/dest keys.
fn scenario(reference: &str, amount: u64) -> (SolanaRail, Receipt, PubKey, PubKey) {
    let payer_key = key(1);
    let payer = payer_key.pubkey();
    let dest = key(2).pubkey();

    let memo = binding_memo(&payer, reference);
    let txn = good_txn(
        &payer,
        &memo,
        &[(&payer, -(amount as i128)), (&dest, amount as i128)],
        MINT,
    );

    let rail = SolanaRail::new(cfg(), FakeRpc::with(txn)).with_signer(payer_key);
    let binding = solana_binding(&payer, &dest, reference, rail.config().mint_base58());
    let receipt = receipt_with(&binding, reference, amount);
    (rail, receipt, payer, dest)
}

// ── Verification: the happy path ─────────────────────────────────────────────

#[tokio::test]
async fn accepts_a_good_transaction() {
    let (rail, receipt, ..) = scenario("game:chess", 1_000_000);
    assert!(rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn accepts_a_good_transaction_with_a_large_amount() {
    let (rail, receipt, ..) = scenario("game:go", 987_654_321);
    assert!(rail.verify(&receipt).await.unwrap());
}

// ── Verification: every rejection ────────────────────────────────────────────

#[tokio::test]
async fn rejects_wrong_recipient() {
    let (rail, mut receipt, ..) = scenario("game:chess", 1_000_000);
    let mut binding: SolanaBinding = serde_json::from_slice(&receipt.proof).unwrap();
    binding.destination = tx::pubkey_to_base58(&key(9).pubkey()); // not who was paid on chain
    receipt.proof = serde_json::to_vec(&binding).unwrap();
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_wrong_amount() {
    let (rail, mut receipt, ..) = scenario("game:chess", 1_000_000);
    receipt.amount_minor += 1;
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_wrong_mint() {
    let payer_key = key(1);
    let payer = payer_key.pubkey();
    let dest = key(2).pubkey();
    // Chain shows the money moving in a DIFFERENT token.
    let txn = good_txn(
        &payer,
        &binding_memo(&payer, "game:chess"),
        &[(&payer, -1_000_000), (&dest, 1_000_000)],
        OTHER_MINT,
    );
    let rail = SolanaRail::new(cfg(), FakeRpc::with(txn));
    let binding = solana_binding(&payer, &dest, "game:chess", rail.config().mint_base58());
    let receipt = receipt_with(&binding, "game:chess", 1_000_000);
    assert!(
        !rail.verify(&receipt).await.unwrap(),
        "payment in another token is not a USDC payment"
    );
}

#[tokio::test]
async fn rejects_a_receipt_claiming_a_different_mint() {
    let (rail, mut receipt, ..) = scenario("game:chess", 1_000_000);
    let mut binding: SolanaBinding = serde_json::from_slice(&receipt.proof).unwrap();
    binding.mint = OTHER_MINT.to_string();
    receipt.proof = serde_json::to_vec(&binding).unwrap();
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_a_receipt_claiming_a_non_usdc_currency() {
    let (rail, mut receipt, ..) = scenario("game:chess", 1_000_000);
    receipt.currency = "SOL".to_string();
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_unconfirmed() {
    let (_, receipt, ..) = scenario("game:chess", 1_000_000);
    let rail = SolanaRail::new(cfg(), FakeRpc::unconfirmed());
    assert!(
        !rail.verify(&receipt).await.unwrap(),
        "a transaction the cluster has never heard of grants nothing"
    );
}

#[tokio::test]
async fn rejects_a_failed_transaction() {
    let (_, receipt, payer, dest) = scenario("game:chess", 1_000_000);
    let mut txn = good_txn(
        &payer,
        &binding_memo(&payer, "game:chess"),
        &[(&payer, -1_000_000), (&dest, 1_000_000)],
        MINT,
    );
    txn["meta"]["err"] = json!({ "InstructionError": [0, "InsufficientFunds"] });
    let rail = SolanaRail::new(cfg(), FakeRpc::with(txn));
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_when_commitment_is_not_met() {
    let (_, receipt, payer, dest) = scenario("game:chess", 1_000_000);
    let mut txn = good_txn(
        &payer,
        &binding_memo(&payer, "game:chess"),
        &[(&payer, -1_000_000), (&dest, 1_000_000)],
        MINT,
    );
    txn["confirmationStatus"] = json!("processed");
    let mut c = cfg();
    c.commitment = Commitment::Finalized;
    let rail = SolanaRail::new(c, FakeRpc::with(txn));
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_a_binding_with_a_forged_payer() {
    let (rail, mut receipt, ..) = scenario("game:chess", 1_000_000);
    let mut binding: SolanaBinding = serde_json::from_slice(&receipt.proof).unwrap();
    // Someone else's key on an otherwise real payment: the reference hash
    // was derived from the ORIGINAL payer, so this can never recompute equal.
    binding.payer = tx::pubkey_to_base58(&key(99).pubkey());
    receipt.proof = serde_json::to_vec(&binding).unwrap();
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_when_payer_did_not_sign() {
    let (_, receipt, payer, dest) = scenario("game:chess", 1_000_000);
    let mut txn = good_txn(
        &payer,
        &binding_memo(&payer, "game:chess"),
        &[(&payer, -1_000_000), (&dest, 1_000_000)],
        MINT,
    );
    txn["transaction"]["message"]["accountKeys"][0]["signer"] = json!(false);
    let rail = SolanaRail::new(cfg(), FakeRpc::with(txn));
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_a_tampered_reference() {
    let (rail, mut receipt, ..) = scenario("game:chess", 1_000_000);
    // Repoint the receipt at another reference without touching the chain.
    receipt.reference = "game:expensive".to_string();
    assert!(
        !rail.verify(&receipt).await.unwrap(),
        "the derived reference no longer matches (payer, reference)"
    );
}

#[tokio::test]
async fn rejects_when_the_chain_memo_binds_a_different_reference() {
    let (_, receipt, payer, dest) = scenario("game:chess", 1_000_000);
    // Receipt says chess; chain says checkers.
    let txn = good_txn(
        &payer,
        &binding_memo(&payer, "game:checkers"),
        &[(&payer, -1_000_000), (&dest, 1_000_000)],
        MINT,
    );
    let rail = SolanaRail::new(cfg(), FakeRpc::with(txn));
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_a_transaction_with_no_memo_at_all() {
    let (_, receipt, payer, dest) = scenario("game:chess", 1_000_000);
    let mut txn = good_txn(
        &payer,
        "unrelated note",
        &[(&payer, -1_000_000), (&dest, 1_000_000)],
        MINT,
    );
    txn["transaction"]["message"]["instructions"] = json!([]);
    let rail = SolanaRail::new(cfg(), FakeRpc::with(txn));
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rpc_error_propagates_as_err_never_verified() {
    let (_, receipt, ..) = scenario("game:chess", 1_000_000);
    let rail = SolanaRail::new(cfg(), FakeRpc::broken());
    assert!(
        rail.verify(&receipt).await.is_err(),
        "an unreachable RPC must be an operational error, never an implied Ok(true)"
    );
}

#[tokio::test]
async fn rejects_a_receipt_with_an_empty_proof() {
    let (rail, mut receipt, ..) = scenario("game:chess", 1_000_000);
    receipt.proof = Vec::new();
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_a_receipt_with_a_malformed_proof() {
    let (rail, mut receipt, ..) = scenario("game:chess", 1_000_000);
    receipt.proof = b"not json".to_vec();
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_a_receipt_naming_a_different_rail() {
    let (rail, mut receipt, ..) = scenario("game:chess", 1_000_000);
    receipt.rail_id = "somewhere-else".to_string();
    assert!(!rail.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn rejects_an_unaccounted_extra_recipient() {
    let (_, receipt, payer, dest) = scenario("game:chess", 1_000_000);
    let sneak = key(77).pubkey();
    let txn = good_txn(
        &payer,
        &binding_memo(&payer, "game:chess"),
        &[(&payer, -1_000_000), (&dest, 900_000), (&sneak, 100_000)],
        MINT,
    );
    let rail = SolanaRail::new(cfg(), FakeRpc::with(txn));
    assert!(
        !rail.verify(&receipt).await.unwrap(),
        "chain must match the claimed transfer exactly"
    );
}

// ── refund is honestly absent ────────────────────────────────────────────────

#[tokio::test]
async fn refund_is_unsupported_not_faked() {
    let (rail, receipt, ..) = scenario("game:chess", 1_000_000);
    let err = rail
        .refund(&receipt)
        .await
        .expect_err("crypto settlement is final; refund must never appear to work");
    assert!(matches!(err, Error::Unsupported(_)));
}

// ── Capabilities ──────────────────────────────────────────────────────────────

#[test]
fn capabilities_report_noncustodial_final() {
    let rail = SolanaRail::new(cfg(), FakeRpc::unconfirmed());
    let caps = rail.capabilities();
    assert_eq!(caps.class, RailClass::NonCustodialFinal);
    assert!(
        !caps.holds_funds,
        "non-custodial: the rail never holds funds"
    );
    assert!(!caps.reversible, "crypto settlement is final");
    assert!(!caps.requires_kyc);
    assert!(caps.currencies.iter().any(|c| c == "USDC"));
}

// ── Config parsing fails loudly ──────────────────────────────────────────────

#[test]
fn config_parsing_rejects_junk_and_processed_commitment() {
    assert!(Cluster::parse("mainnet-beta").is_ok());
    assert!(Cluster::parse("moonnet").is_err());
    assert!(Commitment::parse("finalized").is_ok());
    assert!(
        Commitment::parse("processed").is_err(),
        "processed can be rolled back"
    );
    assert!(Commitment::parse("").is_err());
}

// ── Quoting ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn quote_reflects_the_request_with_zero_rail_fee() {
    let rail = SolanaRail::new(cfg(), FakeRpc::unconfirmed());
    let request = req(
        2_000_000,
        "USDC",
        &tx::pubkey_to_base58(&key(2).pubkey()),
        "game:chess",
    );
    let q = rail.quote(&request).await.unwrap();
    assert_eq!(q.rail_id, "solana");
    assert_eq!(q.amount_minor, 2_000_000);
    assert_eq!(q.currency, "USDC");
    // The rail itself takes no cut; the real Solana network fee is paid in
    // SOL, out of band from the USDC amount quoted here (see lib.rs docs).
    assert_eq!(q.fee_minor, 0);
    assert_eq!(q.total_minor, 2_000_000);
}

#[tokio::test]
async fn quote_rejects_a_non_usdc_currency() {
    let rail = SolanaRail::new(cfg(), FakeRpc::unconfirmed());
    let request = req(
        1_000_000,
        "EUR",
        &tx::pubkey_to_base58(&key(2).pubkey()),
        "game:chess",
    );
    assert!(rail.quote(&request).await.is_err());
}

// ── Transaction construction + submission (charge) ───────────────────────────

#[tokio::test]
async fn charge_builds_and_sends_one_transaction() {
    let signer = key(1);
    let dest = key(2).pubkey();
    let rpc = FakeRpc::unconfirmed(); // get_latest_blockhash ok; get_transaction unused by charge
    let rail = SolanaRail::new(cfg(), rpc.clone()).with_signer(signer);

    let request = req(
        1_000_000,
        "USDC",
        &tx::pubkey_to_base58(&dest),
        "game:chess",
    );
    let receipt = rail.charge(&request).await.unwrap();

    assert_eq!(receipt.rail_id, "solana");
    assert_eq!(receipt.amount_minor, 1_000_000);
    assert_eq!(receipt.currency, "USDC");
    assert_eq!(receipt.reference, "game:chess");
    assert_eq!(
        rpc.sent.lock().unwrap().len(),
        1,
        "exactly one transaction submitted"
    );

    let binding: SolanaBinding = serde_json::from_slice(&receipt.proof).unwrap();
    assert_eq!(binding.chain, "solana");
    assert_eq!(binding.mint, rail.config().mint_base58());
    assert_eq!(binding.destination, tx::pubkey_to_base58(&dest));
}

#[tokio::test]
async fn a_freshly_charged_receipt_verifies_against_the_same_rail() {
    let signer = key(1);
    let dest = key(2).pubkey();
    let rpc = FakeRpc::unconfirmed();
    let rail = SolanaRail::new(cfg(), rpc).with_signer(signer);

    let request = req(500_000, "USDC", &tx::pubkey_to_base58(&dest), "game:go");
    let receipt = rail.charge(&request).await.unwrap();

    // `verify` needs `get_transaction` to answer, which the unconfirmed fake
    // never does (by design, `charge` never calls it) — swap in a rail
    // pointed at a fake that returns a matching landed transaction.
    let payer = key(1).pubkey();
    let txn = good_txn(
        &payer,
        &binding_memo(&payer, "game:go"),
        &[(&payer, -500_000), (&dest, 500_000)],
        MINT,
    );
    let verifier = SolanaRail::new(cfg(), FakeRpc::with(txn));
    assert!(verifier.verify(&receipt).await.unwrap());
}

#[tokio::test]
async fn charge_fails_without_a_signer() {
    let rail = SolanaRail::new(cfg(), FakeRpc::unconfirmed());
    let request = req(
        1_000_000,
        "USDC",
        &tx::pubkey_to_base58(&key(2).pubkey()),
        "game:chess",
    );
    let err = rail.charge(&request).await.unwrap_err();
    assert!(matches!(err, Error::Rail(_)));
}

#[tokio::test]
async fn charge_rejects_a_non_usdc_currency() {
    let rail = SolanaRail::new(cfg(), FakeRpc::unconfirmed()).with_signer(key(1));
    let request = req(
        1_000_000,
        "EUR",
        &tx::pubkey_to_base58(&key(2).pubkey()),
        "game:chess",
    );
    assert!(matches!(
        rail.charge(&request).await,
        Err(Error::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn charge_rejects_a_malformed_destination_address() {
    let rail = SolanaRail::new(cfg(), FakeRpc::unconfirmed()).with_signer(key(1));
    let request = req(
        1_000_000,
        "USDC",
        "not-a-valid-base58-address!!",
        "game:chess",
    );
    assert!(matches!(
        rail.charge(&request).await,
        Err(Error::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn charge_propagates_an_rpc_failure() {
    let rail = SolanaRail::new(cfg(), FakeRpc::broken()).with_signer(key(1));
    let request = req(
        1_000_000,
        "USDC",
        &tx::pubkey_to_base58(&key(2).pubkey()),
        "game:chess",
    );
    assert!(rail.charge(&request).await.is_err());
}

// ── Opt-in live test ─────────────────────────────────────────────────────────

/// Live smoke test against a real cluster. **Skipped unless**
/// `PATALA_SOLANA_LIVE_RPC` is set — see this crate's README for the
/// `solana-test-validator` recipe. Has NOT been run from this environment:
/// UNVERIFIED AGAINST LIVE (`PATALA.md` §8).
///
/// ```sh
/// solana-test-validator -r &
/// PATALA_SOLANA_LIVE_RPC=http://127.0.0.1:8899 \
///   cargo test -p patala-solana live_rpc -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires a live Solana RPC; set PATALA_SOLANA_LIVE_RPC"]
async fn live_rpc_reachable_and_unknown_signature_denies() {
    let Ok(url) = std::env::var("PATALA_SOLANA_LIVE_RPC") else {
        eprintln!("PATALA_SOLANA_LIVE_RPC not set — skipping");
        return;
    };
    let rpc: Arc<dyn SolanaRpc> = Arc::new(rpc::HttpRpc::new(url));
    let bh = rpc
        .get_latest_blockhash("confirmed")
        .await
        .expect("live cluster must answer getLatestBlockhash");
    assert_eq!(bs58::decode(&bh).into_vec().unwrap().len(), 32);

    // A signature that cannot exist must deny, not error out into an entitlement.
    let (_, mut receipt, ..) = scenario("game:chess", 1_000_000);
    let mut binding: SolanaBinding = serde_json::from_slice(&receipt.proof).unwrap();
    binding.tx_signature = bs58::encode([0u8; 64]).into_string();
    receipt.proof = serde_json::to_vec(&binding).unwrap();
    let rail = SolanaRail::new(cfg(), rpc);
    assert!(!rail.verify(&receipt).await.unwrap());
}
