//! Stellar XDR transaction construction, signing base, and hashing.
//!
//! # Dependency choice (see also `README.md`)
//!
//! We build on [`stellar_xdr`] — the Stellar Development Foundation's own
//! crate, generated directly from the same `.x` XDR definitions used by
//! stellar-core (Apache-2.0). Using the *official codec* for the struct/union
//! layout removes an entire class of hand-XDR bugs (field order, union
//! discriminants, variable-length encoding) that a from-scratch byte-pusher
//! would risk. This module still does the actual *transaction* construction,
//! the signature base, the hash, and the envelope assembly itself — it is not
//! a full high-level "Stellar SDK" standing in for `patala`'s own logic.
//!
//! # Wire shape (classic Stellar, `ENVELOPE_TYPE_TX` / v1)
//!
//! One [`Transaction`] with exactly one `PAYMENT` [`Operation`]:
//! `source_account` (payer) → `destination`, moving `amount` of a
//! [`Asset::CreditAlphanum4`] (issuer + 4-byte code, here `"USDC"`), a
//! [`Memo::Hash`] carrying the `(source, destination, reference)` binding,
//! [`Preconditions::None`] (no time bounds), sequence number = the account's
//! current sequence + 1, and a flat per-operation fee in stroops.
//!
//! **Signing.** Per the Stellar protocol, what gets signed is not the
//! transaction's own XDR — it is
//! `SHA256(networkId || XDR(TransactionSignaturePayload{ network_id,
//! tagged_transaction: Tx(tx) }))`, where `networkId = SHA256(network
//! passphrase)`. That 32-byte hash is also the **transaction hash** Horizon
//! indexes transactions by. [`network_id`] / [`signing_payload`] / [`tx_hash`]
//! implement exactly this, and [`envelope`] wraps the signed transaction with
//! a [`DecoratedSignature`] (a 4-byte "hint" — the last 4 bytes of the
//! signer's public key — plus the raw 64-byte signature).
//!
//! # Money math (`PATALA.md` §6, §8)
//!
//! Every asset amount in classic Stellar XDR is a **fixed-point `int64`
//! scaled by 10^7** — there is no separate "decimals" field per asset the way
//! an SPL mint carries one. [`USDC_DECIMALS`] documents this fact; nothing in
//! this module ever divides, rounds, or touches a float — `amount` is an
//! integer count of ten-millionths, taken directly from
//! `patala_core::PayRequest::amount_minor`.
//!
//! # Honesty
//!
//! This encoder has **not** been run against a live Stellar network from this
//! environment. What *is* checked, offline, in `src/tests.rs`: every value
//! built here round-trips through `stellar_xdr`'s own (spec-generated)
//! decoder byte-for-byte, and a fixed-input known-answer transaction produces
//! a deterministic hash + signature that regressions are pinned against. That
//! is strong evidence the wire format is at least *internally* and
//! *spec-decoder* consistent — it is not the same as confirmation from a real
//! Horizon/testnet submission. See `README.md`.

use sha2::{Digest, Sha256};
use stellar_xdr::curr::{
    AccountId, AlphaNum4, Asset, AssetCode4, DecoratedSignature, Hash, Limits, Memo, MuxedAccount,
    Operation, OperationBody, PaymentOp, Preconditions, PublicKey as XdrPublicKey, ReadXdr,
    SequenceNumber, Signature as XdrSignature, SignatureHint, Transaction, TransactionEnvelope,
    TransactionExt, TransactionSignaturePayload, TransactionSignaturePayloadTaggedTransaction,
    TransactionV1Envelope, Uint256, VecM, WriteXdr,
};

use crate::StellarError;

/// USDC on Stellar has no per-asset decimals field; classic Stellar XDR
/// amounts are always this fixed-point scale (10^7). Documented for callers;
/// nothing in this crate uses it to do arithmetic (there is nothing to scale
/// — `PayRequest::amount_minor` is already in these units).
pub const USDC_DECIMALS: u8 = 7;

fn xdr_pubkey(pk: [u8; 32]) -> XdrPublicKey {
    XdrPublicKey::PublicKeyTypeEd25519(Uint256(pk))
}

