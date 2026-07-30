//! # patala-stellar
//!
//! Rail #3 (`PATALA.md` §4, §6): native Circle USDC on Stellar,
//! `NonCustodialFinal`. Real Ed25519 signing + XDR transaction construction +
//! Horizon submission/verification, built new for `patala` (there is no
//! magnetite precursor to port, unlike `patala_solana`).
//!
//! # Shape (mirrors `patala_solana`/`patala_core::PaymentRail`)
//!
//! * [`tx`] — StrKey-independent XDR construction: the payment operation, the
//!   `TransactionSignaturePayload` signing base, the transaction hash, and
//!   envelope assembly/decoding, on top of the Stellar Development
//!   Foundation's own `stellar-xdr` codec (generated from the same `.x`
//!   definitions stellar-core uses).
//! * [`keys`] — the Ed25519 keypair, StrKey-encoded (`G...`/`S...`) via the
//!   Foundation's `stellar-strkey` crate.
//! * [`rpc`] — the `StellarRpc` seam (so `charge`/`verify` are unit-testable
//!   offline against a fake) plus the real `HorizonRpc` REST client.
//!
//! # Atomic multi-party splits (B1, `docs/shared-economics.md` §5)
//!
//! [`PaymentRail::charge`]/[`PaymentRail::verify`] are single-recipient —
//! that is `patala_core`'s own seam. [`StellarRail::charge_split`]/
//! [`StellarRail::verify_split`] are the rail-specific, **atomic** N-leg
//! counterpart, living beneath that seam: one Stellar transaction, one to
//! [`tx::MAX_OPERATIONS`] `PAYMENT` operations, where either every leg lands
//! or none does. They are deliberately not on the [`PaymentRail`] trait —
//! see their own docs for why — so a consumer that needs one holds a
//! concrete [`StellarRail`], not a `Box<dyn PaymentRail>`. Tested offline
//! only; **not** run against a live network from this environment.
//!
//! # Keys (`PATALA.md` §6)
//!
//! Stellar is Ed25519-native, exactly like Solana: [`StellarRail`]'s
//! configured [`keys::Keypair`] is simultaneously the signing identity and
//! the wallet the funds move from — StrKey is just an encoding of the same
//! raw public key, so there is no identity → wallet mapping table. Load one
//! from `STELLAR_SECRET_KEY` (a StrKey secret seed, `S...`) via
//! [`keys::Keypair::from_env`]. It is never logged, never serialized, never
//! written anywhere by this crate.
//!
//! # Money math (`PATALA.md` §8)
//!
//! USDC on Stellar has [`tx::USDC_DECIMALS`] (7) — classic Stellar XDR
//! amounts are always this fixed-point `int64` scale; there is no separate
//! decimals field. Every amount here is that integer count of ten-millionths
//! — `u64`/`i64`, never a float, per `patala_core::PayRequest`/`Quote`/
//! `Receipt`.
//!
//! # Finality — no "commitment" knob
//!
//! Unlike Solana's probabilistic `confirmed`/`finalized` levels,
//! [`StellarRail::verify`] treats a transaction Horizon reports as
//! `successful` inside a closed ledger as final: Stellar's federated
//! Byzantine agreement (SCP) does not have Solana's "confirmed but could
//! still be skipped" intermediate state — a ledger closing *is* the finality
//! event (`PATALA.md` §6, ~3-5s). There is deliberately no extra
//! confirmation-depth parameter to configure here.
//!
//! # What `verify` actually checks
//!
//! 1. The receipt names this rail and currency (`"USDC"`).
//! 2. Its `proof` parses as a `StellarBinding` at all.
//! 3. The claimed asset (code + issuer) matches this rail's *configured*
//!    issuer — never the receipt's own say-so.
//! 4. `binding.amount_stroops == receipt.amount_minor` (the outer `Receipt`
//!    and the opaque proof blob must agree — tampering either one alone is
//!    caught).
//! 5. The memo-hash binding re-derives from `(rail id, source, destination,
//!    reference)` — a receipt cannot be re-pointed at a different reference
//!    or destination by editing one field.
//! 6. **Offline, no network:** the whole `Transaction` is rebuilt from the
//!    binding's own scalar fields, hashed, and the claimed signature is
//!    Ed25519-verified against the claimed source over that hash. This is a
//!    genuine cryptographic guarantee checkable without Horizon at all — only
//!    the real secret key could have produced it.
//! 7. **Online:** Horizon is asked for this transaction hash. Not found ⇒
//!    deny. Found ⇒ `successful` must be `true`, and the envelope XDR Horizon
//!    actually returns is decoded and compared operation-for-operation
//!    (source, destination, asset, amount, memo) against the binding — never
//!    trusting Horizon's summary fields alone for the money-moving details.
//! 8. Any RPC failure at step 7 propagates as `Err` (an operational failure
//!    to even check — `patala_core::PaymentRail::verify`'s own contract),
//!    never as an implied "verified".
//!
//! # Honesty (`PATALA.md` §8) — READ THIS
//!
//! **Testnet: one payment operation has settled.** On 2026-07-30, a
//! throwaway keypair paid another throwaway keypair a single-leg
//! USDC-shaped payment (self-issued `CreditAlphanum4` asset coded `"USDC"`,
//! not Circle's own testnet issuer) on Stellar **testnet**, built and
//! submitted through this crate's real public entry point,
//! [`StellarRail::charge`], and independently re-confirmed by
//! [`StellarRail::verify`] reading it back from Horizon: tx hash
//! `32663937fe1407f9de3e781effa6ac9f4b1d29340ea63e72f6335a6c91effb89`,
//! ledger `3882739`. Reproduce it: `PATALA_LIVE_TESTNET=1 cargo test -p
//! patala-stellar live_testnet_round_trip -- --ignored --nocapture` (see
//! `live_testnet_round_trip_settles_a_real_payment` in `src/tests.rs`, and
//! `README.md` for the full evidence and caveats).
//!
//! **Read that narrowly.** It proves the wire encoding, signing base,
//! Horizon submission, and online verification work end-to-end against real
//! testnet infrastructure, through this crate's `charge`/`verify` API,
//! **single-leg only**. It does **not** prove: mainnet (untouched, a
//! structurally different real-money network); atomic multi-party splits
//! ([`StellarRail::charge_split`]/[`StellarRail::verify_split`], added after
//! this test settled, are tested offline only — never run against a live
//! network from this environment); or that Circle's own USDC issuer behaves
//! identically (only the wire shape was exercised). Every offline test in
//! the `tests` module still runs with
//! no network — Horizon is a scripted fake there — and a known-answer
//! transaction (fixed seed, fixed inputs) is round-tripped through
//! `stellar-xdr`'s own spec-generated decoder to catch wire-format bugs.
//! A second live test, gated on `PATALA_STELLAR_LIVE` exactly as
//! `patala-solana` gates its analogous test on `PATALA_SOLANA_LIVE_RPC`,
//! checks only Horizon connectivity (it predates the round trip above).
//! Mainnet remains **UNVERIFIED AGAINST LIVE**. See `README.md`.

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
use rpc::StellarRpc;

