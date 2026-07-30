//! # patala-solana
//!
//! Rail #2 (`PATALA.md` §4): SPL-USDC on Solana, `NonCustodialFinal`. Ported
//! from `magnetite/magnetite-seams/src/solana/` (~1760 lines, 95 tests, real
//! Ed25519 signing + SPL transaction construction + JSON-RPC) and adapted to
//! [`patala_core::PaymentRail`].
//!
//! # What ported unchanged
//!
//! The actual crypto/wire-format logic is untouched, just moved:
//! * [`tx`] — base58 pubkeys, associated-token-account (PDA) derivation via
//!   SHA-256 + curve25519 on-curve check, SPL `TransferChecked` + Memo
//!   instruction encoding, legacy transaction message serialization. Byte-for-
//!   byte identical to `magnetite-seams::solana::tx`.
//! * [`rpc`] — the `SolanaRpc` seam (so verification is unit-testable
//!   offline against a fake) plus the real `HttpRpc` JSON-RPC client.
//! * [`keys`] — the Ed25519 keypair. Trimmed from magnetite's
//!   `RawKeypairAuth` (which also carried challenge/response login and scoped
//!   token minting — out of scope for a payment rail) down to sign/verify/
//!   pubkey/env-loading.
//!
//! # What changed, and why
//!
//! * **The trait.** `magnetite_seams::payment::PaymentRail` was
//!   `checkout`/`checkout_for_item` (buyer + a multi-party `PaymentSplit`) and
//!   a **synchronous** `verify_receipt`/`verify_receipt_for_item`. This crate
//!   implements [`patala_core::PaymentRail`] instead:
//!   `checkout_item` → [`SolanaRail::charge`], the item-binding
//!   `verify_receipt_for_item` → [`SolanaRail::verify`]. `patala_core`'s
//!   `PayRequest` carries one `(amount_minor, currency, destination,
//!   reference)`, not a multi-way developer/operator/protocol-fee split, so
//!   `charge` builds one memo + one `TransferChecked` leg rather than
//!   magnetite's `Plan` of several. A rail wanting to reintroduce a split can
//!   still build one on top of the same [`tx`] primitives; it just is not
//!   part of the `patala-core` seam.
//! * **Sync-over-async is gone.** Magnetite's `verify_receipt` was
//!   synchronous by *its* trait's contract, so it carried a `block_on`
//!   helper (spawn onto a runtime, block on a channel) purely to let a sync
//!   method do network I/O. `patala_core::PaymentRail::verify` is `async`
//!   already, so [`SolanaRail::verify`] just `.await`s the RPC call directly.
//!   That scaffolding was adapter plumbing, not crypto, and dropping it is
//!   not a rewrite of the ported logic — every check inside it (steps 1-9
//!   below) survives intact.
//! * **The rail-self-signature is gone.** Magnetite additionally signed every
//!   receipt with a fixed `rail` keypair as (in its own words) "a
//!   self-consistency marker, NOT the security boundary". `patala_core`'s
//!   [`patala_core::Receipt::proof`] is already the designated place for a
//!   rail's own binding data, and the actual security boundary here — as it
//!   was there — is on-chain state: the buyer's real Ed25519 transaction
//!   signature (checked via the `signer: true` flag on-chain, step 7), the
//!   memo binding (step 8), and the exact token-balance deltas (step 9). This
//!   crate keeps all of those; only the redundant non-cryptographic bookkeeping
//!   signature was dropped.
//! * **`open_channel`/`escrow` are gone.** Those were magnetite-specific
//!   (hosting fees / wager stakes) and have no equivalent in
//!   `patala_core::PaymentRail`, which only has `quote`/`charge`/`verify`/
//!   `refund`. They were never implemented for real anyway (magnetite
//!   returned `Unsupported` for both); [`SolanaRail::refund`] is this crate's
//!   one honestly-`Unsupported` method, for the same reason: crypto
//!   settlement here is final (`PATALA.md` §3, §8).
//!
//! # Money math
//!
//! USDC has [`tx::USDC_DECIMALS`] (6) decimals. Every amount in this crate is
//! an integer count of smallest units (micro-USDC) — `u64`, never a float,
//! per `patala_core::PayRequest`/`Quote`/`Receipt` and `PATALA.md` §8.
//!
//! # Keys (`PATALA.md` §6)
//!
//! Solana is Ed25519-native: [`SolanaRail`]'s configured [`keys::Keypair`] is
//! simultaneously the signing identity and the wallet the funds move from —
//! there is no identity → wallet mapping table. Load one from
//! `SOLANA_KEYPAIR_PATH` (a solana-CLI JSON byte array, `chmod 600`) or
//! `SOLANA_KEYPAIR` (base58 secret key) via [`keys::Keypair::from_env`]. It is
//! never logged, never serialized, never written anywhere by this crate.
//!
//! # Honesty (`PATALA.md` §8)
//!
//! Every offline test in the `tests` module runs with **no network** — the RPC is a
//! scripted fake. The one test that touches a live cluster is
//! `#[ignore]`d and gated on `PATALA_SOLANA_LIVE_RPC`, exactly as magnetite
//! gated its analogous test on `MAGNETITE_SOLANA_LIVE_RPC`. **This crate has
//! not been run against a live Solana RPC from this environment — treat the
//! live path as UNVERIFIED AGAINST LIVE** until someone runs that ignored test
//! against devnet or `solana-test-validator` (see its doc comment for the
//! exact command) and confirms it passes.