fn muxed(pk: [u8; 32]) -> MuxedAccount {
    MuxedAccount::Ed25519(Uint256(pk))
}

/// Pad an asset code (1-4 ASCII alphanumerics, e.g. `"USDC"`) to the 4-byte
/// `AssetCode4` layout. Anything else — empty, too long, non-ASCII — is
/// rejected rather than silently truncated or coerced.
pub fn asset_code4(code: &str) -> Result<AssetCode4, StellarError> {
    if code.is_empty() || code.len() > 4 || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(StellarError::Config(format!(
            "asset code {code:?} must be 1-4 ASCII alphanumerics"
        )));
    }
    let mut bytes = [0u8; 4];
    bytes[..code.len()].copy_from_slice(code.as_bytes());
    Ok(AssetCode4(bytes))
}

/// Build the native Circle USDC [`Asset`] (`PATALA.md` §6): a
/// `CreditAlphanum4` credit asset, `issuer_pk` being the issuing account.
pub fn usdc_asset(code: &str, issuer_pk: [u8; 32]) -> Result<Asset, StellarError> {
    Ok(Asset::CreditAlphanum4(AlphaNum4 {
        asset_code: asset_code4(code)?,
        issuer: AccountId(xdr_pubkey(issuer_pk)),
    }))
}

/// Domain-separated 32-byte binding: `SHA256("patala-stellar-pay-v1" ||
/// rail_id || source || destination || reference)`, each field length-
/// prefixed so no concatenation ambiguity is possible. Carried on-chain as
/// the transaction's [`Memo::Hash`] — a receipt's claimed binding must match
/// both what is re-derived from its own fields *and* what the chain actually
/// carries (`src/lib.rs`'s `verify`).
///
/// ```
/// use patala_stellar::tx::memo_hash;
///
/// // Deterministic in (rail_id, source, destination, reference)...
/// let a = memo_hash("stellar", "GSOURCE", "GDEST", "order-42");
/// assert_eq!(a, memo_hash("stellar", "GSOURCE", "GDEST", "order-42"));
///
/// // ...a different reference never collides (anti-replay)...
/// assert_ne!(a, memo_hash("stellar", "GSOURCE", "GDEST", "order-43"));
///
/// // ...and length-prefixing keeps fields from bleeding into each other:
/// // ("ab","c") and ("a","bc") are distinct bindings, never the same "abc".
/// assert_ne!(
///     memo_hash("stellar", "ab", "c", "r"),
///     memo_hash("stellar", "a", "bc", "r"),
/// );
/// ```
pub fn memo_hash(rail_id: &str, source: &str, destination: &str, reference: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"patala-stellar-pay-v1");
    for field in [rail_id, source, destination, reference] {
        h.update((field.len() as u64).to_le_bytes());
        h.update(field.as_bytes());
    }
    h.finalize().into()
}

/// `networkId = SHA256(network passphrase)` — the Stellar-protocol-defined
/// domain separator between e.g. the public network and testnet, so a
/// signature over a testnet transaction can never be replayed as a mainnet
/// one.
pub fn network_id(passphrase: &str) -> [u8; 32] {
    Sha256::digest(passphrase.as_bytes()).into()
}

/// Build the (unsigned) payment [`Transaction`]: one `PAYMENT` operation,
/// [`Preconditions::None`] (no time bounds), the given sequence number and
/// per-operation fee, and a [`Memo::Hash`] carrying `memo`.
pub fn build_transaction(
    source_pk: [u8; 32],
    dest_pk: [u8; 32],
    asset: Asset,
    amount: i64,
    seq_num: i64,
    fee: u32,
    memo: [u8; 32],
) -> Transaction {
    let op = Operation {
        source_account: None,
        body: OperationBody::Payment(PaymentOp {
            destination: muxed(dest_pk),
            asset,
            amount,
        }),
    };
    Transaction {
        source_account: muxed(source_pk),
        fee,
        seq_num: SequenceNumber(seq_num),
        cond: Preconditions::None,
        memo: Memo::Hash(Hash(memo)),
        operations: VecM::try_from(vec![op]).expect("one operation is well under the 100 limit"),
        ext: TransactionExt::V0,
    }
}