/// Everything that can go wrong on this rail. Every variant is a *refusal*:
/// none of them ever results in an entitlement being granted. Converted to
/// `patala_core::Error` at the [`PaymentRail`] boundary.
#[derive(Debug, thiserror::Error)]
pub enum StellarError {
    /// Horizon was unreachable, slow, or answered with an error.
    #[error("stellar horizon: {0}")]
    Rpc(String),
    /// A StrKey address failed to decode (bad checksum, wrong version byte,
    /// wrong length, or simply not StrKey at all).
    #[error("not a valid stellar address: {0}")]
    BadAddress(String),
    /// Misconfiguration — bad issuer address, unusable keypair, bad asset
    /// code, ...
    #[error("stellar rail misconfigured: {0}")]
    Config(String),
    /// Building, encoding, or decoding an XDR value failed.
    #[error("stellar xdr: {0}")]
    Xdr(String),
}

impl From<StellarError> for Error {
    fn from(e: StellarError) -> Self {
        Error::Rail(e.to_string())
    }
}

/// Which Stellar network the rail is pointed at. `Public` moves **real
/// money**.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Network {
    /// Real funds. Real losses.
    Public,
    /// Free test USDC / test assets.
    Testnet,
}

impl Network {
    /// The network passphrase Stellar defines for this network — the input
    /// to [`tx::network_id`]. These exact strings are part of the Stellar
    /// protocol, not this crate's invention.
    pub fn passphrase(&self) -> &'static str {
        match self {
            Network::Public => "Public Global Stellar Network ; September 2015",
            Network::Testnet => "Test SDF Network ; September 2015",
        }
    }

    /// Does this network move real money?
    pub fn is_mainnet(&self) -> bool {
        matches!(self, Network::Public)
    }

    /// Parse a network name; unknown names are a hard error (never a
    /// default).
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "public" | "pubnet" | "mainnet" => Ok(Network::Public),
            "testnet" => Ok(Network::Testnet),
            other => Err(StellarError::Config(format!("unknown stellar network {other:?}")).into()),
        }
    }
}

/// Circle's publicly-documented USDC issuing account on the Stellar public
/// network. **Stated as a public fact, not independently re-verified against
/// a live ledger from this environment** — pass your own `usdc_issuer` in
/// [`StellarConfig`] if you need certainty, and see `README.md`'s honesty
/// section.
pub const CIRCLE_USDC_ISSUER_PUBLIC: &str =
    "GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN";