pub mod destination;
pub mod keys;
pub mod rpc;
pub mod tx;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use patala_core::{
    Error, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result, Settlement,
};

use keys::{Keypair, PubKey};
use rpc::SolanaRpc;

/// Everything that can go wrong on this rail. Every variant is a *refusal*:
/// none of them ever results in an entitlement being granted. Converted to
/// `patala_core::Error` at the [`PaymentRail`] boundary — see
/// `impl From<SolanaError> for Error`.
#[derive(Debug, thiserror::Error)]
pub enum SolanaError {
    /// The RPC endpoint was unreachable, slow, or answered with an error.
    #[error("solana rpc: {0}")]
    Rpc(String),
    /// A base58 address failed to decode into 32 bytes.
    #[error("not a valid solana address: {0}")]
    BadAddress(String),
    /// Program-derived-address search exhausted every bump (astronomically
    /// unlikely).
    #[error("could not derive associated token address")]
    Derivation,
    /// Misconfiguration — missing mint, bad RPC URL, unusable keypair, ...
    #[error("solana rail misconfigured: {0}")]
    Config(String),
}

impl From<SolanaError> for Error {
    fn from(e: SolanaError) -> Self {
        Error::Rail(e.to_string())
    }
}

/// Which cluster the rail is pointed at. `MainnetBeta` moves **real money**.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cluster {
    /// Real funds. Real losses.
    MainnetBeta,
    /// Free test USDC.
    Devnet,
    /// Free test USDC.
    Testnet,
    /// `solana-test-validator` on localhost.
    Localnet,
}

impl Cluster {
    /// Parse a cluster name; unknown names are a hard error (never a default).
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "mainnet-beta" | "mainnet" => Ok(Cluster::MainnetBeta),
            "devnet" => Ok(Cluster::Devnet),
            "testnet" => Ok(Cluster::Testnet),
            "localnet" | "local" => Ok(Cluster::Localnet),
            other => Err(SolanaError::Config(format!("unknown cluster {other:?}")).into()),
        }
    }
    /// Does this cluster move real money?
    pub fn is_mainnet(&self) -> bool {
        matches!(self, Cluster::MainnetBeta)
    }
}

/// Confirmation level required before a receipt may be honoured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Commitment {
    /// Supermajority-voted. Reasonable for low-value goods. ~1s typical.
    Confirmed,
    /// Rooted; cannot be rolled back. Correct for anything valuable. ~13s
    /// typical (32 slots at Solana's ~400ms slot time).
    Finalized,
}