/// The exact bytes that get hashed and signed: `XDR(TransactionSignaturePayload
/// { network_id, tagged_transaction: Tx(tx) })`.
pub fn signing_payload(net_id: [u8; 32], tx: &Transaction) -> Result<Vec<u8>, StellarError> {
    let payload = TransactionSignaturePayload {
        network_id: Hash(net_id),
        tagged_transaction: TransactionSignaturePayloadTaggedTransaction::Tx(tx.clone()),
    };
    payload
        .to_xdr(Limits::none())
        .map_err(|e| StellarError::Xdr(format!("encode signature payload: {e}")))
}

/// `tx_hash = SHA256(signing_payload)` — also the transaction hash Horizon
/// indexes by, and what an Ed25519 signature over this transaction is a
/// signature *of*.
pub fn tx_hash(net_id: [u8; 32], tx: &Transaction) -> Result<[u8; 32], StellarError> {
    Ok(Sha256::digest(signing_payload(net_id, tx)?).into())
}

/// Wrap a signed [`Transaction`] into a `ENVELOPE_TYPE_TX`
/// [`TransactionEnvelope`], with a single [`DecoratedSignature`] — hint =
/// last 4 bytes of the signer's public key, per the Stellar spec.
pub fn envelope(
    tx: Transaction,
    signer_pk: [u8; 32],
    signature: [u8; 64],
) -> Result<TransactionEnvelope, StellarError> {
    let mut hint = [0u8; 4];
    hint.copy_from_slice(&signer_pk[28..32]);
    let sig = DecoratedSignature {
        hint: SignatureHint(hint),
        signature: XdrSignature(
            signature
                .to_vec()
                .try_into()
                .expect("a 64-byte Ed25519 signature always fits Signature's BytesM<64>"),
        ),
    };
    Ok(TransactionEnvelope::Tx(TransactionV1Envelope {
        tx,
        signatures: VecM::try_from(vec![sig])
            .expect("one signature is well under the 20-signature limit"),
    }))
}

/// Base64-encode a [`TransactionEnvelope`] for Horizon's `POST /transactions`
/// `tx` form field.
pub fn envelope_to_xdr_base64(env: &TransactionEnvelope) -> Result<String, StellarError> {
    env.to_xdr_base64(Limits::none())
        .map_err(|e| StellarError::Xdr(format!("encode envelope: {e}")))
}

/// Decode a base64 [`TransactionEnvelope`] — used by `verify` to check what
/// Horizon actually reports against what a receipt claims.
pub fn envelope_from_xdr_base64(b64: &str) -> Result<TransactionEnvelope, StellarError> {
    TransactionEnvelope::from_xdr_base64(b64, Limits::none())
        .map_err(|e| StellarError::Xdr(format!("decode envelope: {e}")))
}

/// Pull the single payment leg + memo hash out of a decoded v1 envelope, for
/// `verify` to compare against a receipt's binding. Rejects (with a
/// descriptive [`StellarError::Xdr`]) anything that is not *exactly* the
/// one-operation, memo-hash shape this crate itself only ever builds:
/// `TxV0`/fee-bump envelopes, zero or multiple operations, a non-`Payment`
/// operation, an operation-level source override, or a non-`Hash` memo.
pub struct DecodedPayment {
    pub source_pk: [u8; 32],
    pub seq_num: i64,
    pub fee: u32,
    pub dest_pk: [u8; 32],
    pub asset: Asset,
    pub amount: i64,
    pub memo: [u8; 32],
}