/// Static configuration for the rail. Built by the caller (a backend reads it
/// from env and fails loudly if it is wrong).
#[derive(Clone, Debug)]
pub struct StellarConfig {
    /// Which network this rail talks to.
    pub network: Network,
    /// The USDC issuer account (StrKey `G...`). Anything paid in an asset
    /// with a different issuer, or a different code, is not a USDC payment.
    pub usdc_issuer: String,
    /// Per-operation fee, in stroops (1 XLM = 10^7 stroops). `PATALA.md` §6
    /// measures Stellar fees at roughly 100-1,000 stroops
    /// (~0.00001-0.0001 XLM); this is a *bid*, not a guarantee — network
    /// surge pricing can require more.
    pub base_fee_stroops: u32,
}

impl StellarConfig {
    /// Testnet, Circle's testnet USDC issuer must be supplied by the caller
    /// (unlike mainnet, the testnet issuer rotates and is not a fixed
    /// well-known constant) — this constructor takes it explicitly.
    pub fn testnet(usdc_issuer: impl Into<String>) -> Self {
        Self {
            network: Network::Testnet,
            usdc_issuer: usdc_issuer.into(),
            base_fee_stroops: 100,
        }
    }

    /// Public network, using [`CIRCLE_USDC_ISSUER_PUBLIC`] — see that
    /// constant's honesty caveat.
    pub fn public() -> Self {
        Self {
            network: Network::Public,
            usdc_issuer: CIRCLE_USDC_ISSUER_PUBLIC.to_string(),
            base_fee_stroops: 100,
        }
    }
}

/// The rail-specific data carried in `patala_core::Receipt::proof` (JSON,
/// opaque to `patala-core`). This is the "on-chain binding" `PATALA.md` §3
/// requires [`StellarRail::verify`] to check against — a receipt with no
/// binding, or one that does not match it, is invalid, never assumed-valid.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StellarBinding {
    /// Chain name, always `"stellar"` (checked, not assumed, in `verify`).
    chain: String,
    /// Which network this was submitted to — a testnet receipt must never
    /// verify against a mainnet-configured rail or vice versa.
    network_passphrase: String,
    /// The transaction hash (hex) — also what Horizon indexes the
    /// transaction by.
    tx_hash: String,
    /// The paying wallet (StrKey `G...`) — also the signing identity
    /// (`PATALA.md` §6).
    source: String,
    /// The destination wallet (StrKey `G...`), echoing
    /// `PayRequest::destination`.
    destination: String,
    /// The asset code, always `"USDC"`.
    asset_code: String,
    /// The asset issuer (StrKey `G...`).
    asset_issuer: String,
    /// The amount actually encoded on-chain (stroops-scale integer).
    amount_stroops: i64,
    /// The exact sequence number consumed, needed to rebuild and re-hash the
    /// identical transaction offline.
    seq_num: i64,
    /// The exact per-operation fee used.
    fee: u32,
    /// Hex `SHA256("patala-stellar-pay-v1" || rail id || source ||
    /// destination || reference)` — also carried on-chain as `MEMO_HASH`.
    memo_hash: String,
    /// Hex Ed25519 signature over `tx_hash`, by `source`.
    signature: String,
}

/// Native-USDC-on-Stellar payment rail. `NonCustodialFinal` (`PATALA.md` §3,
/// §4, §6).
pub struct StellarRail {
    cfg: StellarConfig,
    rpc: Arc<dyn StellarRpc>,
    /// The wallet this rail can spend from — absent for a verify-only rail
    /// (a server that only ever checks receipts, never pays).
    signer: Option<Keypair>,
    capabilities: RailCapabilities,
}