impl Commitment {
    /// Parse a commitment level. `processed` is deliberately REJECTED: it can
    /// be rolled back, so honouring it would hand out goods for reverted
    /// payments.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "confirmed" => Ok(Commitment::Confirmed),
            "finalized" => Ok(Commitment::Finalized),
            "processed" => Err(SolanaError::Config(
                "commitment 'processed' can be rolled back and is not accepted".into(),
            )
            .into()),
            other => Err(SolanaError::Config(format!("unknown commitment {other:?}")).into()),
        }
    }
    /// The wire string for JSON-RPC.
    pub fn as_str(&self) -> &'static str {
        match self {
            Commitment::Confirmed => "confirmed",
            Commitment::Finalized => "finalized",
        }
    }
    /// Rough, honestly-approximate finality latency at this commitment —
    /// used only to fill in [`RailCapabilities::settlement`]. Real finality
    /// varies with cluster load; this is not a guarantee.
    fn settlement(&self) -> Settlement {
        match self {
            Commitment::Confirmed => Settlement::Seconds(1),
            Commitment::Finalized => Settlement::Seconds(13),
        }
    }
}

/// Static configuration for the rail. Built by the caller (a backend reads it
/// from env and fails loudly if it is wrong).
#[derive(Clone, Debug)]
pub struct SolanaConfig {
    /// JSON-RPC endpoint.
    pub rpc_url: String,
    /// Cluster the endpoint belongs to.
    pub cluster: Cluster,
    /// Commitment required for a receipt to verify.
    pub commitment: Commitment,
    /// The USDC mint. Anything paid in another mint is not a payment.
    pub usdc_mint: PubKey,
}

impl SolanaConfig {
    /// Devnet USDC, `confirmed` commitment.
    pub fn devnet(rpc_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            cluster: Cluster::Devnet,
            commitment: Commitment::Confirmed,
            usdc_mint: tx::pubkey_from_base58(tx::USDC_DEVNET_MINT)
                .expect("USDC_DEVNET_MINT is a valid constant"),
        }
    }

    /// Mainnet-beta USDC, `finalized` commitment (real money: prefer the
    /// stronger guarantee by default).
    pub fn mainnet(rpc_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            cluster: Cluster::MainnetBeta,
            commitment: Commitment::Finalized,
            usdc_mint: tx::pubkey_from_base58(tx::USDC_MAINNET_MINT)
                .expect("USDC_MAINNET_MINT is a valid constant"),
        }
    }

    /// The mint as base58.
    pub fn mint_base58(&self) -> String {
        tx::pubkey_to_base58(&self.usdc_mint)
    }
}

/// Domain-separated binding reference: `blake3("patala-solana-pay-v1" ||
/// payer || reference)`. Ported from magnetite's `binding_reference` with a
/// patala-specific domain tag (this crate is not magnetite, so it does not
/// reuse magnetite's domain string — a receipt from one must never be
/// mistakable for a receipt from the other).
pub fn binding_reference(payer: &PubKey, reference: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"patala-solana-pay-v1");
    h.update(&payer.0);
    h.update(&(reference.len() as u64).to_le_bytes());
    h.update(reference.as_bytes());
    *h.finalize().as_bytes()
}

/// The exact memo string a bound transaction must carry.
///
/// The binding is deterministic in `(payer, reference)` and domain-separated,
/// so the same order always maps to the same memo and a different reference
/// never collides — this is what lets [`SolanaRail::verify`] fail closed on a
/// transaction whose on-chain memo does not match the receipt it is handed.
///
/// ```
/// use patala_solana::{binding_memo, binding_reference};
/// use patala_solana::keys::PubKey;
///
/// let payer = PubKey([7u8; 32]);
///
/// // Every bound memo is domain-tagged and hex-encoded.
/// assert!(binding_memo(&payer, "order-42").starts_with("patala:v1:"));
///
/// // Deterministic in (payer, reference)...
/// assert_eq!(binding_reference(&payer, "order-42"), binding_reference(&payer, "order-42"));
/// // ...and a different reference never collides (anti-replay).
/// assert_ne!(binding_reference(&payer, "order-42"), binding_reference(&payer, "order-43"));
/// ```
pub fn binding_memo(payer: &PubKey, reference: &str) -> String {
    format!(
        "patala:v1:{}",
        hex::encode(binding_reference(payer, reference))
    )
}