pub fn decode_single_payment(env: &TransactionEnvelope) -> Result<DecodedPayment, StellarError> {
    let TransactionEnvelope::Tx(v1) = env else {
        return Err(StellarError::Xdr(
            "not a v1 (ENVELOPE_TYPE_TX) envelope".into(),
        ));
    };
    let tx = &v1.tx;
    let MuxedAccount::Ed25519(Uint256(source_pk)) = tx.source_account else {
        return Err(StellarError::Xdr(
            "source account is not a plain Ed25519 key".into(),
        ));
    };
    let Memo::Hash(Hash(memo)) = &tx.memo else {
        return Err(StellarError::Xdr("memo is not MEMO_HASH".into()));
    };
    let ops: &[Operation] = tx.operations.as_ref();
    let [op] = ops else {
        return Err(StellarError::Xdr(format!(
            "expected exactly one operation, found {}",
            ops.len()
        )));
    };
    if op.source_account.is_some() {
        return Err(StellarError::Xdr(
            "operation carries its own source override".into(),
        ));
    }
    let OperationBody::Payment(pay) = &op.body else {
        return Err(StellarError::Xdr("operation is not PAYMENT".into()));
    };
    let MuxedAccount::Ed25519(Uint256(dest_pk)) = pay.destination else {
        return Err(StellarError::Xdr(
            "destination is not a plain Ed25519 key".into(),
        ));
    };
    Ok(DecodedPayment {
        source_pk,
        seq_num: tx.seq_num.0,
        fee: tx.fee,
        dest_pk,
        asset: pay.asset.clone(),
        amount: pay.amount,
        memo: *memo,
    })
}

/// Does `asset` equal the given `CreditAlphanum4(code, issuer_pk)`? (We never
/// accept `Native` XLM or `CreditAlphanum12` — USDC on Stellar is a 4-char
/// code, `PATALA.md` §6.)
pub fn asset_is(asset: &Asset, code: &str, issuer_pk: [u8; 32]) -> bool {
    let Asset::CreditAlphanum4(a) = asset else {
        return false;
    };
    let Ok(want_code) = asset_code4(code) else {
        return false;
    };
    a.asset_code == want_code && a.issuer.0 == xdr_pubkey(issuer_pk)
}

/// Extract the raw signature bytes + signer hint from a decoded v1 envelope's
/// lone signature, for an offline Ed25519 re-check against the claimed
/// signer. Rejects anything other than exactly one signature (this crate
/// never produces more than one).
pub fn single_signature(
    env: &TransactionEnvelope,
) -> Result<(SignatureHint, [u8; 64]), StellarError> {
    let TransactionEnvelope::Tx(v1) = env else {
        return Err(StellarError::Xdr(
            "not a v1 (ENVELOPE_TYPE_TX) envelope".into(),
        ));
    };
    let sigs: &[DecoratedSignature] = v1.signatures.as_ref();
    let [s] = sigs else {
        return Err(StellarError::Xdr(format!(
            "expected exactly one signature, found {}",
            sigs.len()
        )));
    };
    let bytes: [u8; 64] = s.signature[..]
        .try_into()
        .map_err(|_| StellarError::Xdr("signature is not 64 bytes".into()))?;
    Ok((s.hint.clone(), bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_code4_pads_short_codes_and_rejects_bad_ones() {
        assert_eq!(asset_code4("USDC").unwrap().0, *b"USDC");
        assert_eq!(asset_code4("A").unwrap().0, [b'A', 0, 0, 0]);
        assert!(asset_code4("").is_err());
        assert!(asset_code4("TOOLONG").is_err());
        assert!(asset_code4("US-C").is_err(), "non-alphanumeric rejected");
    }

    #[test]
    fn memo_hash_is_deterministic_and_domain_separated() {
        let a = memo_hash("stellar", "G_SRC", "G_DST", "order-1");
        let b = memo_hash("stellar", "G_SRC", "G_DST", "order-1");
        assert_eq!(a, b);
        let c = memo_hash("stellar", "G_SRC", "G_DST", "order-2");
        assert_ne!(a, c, "different reference must bind to a different memo");
        // Concatenation ambiguity: ("ab","c") vs ("a","bc") must not collide
        // because every field is length-prefixed.
        let x = memo_hash("ab", "c", "d", "e");
        let y = memo_hash("a", "bc", "d", "e");
        assert_ne!(x, y);
    }

    #[test]
    fn network_id_is_sha256_of_the_passphrase() {
        let want: [u8; 32] = Sha256::digest(b"Test SDF Network ; September 2015").into();
        assert_eq!(network_id("Test SDF Network ; September 2015"), want);
    }
}