impl StellarRail {
    /// Build a rail over an arbitrary RPC implementation (unit tests pass a
    /// fake; production passes [`rpc::HorizonRpc`]). No signer — verify-only
    /// until [`Self::with_signer`].
    pub fn new(cfg: StellarConfig, rpc: Arc<dyn StellarRpc>) -> Self {
        let capabilities = RailCapabilities {
            class: RailClass::NonCustodialFinal,
            reversible: false,
            requires_kyc: false,
            holds_funds: false,
            currencies: vec!["USDC".to_string()],
            settlement: Settlement::Seconds(5),
            // NOT "unbuilt" — `charge_split`/`verify_split` DO exist on this rail (B1).
            // This flag is read through the `PaymentRail` trait, where those methods are
            // deliberately unreachable, so it reports what a trait-object holder can
            // actually get: no atomic split. A consumer holding a concrete `StellarRail`
            // has one. See `charge_split`'s docs for the full rationale.
            atomic_multi_party: false,
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
    /// — since a Stellar address is nothing but an Ed25519 public key,
    /// StrKey-encoded — the wallet the funds move from. No separate mapping
    /// table.
    pub fn with_signer(mut self, signer: Keypair) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Build a rail whose signer (if any) is loaded from
    /// `STELLAR_SECRET_KEY` (see [`Keypair::from_env`]).
    pub fn from_env(cfg: StellarConfig, rpc: Arc<dyn StellarRpc>) -> Result<Self> {
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
    pub fn config(&self) -> &StellarConfig {
        &self.cfg
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[async_trait]
impl PaymentRail for StellarRail {
    fn id(&self) -> &str {
        "stellar"
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        req.validate()?;
        if req.currency != "USDC" {
            return Err(Error::InvalidRequest(format!(
                "stellar rail only supports USDC, got {}",
                req.currency
            )));
        }
        Ok(Quote {
            rail_id: self.id().to_string(),
            amount_minor: req.amount_minor,
            currency: req.currency.clone(),
            // The Stellar network fee is paid in XLM (stroops) by the
            // signer, not deducted from the USDC amount transferred —
            // `patala_core::Quote::fee_minor` is denominated in the
            // request's own currency, which has no field for a *different*
            // currency's fee. Stated plainly rather than hidden: this rail's
            // `fee_minor` is always 0; the real XLM gas cost
            // (`self.cfg.base_fee_stroops` stroops, ~0.00001-0.0001 XLM per
            // `PATALA.md` §6) is a separate, out-of-band cost of using this
            // rail at all (`PATALA.md` §8) — mirrors `patala-solana`'s
            // identical stance on SOL gas.
            fee_minor: 0,
            total_minor: req.amount_minor,
            settlement: self.capabilities.settlement,
            // A Stellar ledger closes every ~5s; treat a quote as needing a
            // fresh sequence number/fee bid after roughly a minute.
            expires_at_unix: now_unix().saturating_add(60),
        })
    }

    async fn charge(&self, req: &PayRequest) -> Result<Receipt> {
        req.validate()?;
        if req.currency != "USDC" {
            return Err(Error::InvalidRequest(format!(
                "stellar rail only supports USDC, got {}",
                req.currency
            )));
        }
        let signer = self.signer.as_ref().ok_or_else(|| {
            Error::Rail(
                "stellar rail holds no signing key; it is verify-only (configure one with \
                 `with_signer`/`from_env` to charge)"
                    .into(),
            )
        })?;
        let dest = PubKey::from_strkey(&req.destination).map_err(|e| {
            Error::InvalidRequest(format!("destination is not a valid stellar address: {e}"))
        })?;
        let issuer = PubKey::from_strkey(&self.cfg.usdc_issuer)
            .map_err(|e| Error::Rail(format!("configured usdc_issuer is invalid: {e}")))?;
        let asset = tx::usdc_asset("USDC", issuer.0).map_err(Error::from)?;
        let amount = i64::try_from(req.amount_minor).map_err(|_| {
            Error::InvalidRequest("amount_minor exceeds Stellar's i64 range".into())
        })?;

        let source = signer.pubkey();
        let seq = self
            .rpc
            .load_sequence(&source.to_strkey())
            .await
            .map_err(Error::from)?;
        let next_seq = seq
            .checked_add(1)
            .ok_or_else(|| Error::Rail("account sequence number overflow".into()))?;

        let memo = tx::memo_hash(
            self.id(),
            &source.to_strkey(),
            &req.destination,
            &req.reference,
        );
        let unsigned = tx::build_transaction(
            source.0,
            dest.0,
            asset,
            amount,
            next_seq,
            self.cfg.base_fee_stroops,
            memo,
        );
        let net_id = tx::network_id(self.cfg.network.passphrase());
        let hash = tx::tx_hash(net_id, &unsigned).map_err(Error::from)?;
        let sig = signer.sign(&hash);
        let env = tx::envelope(unsigned, source.0, sig.0).map_err(Error::from)?;
        let env_b64 = tx::envelope_to_xdr_base64(&env).map_err(Error::from)?;

        let submitted = self
            .rpc
            .submit_transaction(&env_b64)
            .await
            .map_err(Error::from)?;
        if !submitted.successful {
            return Err(Error::Rail(
                "stellar transaction failed on submission".into(),
            ));
        }
        if submitted.hash != hex::encode(hash) {
            // Horizon's own report of what it submitted disagrees with what
            // we computed locally — refuse to issue a receipt rather than
            // trust a possibly-substituted transaction.
            return Err(Error::Rail(
                "horizon-reported transaction hash does not match the locally computed hash".into(),
            ));
        }

        let binding = StellarBinding {
            chain: "stellar".to_string(),
            network_passphrase: self.cfg.network.passphrase().to_string(),
            tx_hash: submitted.hash,
            source: source.to_strkey(),
            destination: req.destination.clone(),
            asset_code: "USDC".to_string(),
            asset_issuer: issuer.to_strkey(),
            amount_stroops: amount,
            seq_num: next_seq,
            fee: self.cfg.base_fee_stroops,
            memo_hash: hex::encode(memo),
            signature: hex::encode(sig.0),
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
        // Fail closed: a receipt naming a different rail/currency, or one
        // whose proof does not even parse, is never assumed valid.
        if receipt.rail_id != self.id() {
            return Ok(false);
        }
        if receipt.currency != "USDC" {
            return Ok(false);
        }
        let Ok(binding) = serde_json::from_slice::<StellarBinding>(&receipt.proof) else {
            return Ok(false);
        };

        // 1. Right chain, right network, right asset. A USDC receipt is not
        //    a receipt in some other asset the source happened to hold, and
        //    a testnet receipt must never verify against a mainnet rail.
        if binding.chain != "stellar" {
            return Ok(false);
        }
        if binding.network_passphrase != self.cfg.network.passphrase() {
            return Ok(false);
        }
        let Ok(configured_issuer) = PubKey::from_strkey(&self.cfg.usdc_issuer) else {
            return Ok(false);
        };
        if binding.asset_code != "USDC" || binding.asset_issuer != configured_issuer.to_strkey() {
            return Ok(false);
        }

        // 2. The outer `Receipt` and the opaque proof blob must agree —
        //    tampering either one alone is caught.
        if binding.amount_stroops != receipt.amount_minor as i64 {
            return Ok(false);
        }

        // 3. The binding must be the one derived from (source, destination,
        //    reference) — a receipt cannot be re-pointed at a different
        //    reference or destination by editing a field.
        let expected_memo = hex::encode(tx::memo_hash(
            self.id(),
            &binding.source,
            &binding.destination,
            &receipt.reference,
        ));
        if binding.memo_hash != expected_memo {
            return Ok(false);
        }

        let (Ok(source_pk), Ok(dest_pk)) = (
            PubKey::from_strkey(&binding.source),
            PubKey::from_strkey(&binding.destination),
        ) else {
            return Ok(false);
        };
        let (Ok(memo_bytes), Ok(sig_bytes)) = (
            hex::decode(&binding.memo_hash),
            hex::decode(&binding.signature),
        ) else {
            return Ok(false);
        };
        let (Ok(memo_arr), Ok(sig_arr)) = (
            <[u8; 32]>::try_from(memo_bytes),
            <[u8; 64]>::try_from(sig_bytes),
        ) else {
            return Ok(false);
        };

        // 4. Offline, no network: rebuild the exact transaction from the
        //    binding's own scalar fields, hash it, and require both that the
        //    hash matches the claimed `tx_hash` (self-consistency — catches
        //    a corrupted/hand-edited proof blob before ever touching the
        //    network) and that the claimed signature is a genuine Ed25519
        //    signature by `source` over that hash (only the real secret key
        //    could produce this).
        let Ok(asset) = tx::usdc_asset(&binding.asset_code, configured_issuer.0) else {
            return Ok(false);
        };
        let rebuilt = tx::build_transaction(
            source_pk.0,
            dest_pk.0,
            asset.clone(),
            binding.amount_stroops,
            binding.seq_num,
            binding.fee,
            memo_arr,
        );
        let net_id = tx::network_id(&binding.network_passphrase);
        let Ok(recomputed_hash) = tx::tx_hash(net_id, &rebuilt) else {
            return Ok(false);
        };
        let Ok(claimed_hash_bytes) = hex::decode(&binding.tx_hash) else {
            return Ok(false);
        };
        if claimed_hash_bytes != recomputed_hash {
            return Ok(false);
        }
        if !Keypair::verify(&source_pk, &recomputed_hash, &keys::Sig(sig_arr)) {
            return Ok(false);
        }

        // 5. Online: the real trust anchor. Horizon must know this
        //    transaction hash, report it as successful, and the envelope it
        //    actually returns must decode to exactly this payment — never
        //    trusting Horizon's summary fields alone for the money-moving
        //    details. An unreachable Horizon propagates as `Err` — an
        //    operational failure to even check, never an implied "verified".
        let record = match self.rpc.get_transaction(&binding.tx_hash).await {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(false), // horizon has never heard of this hash
            Err(e) => return Err(e.into()),
        };
        if !record.successful {
            return Ok(false);
        }
        let Ok(env) = tx::envelope_from_xdr_base64(&record.envelope_xdr) else {
            return Ok(false);
        };
        let Ok(decoded) = tx::decode_single_payment(&env) else {
            return Ok(false);
        };
        if decoded.source_pk != source_pk.0
            || decoded.dest_pk != dest_pk.0
            || decoded.amount != binding.amount_stroops
            || decoded.memo != memo_arr
            || decoded.seq_num != binding.seq_num
            || decoded.fee != binding.fee
            || !tx::asset_is(&decoded.asset, "USDC", configured_issuer.0)
        {
            return Ok(false);
        }

        Ok(true)
    }

    /// Check a destination address offline, before any money moves —
    /// delegated whole to [`destination::validate`], which is a free function
    /// so it needs no configured rail, no Horizon URL and no keypair to run.
    ///
    /// StrKey lets this rail decide more offline than most: a bad checksum is
    /// unambiguous, and the version byte tells an account (`G…`) from a muxed
    /// account (`M…`), a contract (`C…`) and a **secret seed** (`S…`) — which
    /// gets its own loud refusal, because a seed in a destination field is a
    /// key disclosure, not a typo. What still needs Horizon (does the account
    /// exist, does it hold a USDC trustline) is named in that module's docs
    /// and deliberately not guessed at here.
    fn validate_destination(&self, dest: &str) -> patala_core::DestinationVerdict {
        debug_assert_eq!(self.id(), destination::RAIL_ID);
        destination::validate(dest)
    }

    /// Settlement here is final by construction — Stellar payments do not
    /// reverse — and this rail will not pretend otherwise (`PATALA.md` §3,
    /// §8).
    ///
    /// **This does not mean a customer cannot be paid back.** It means this
    /// rail cannot *undo* a transaction. Giving the money back on Stellar is a
    /// compensating payment: ask the customer for a destination (never reuse
    /// the address the payment came from — it is very often an exchange
    /// withdrawal address), run it through [`Self::validate_destination`],
    /// show a human the verdict and its caveat, then [`Self::charge`] to it
    /// with a fresh [`PayRequest::reference`]. The receipt that `charge`
    /// returns is the proof of the payout; the original receipt is unchanged.
    /// See [`patala_core::destination`].
    async fn refund(&self, _receipt: &Receipt) -> Result<Receipt> {
        Err(Error::Unsupported("refund"))
    }
}

/// One payee of an atomic split — see [`StellarRail::charge_split`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitLeg {
    /// The payee's wallet address (StrKey `G...`).
    pub destination: String,
    /// The amount this leg pays, in the rail's currency's minor units
    /// (`"USDC"`'s fixed-point ten-millionths — same scale as
    /// [`PayRequest::amount_minor`]). Must be strictly positive.
    pub amount_minor: u64,
}

impl SplitLeg {
    /// A leg paying `amount_minor` to `destination`.
    pub fn new(destination: impl Into<String>, amount_minor: u64) -> Self {
        Self {
            destination: destination.into(),
            amount_minor,
        }
    }
}

/// The rail-specific data carried in a split [`Receipt::proof`] — the N-leg
/// counterpart of [`StellarBinding`]. Same shape, same checks, generalised to
/// a `Vec` of `(destination, amount)` legs bound, in order, by
/// [`tx::split_memo_hash`].
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StellarSplitBinding {
    chain: String,
    network_passphrase: String,
    tx_hash: String,
    source: String,
    asset_code: String,
    asset_issuer: String,
    /// `(destination StrKey, amount in stroops)`, in transaction order — the
    /// order [`tx::split_memo_hash`] binds.
    legs: Vec<(String, i64)>,
    seq_num: i64,
    /// The **total** fee bid (all legs together) — matches
    /// [`tx::DecodedPayments::fee`]'s convention.
    fee: u32,
    memo_hash: String,
    signature: String,
}

impl StellarRail {
    /// Pay every leg in `legs` **atomically** — in one Stellar transaction,
    /// where either every leg lands or none does (`docs/shared-economics.md`
    /// §5, `PATALA.md` §4/§6) — using [`tx::build_payment_transaction`], the
    /// N-leg generalisation of what [`PaymentRail::charge`] builds for one.
    ///
    /// **Deliberately not on the [`PaymentRail`] trait.** `patala_core`'s seam
    /// is single-recipient by design (see that trait's docs); an atomic
    /// N-way split is Tier-B, per-rail work that lives *beneath* the seam.
    /// A consumer that needs one holds a concrete [`StellarRail`] (not a
    /// `Box<dyn PaymentRail>`) — see [`RailCapabilities::atomic_multi_party`]'s
    /// docs for why this rail's own `capabilities()` still reports `false`
    /// even though this method exists.
    ///
    /// `receipt.amount_minor` is the **sum** of every leg (see
    /// [`tx::total_amount`]); `receipt.proof` is a `StellarSplitBinding`,
    /// verified by [`Self::verify_split`] (never by the trait's plain
    /// [`PaymentRail::verify`], which only ever reads a `StellarBinding`).
    ///
    /// Refused, exactly as [`tx::build_payment_transaction`] refuses building
    /// one: no legs, more than [`tx::MAX_OPERATIONS`] legs, or any
    /// non-positive/unrepresentable leg amount — named by index. Also refused:
    /// no signer configured, a malformed leg destination, or a blank
    /// `reference`.
    ///
    /// **Honesty:** tested offline only (`src/tests.rs`) against the same
    /// scripted `FakeRpc` `charge`/`verify` use. It has **not** been run
    /// against a live network from this environment — unlike the single-leg
    /// path, which settled once on testnet 2026-07-30 (see `README.md`).
    /// Treat this method as **UNVERIFIED AGAINST LIVE**.
    pub async fn charge_split(&self, legs: &[SplitLeg], reference: &str) -> Result<Receipt> {
        if reference.trim().is_empty() {
            return Err(Error::InvalidRequest("reference must not be empty".into()));
        }
        let signer = self.signer.as_ref().ok_or_else(|| {
            Error::Rail(
                "stellar rail holds no signing key; it is verify-only (configure one with \
                 `with_signer`/`from_env` to charge_split)"
                    .into(),
            )
        })?;
        let issuer = PubKey::from_strkey(&self.cfg.usdc_issuer)
            .map_err(|e| Error::Rail(format!("configured usdc_issuer is invalid: {e}")))?;
        let asset = tx::usdc_asset("USDC", issuer.0).map_err(Error::from)?;

        let mut payment_legs = Vec::with_capacity(legs.len());
        let mut leg_pairs: Vec<(String, i64)> = Vec::with_capacity(legs.len());
        for (i, leg) in legs.iter().enumerate() {
            let dest = PubKey::from_strkey(&leg.destination).map_err(|e| {
                Error::InvalidRequest(format!(
                    "leg {i}: destination is not a valid stellar address: {e}"
                ))
            })?;
            let amount = i64::try_from(leg.amount_minor).map_err(|_| {
                Error::InvalidRequest(format!("leg {i}: amount_minor exceeds Stellar's i64 range"))
            })?;
            payment_legs.push(tx::PaymentLeg::new(dest.0, asset.clone(), amount));
            leg_pairs.push((leg.destination.clone(), amount));
        }

        let source = signer.pubkey();
        let seq = self
            .rpc
            .load_sequence(&source.to_strkey())
            .await
            .map_err(Error::from)?;
        let next_seq = seq
            .checked_add(1)
            .ok_or_else(|| Error::Rail("account sequence number overflow".into()))?;

        let leg_refs: Vec<(&str, i64)> = leg_pairs.iter().map(|(d, a)| (d.as_str(), *a)).collect();
        let memo = tx::split_memo_hash(self.id(), &source.to_strkey(), reference, &leg_refs);

        let fee =
            tx::total_fee(self.cfg.base_fee_stroops, payment_legs.len()).map_err(Error::from)?;
        let unsigned = tx::build_payment_transaction(
            source.0,
            &payment_legs,
            next_seq,
            self.cfg.base_fee_stroops,
            memo,
        )
        .map_err(Error::from)?;
        let net_id = tx::network_id(self.cfg.network.passphrase());
        let hash = tx::tx_hash(net_id, &unsigned).map_err(Error::from)?;
        let sig = signer.sign(&hash);
        let env = tx::envelope(unsigned, source.0, sig.0).map_err(Error::from)?;
        let env_b64 = tx::envelope_to_xdr_base64(&env).map_err(Error::from)?;

        let submitted = self
            .rpc
            .submit_transaction(&env_b64)
            .await
            .map_err(Error::from)?;
        if !submitted.successful {
            return Err(Error::Rail(
                "stellar split transaction failed on submission".into(),
            ));
        }
        if submitted.hash != hex::encode(hash) {
            return Err(Error::Rail(
                "horizon-reported transaction hash does not match the locally computed hash".into(),
            ));
        }

        let total = tx::total_amount(&payment_legs).map_err(Error::from)?;
        let binding = StellarSplitBinding {
            chain: "stellar".to_string(),
            network_passphrase: self.cfg.network.passphrase().to_string(),
            tx_hash: submitted.hash,
            source: source.to_strkey(),
            asset_code: "USDC".to_string(),
            asset_issuer: issuer.to_strkey(),
            legs: leg_pairs,
            seq_num: next_seq,
            fee,
            memo_hash: hex::encode(memo),
            signature: hex::encode(sig.0),
        };
        let proof = serde_json::to_vec(&binding)
            .map_err(|e| Error::Rail(format!("encode receipt proof: {e}")))?;

        Ok(Receipt {
            rail_id: self.id().to_string(),
            amount_minor: total as u64,
            currency: "USDC".to_string(),
            reference: reference.to_string(),
            proof,
            settled_at_unix: now_unix(),
        })
    }

    /// Verify a receipt produced by [`Self::charge_split`] — the N-leg
    /// counterpart of [`PaymentRail::verify`], with exactly the same checks
    /// generalised over every leg: chain/network/asset, the outer
    /// [`Receipt::amount_minor`] against the sum of the legs, the
    /// [`tx::split_memo_hash`] binding, an offline signature re-check, and an
    /// online read-back from Horizon compared operation-for-operation, in
    /// order.
    ///
    /// A receipt built by plain [`PaymentRail::charge`] — a `StellarBinding`,
    /// not a `StellarSplitBinding` — fails to parse here and is refused, not
    /// misread as a one-leg split; the reverse is equally true of
    /// [`PaymentRail::verify`].
    pub async fn verify_split(&self, receipt: &Receipt) -> Result<bool> {
        if receipt.rail_id != self.id() {
            return Ok(false);
        }
        if receipt.currency != "USDC" {
            return Ok(false);
        }
        let Ok(binding) = serde_json::from_slice::<StellarSplitBinding>(&receipt.proof) else {
            return Ok(false);
        };
        if binding.chain != "stellar" {
            return Ok(false);
        }
        if binding.network_passphrase != self.cfg.network.passphrase() {
            return Ok(false);
        }
        let Ok(configured_issuer) = PubKey::from_strkey(&self.cfg.usdc_issuer) else {
            return Ok(false);
        };
        if binding.asset_code != "USDC" || binding.asset_issuer != configured_issuer.to_strkey() {
            return Ok(false);
        }
        if binding.legs.is_empty() {
            return Ok(false);
        }

        // The outer Receipt and the opaque proof blob must agree on the total.
        let mut total: i64 = 0;
        for (_, amount) in &binding.legs {
            let Some(t) = total.checked_add(*amount) else {
                return Ok(false);
            };
            total = t;
        }
        if total != receipt.amount_minor as i64 {
            return Ok(false);
        }

        let leg_refs: Vec<(&str, i64)> =
            binding.legs.iter().map(|(d, a)| (d.as_str(), *a)).collect();
        let expected_memo = hex::encode(tx::split_memo_hash(
            self.id(),
            &binding.source,
            &receipt.reference,
            &leg_refs,
        ));
        if binding.memo_hash != expected_memo {
            return Ok(false);
        }

        let Ok(source_pk) = PubKey::from_strkey(&binding.source) else {
            return Ok(false);
        };
        let Ok(asset) = tx::usdc_asset(&binding.asset_code, configured_issuer.0) else {
            return Ok(false);
        };
        let mut payment_legs = Vec::with_capacity(binding.legs.len());
        for (i, (dest_str, amount)) in binding.legs.iter().enumerate() {
            let Ok(dest_pk) = PubKey::from_strkey(dest_str) else {
                return Ok(false);
            };
            let _ = i;
            payment_legs.push(tx::PaymentLeg::new(dest_pk.0, asset.clone(), *amount));
        }

        let (Ok(memo_bytes), Ok(sig_bytes)) = (
            hex::decode(&binding.memo_hash),
            hex::decode(&binding.signature),
        ) else {
            return Ok(false);
        };
        let (Ok(memo_arr), Ok(sig_arr)) = (
            <[u8; 32]>::try_from(memo_bytes),
            <[u8; 64]>::try_from(sig_bytes),
        ) else {
            return Ok(false);
        };
        let Some(per_op_fee) = u32::try_from(payment_legs.len())
            .ok()
            .filter(|n| *n > 0)
            .map(|n| binding.fee / n)
        else {
            return Ok(false);
        };
        let Ok(rebuilt) = tx::build_payment_transaction(
            source_pk.0,
            &payment_legs,
            binding.seq_num,
            per_op_fee,
            memo_arr,
        ) else {
            return Ok(false);
        };
        let net_id = tx::network_id(&binding.network_passphrase);
        let Ok(recomputed_hash) = tx::tx_hash(net_id, &rebuilt) else {
            return Ok(false);
        };
        let Ok(claimed_hash_bytes) = hex::decode(&binding.tx_hash) else {
            return Ok(false);
        };
        if claimed_hash_bytes != recomputed_hash {
            return Ok(false);
        }
        if !Keypair::verify(&source_pk, &recomputed_hash, &keys::Sig(sig_arr)) {
            return Ok(false);
        }

        // Online: Horizon must know this hash, report it successful, and its
        // envelope must decode to exactly these legs, in order.
        let record = match self.rpc.get_transaction(&binding.tx_hash).await {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        if !record.successful {
            return Ok(false);
        }
        let Ok(env) = tx::envelope_from_xdr_base64(&record.envelope_xdr) else {
            return Ok(false);
        };
        let Ok(decoded) = tx::decode_payments(&env) else {
            return Ok(false);
        };
        if decoded.legs.len() != binding.legs.len()
            || decoded.source_pk != source_pk.0
            || decoded.memo != memo_arr
            || decoded.seq_num != binding.seq_num
            || decoded.fee != binding.fee
        {
            return Ok(false);
        }
        for (i, (dest_str, amount)) in binding.legs.iter().enumerate() {
            let Ok(dest_pk) = PubKey::from_strkey(dest_str) else {
                return Ok(false);
            };
            let d = &decoded.legs[i];
            if d.dest_pk != dest_pk.0
                || d.amount != *amount
                || !tx::asset_is(&d.asset, "USDC", configured_issuer.0)
            {
                return Ok(false);
            }
        }

        Ok(true)
    }
}