/// The rail-specific data carried in `patala_core::Receipt::proof` (JSON,
/// opaque to `patala-core`). This is the "on-chain binding" the task and
/// `PATALA.md` §3 require [`SolanaRail::verify`] to check against — a receipt
/// with no binding, or one that does not match it, is invalid, never
/// assumed-valid.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SolanaBinding {
    /// Chain name, always `"solana"` (checked, not assumed, in `verify`).
    chain: String,
    /// The on-chain transaction signature (base58).
    tx_signature: String,
    /// The token mint that was transferred (base58).
    mint: String,
    /// The paying wallet (base58) — also the signing identity (`PATALA.md` §6).
    payer: String,
    /// The destination wallet (base58), echoing `PayRequest::destination`.
    destination: String,
    /// Hex `blake3("patala-solana-pay-v1" || payer || reference)`; also
    /// carried on-chain as a memo so the binding is not merely asserted by
    /// the receipt.
    reference: String,
}

/// SPL-USDC-on-Solana payment rail. `NonCustodialFinal` (`PATALA.md` §3, §4).
pub struct SolanaRail {
    cfg: SolanaConfig,
    rpc: Arc<dyn SolanaRpc>,
    /// The wallet this rail can spend from — absent for a verify-only rail
    /// (a server that only ever checks receipts, never pays).
    signer: Option<Keypair>,
    capabilities: RailCapabilities,
}

impl SolanaRail {
    /// Build a rail over an arbitrary RPC implementation (unit tests pass a
    /// fake; production passes [`rpc::HttpRpc`]). No signer — verify-only
    /// until [`Self::with_signer`].
    pub fn new(cfg: SolanaConfig, rpc: Arc<dyn SolanaRpc>) -> Self {
        let capabilities = RailCapabilities {
            class: RailClass::NonCustodialFinal,
            reversible: false,
            requires_kyc: false,
            holds_funds: false,
            currencies: vec!["USDC".to_string()],
            settlement: cfg.commitment.settlement(),
        };
        Self {
            cfg,
            rpc,
            signer: None,
            capabilities,
        }
    }

    /// Attach a signing key so this rail can submit transactions itself. Per
    /// `PATALA.md` §6, this same key is both the rail's signing identity and
    /// — since Solana addresses are Ed25519 public keys — the wallet the
    /// funds move from. No separate mapping table.
    pub fn with_signer(mut self, signer: Keypair) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Build a rail whose signer (if any) is loaded from
    /// `SOLANA_KEYPAIR_PATH`/`SOLANA_KEYPAIR` (see [`Keypair::from_env`]).
    pub fn from_env(cfg: SolanaConfig, rpc: Arc<dyn SolanaRpc>) -> Result<Self> {
        let rail = Self::new(cfg, rpc);
        Ok(match Keypair::from_env().map_err(Error::from)? {
            Some(k) => rail.with_signer(k),
            None => rail,
        })
    }

    /// The wallet this rail can sign for, if any.
    pub fn signer_pubkey(&self) -> Option<PubKey> {
        self.signer.as_ref().map(|s| s.pubkey())
    }

    /// The configuration (read-only).
    pub fn config(&self) -> &SolanaConfig {
        &self.cfg
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Does any (top-level or inner) memo instruction carry exactly `want`?
/// Ported from magnetite's `memo_matches`.
fn memo_matches(txn: &serde_json::Value, want: &str) -> bool {
    fn scan(ixs: Option<&serde_json::Value>, want: &str) -> bool {
        ixs.and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|ix| {
                    ix.get("program").and_then(|v| v.as_str()) == Some("spl-memo")
                        && ix.get("parsed").and_then(|v| v.as_str()) == Some(want)
                })
            })
            .unwrap_or(false)
    }
    let msg = txn.get("transaction").and_then(|t| t.get("message"));
    if scan(msg.and_then(|m| m.get("instructions")), want) {
        return true;
    }
    txn.get("meta")
        .and_then(|m| m.get("innerInstructions"))
        .and_then(|v| v.as_array())
        .map(|groups| groups.iter().any(|g| scan(g.get("instructions"), want)))
        .unwrap_or(false)
}

/// Net per-owner balance change for `mint`, in integer smallest units. Ported
/// from magnetite's `mint_deltas`.
fn mint_deltas(txn: &serde_json::Value, mint: &str) -> Result<Vec<(String, i128)>> {
    let meta = txn
        .get("meta")
        .ok_or_else(|| Error::Rail("transaction has no meta".into()))?;
    let mut deltas: Vec<(String, i128)> = Vec::new();

    let mut apply = |list: Option<&serde_json::Value>, sign: i128| -> Result<()> {
        let empty: Vec<serde_json::Value> = Vec::new();
        for e in list.and_then(|v| v.as_array()).unwrap_or(&empty) {
            if e.get("mint").and_then(|v| v.as_str()) != Some(mint) {
                continue;
            }
            let owner = e
                .get("owner")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Rail("token balance without owner".into()))?;
            let amount: i128 = e
                .get("uiTokenAmount")
                .and_then(|u| u.get("amount"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Rail("token balance without amount".into()))?
                .parse()
                .map_err(|_| Error::Rail("token amount is not an integer".into()))?;
            match deltas.iter_mut().find(|(o, _)| o == owner) {
                Some(d) => d.1 += sign * amount,
                None => deltas.push((owner.to_string(), sign * amount)),
            }
        }
        Ok(())
    };
    apply(meta.get("preTokenBalances"), -1)?;
    apply(meta.get("postTokenBalances"), 1)?;
    Ok(deltas)
}

#[async_trait]
impl PaymentRail for SolanaRail {
    fn id(&self) -> &str {
        "solana"
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        if req.currency != "USDC" {
            return Err(Error::InvalidRequest(format!(
                "solana rail only supports USDC, got {}",
                req.currency
            )));
        }
        Ok(Quote {
            rail_id: self.id().to_string(),
            amount_minor: req.amount_minor,
            currency: req.currency.clone(),
            // The Solana network fee (a few thousand lamports) is paid in
            // SOL by whoever signs the transaction, not deducted from the
            // USDC amount transferred — `patala_core::Quote::fee_minor` is
            // denominated in the request's own currency, which has no field
            // for a *different* currency's fee. Stated plainly rather than
            // hidden: this rail's `fee_minor` is always 0; the real SOL gas
            // cost is a separate, out-of-band cost of using this rail at
            // all (`PATALA.md` §8).
            fee_minor: 0,
            total_minor: req.amount_minor,
            settlement: self.capabilities.settlement,
            // A blockhash is valid for ~150 slots (~60-90s at Solana's
            // ~400-600ms slot time); a quote older than that needs a fresh
            // blockhash to actually submit, so treat it as stale first.
            expires_at_unix: now_unix().saturating_add(60),
        })
    }

    async fn charge(&self, req: &PayRequest) -> Result<Receipt> {
        req.validate()?;
        if req.currency != "USDC" {
            return Err(Error::InvalidRequest(format!(
                "solana rail only supports USDC, got {}",
                req.currency
            )));
        }
        let signer = self.signer.as_ref().ok_or_else(|| {
            Error::Rail(
                "solana rail holds no signing key; it is verify-only (configure one with \
                 `with_signer`/`from_env` to charge)"
                    .into(),
            )
        })?;
        let dest = tx::pubkey_from_base58(&req.destination).map_err(|e| {
            Error::InvalidRequest(format!("destination is not a valid solana address: {e}"))
        })?;
        let payer = signer.pubkey();

        let blockhash = self
            .rpc
            .get_latest_blockhash(self.cfg.commitment.as_str())
            .await
            .map_err(Error::from)?;
        let bh: [u8; 32] = bs58::decode(&blockhash)
            .into_vec()
            .ok()
            .and_then(|v| <[u8; 32]>::try_from(v).ok())
            .ok_or_else(|| Error::Rail("blockhash is not 32 bytes".into()))?;

        let source =
            tx::associated_token_address(&payer, &self.cfg.usdc_mint).map_err(Error::from)?;
        let dest_ata =
            tx::associated_token_address(&dest, &self.cfg.usdc_mint).map_err(Error::from)?;
        let memo_str = binding_memo(&payer, &req.reference);
        let ixs = vec![
            tx::memo(payer, &memo_str),
            tx::transfer_checked(
                source,
                self.cfg.usdc_mint,
                dest_ata,
                payer,
                req.amount_minor,
                tx::USDC_DECIMALS,
            ),
        ];
        let message = tx::serialize_message(&payer, &ixs, &bh);
        let sig = signer.sign(&message);
        let wire = tx::wire_transaction(&sig.0, &message);
        let signature = self
            .rpc
            .send_transaction(&tx::b64(&wire))
            .await
            .map_err(Error::from)?;

        let binding = SolanaBinding {
            chain: "solana".to_string(),
            tx_signature: signature,
            mint: self.cfg.mint_base58(),
            payer: tx::pubkey_to_base58(&payer),
            destination: req.destination.clone(),
            reference: hex::encode(binding_reference(&payer, &req.reference)),
        };
        let proof = serde_json::to_vec(&binding)
            .map_err(|e| Error::Rail(format!("encode receipt proof: {e}")))?;

        Ok(Receipt {
            rail_id: self.id().to_string(),
            amount_minor: req.amount_minor,
            currency: req.currency.clone(),
            reference: req.reference.clone(),
            proof,
            settled_at_unix: now_unix(),
        })
    }

    async fn verify(&self, receipt: &Receipt) -> Result<bool> {
        // Fail closed: a receipt naming a different rail, or one whose proof
        // does not even parse, is never assumed valid.
        if receipt.rail_id != self.id() {
            return Ok(false);
        }
        if receipt.currency != "USDC" {
            return Ok(false);
        }
        let Ok(binding) = serde_json::from_slice::<SolanaBinding>(&receipt.proof) else {
            return Ok(false);
        };

        // 1. Right chain, right mint. A USDC receipt is not a receipt in
        //    some other token the payer happened to have.
        if binding.chain != "solana" {
            return Ok(false);
        }
        if binding.mint != self.cfg.mint_base58() {
            return Ok(false);
        }
        let Ok(payer) = tx::pubkey_from_base58(&binding.payer) else {
            return Ok(false);
        };

        // 2. The binding must be the one derived from (payer, reference) —
        //    a receipt cannot be re-pointed at a different reference by
        //    editing a field.
        let expected_ref = hex::encode(binding_reference(&payer, &receipt.reference));
        if binding.reference != expected_ref {
            return Ok(false);
        }

        // 3. Chain state. Unreachable RPC propagates as `Err` — an
        //    operational failure to even check, never "verified" — per
        //    `patala_core::PaymentRail::verify`'s own contract.
        let txn = match self
            .rpc
            .get_transaction(&binding.tx_signature, self.cfg.commitment.as_str())
            .await
        {
            Ok(Some(txn)) => txn,
            Ok(None) => return Ok(false), // cluster has never heard of this signature
            Err(e) => return Err(e.into()),
        };

        // 4. The transaction must have SUCCEEDED. A landed-but-failed tx
        //    moved nothing.
        match txn.get("meta").and_then(|m| m.get("err")) {
            None => return Ok(false), // no meta.err field at all — malformed
            Some(e) if !e.is_null() => return Ok(false), // transaction failed
            _ => {}
        }
        // Some RPCs echo a confirmationStatus; if present it must be good
        // enough for the configured commitment.
        if let Some(status) = txn.get("confirmationStatus").and_then(|v| v.as_str()) {
            let ok = match self.cfg.commitment {
                Commitment::Confirmed => status == "confirmed" || status == "finalized",
                Commitment::Finalized => status == "finalized",
            };
            if !ok {
                return Ok(false);
            }
        }

        // 5. The payer must have signed. Otherwise anyone could point a
        //    receipt at someone else's payment.
        let signed = txn
            .get("transaction")
            .and_then(|t| t.get("message"))
            .and_then(|m| m.get("accountKeys"))
            .and_then(|k| k.as_array())
            .map(|keys| {
                let want = tx::pubkey_to_base58(&payer);
                keys.iter().any(|k| {
                    k.get("pubkey").and_then(|v| v.as_str()) == Some(want.as_str())
                        && k.get("signer").and_then(|v| v.as_bool()) == Some(true)
                })
            })
            .unwrap_or(false);
        if !signed {
            return Ok(false);
        }

        // 6. The on-chain memo must be exactly the derived binding, so a
        //    transaction is redeemable for one (payer, reference) and
        //    nothing else.
        let want_memo = binding_memo(&payer, &receipt.reference);
        if !memo_matches(&txn, &want_memo) {
            return Ok(false);
        }

        // 7. The money. Net token-balance deltas for the configured mint
        //    must be EXACTLY: payer -amount_minor, destination
        //    +amount_minor, and no unaccounted third party. Balance deltas
        //    (rather than reading instructions) mean a transfer cancelled
        //    out by a hidden reverse transfer in the same transaction cannot
        //    pass.
        let deltas = mint_deltas(&txn, &self.cfg.mint_base58())?;
        let payer_b58 = tx::pubkey_to_base58(&payer);
        let dest_b58 = binding.destination.clone();
        let expected: Vec<(String, i128)> = vec![
            (dest_b58, receipt.amount_minor as i128),
            (payer_b58, -(receipt.amount_minor as i128)),
        ];
        for (who, want) in &expected {
            let got = deltas
                .iter()
                .find(|(o, _)| o == who)
                .map(|(_, d)| *d)
                .unwrap_or(0);
            if got != *want {
                return Ok(false);
            }
        }
        for (who, d) in &deltas {
            if *d != 0 && !expected.iter().any(|(e, _)| e == who) {
                return Ok(false); // unaccounted balance change
            }
        }

        Ok(true)
    }

    /// Check a destination address offline, before any money moves —
    /// delegated whole to [`destination::validate`], which is a free function
    /// so it needs no configured rail, no RPC URL and no keypair to run.
    ///
    /// See that module for exactly what is decidable from a Solana address
    /// alone (alphabet, 32-byte length, on-curve-ness, the well-known
    /// programs and mints) and what is deliberately not attempted (account
    /// existence, rent-exemption, token-account state — all chain queries —
    /// and ownership, which patala never guesses at).
    fn validate_destination(&self, dest: &str) -> patala_core::DestinationVerdict {
        debug_assert_eq!(self.id(), destination::RAIL_ID);
        destination::validate(dest)
    }

    /// Crypto settlement here is final by construction — there is no
    /// reversal to perform, and this rail will not pretend otherwise
    /// (`PATALA.md` §3, §8).
    ///
    /// **This does not mean a customer cannot be paid back.** It means this
    /// rail cannot *undo* a transaction. Giving the money back on Solana is a
    /// compensating payment: ask the customer for a destination (never reuse
    /// the address the payment came from — on Solana that is very often an
    /// exchange withdrawal address), run it through
    /// [`Self::validate_destination`], show a human the verdict and its
    /// caveat, then [`Self::charge`] to it with a fresh
    /// [`PayRequest::reference`]. The receipt that `charge` returns is the
    /// proof of the payout; the original receipt is unchanged. See
    /// [`patala_core::destination`].
    async fn refund(&self, _receipt: &Receipt) -> Result<Receipt> {
        Err(Error::Unsupported("refund"))
    }
}
