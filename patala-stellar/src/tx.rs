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
//! One [`Transaction`] with **1 to [`MAX_OPERATIONS`] `PAYMENT`
//! [`Operation`]s**, all paid by the transaction's own `source_account` (the
//! payer): each one moves `amount` of an [`Asset::CreditAlphanum4`] (issuer +
//! 4-byte code, here `"USDC"`) to one `destination`. Plus a [`Memo::Hash`]
//! carrying the binding (`(source, destination, reference)` for a single
//! payment — see [`memo_hash`]; `(source, reference, every leg in order)` for
//! a split — see [`split_memo_hash`]), [`Preconditions::None`] (no time
//! bounds), sequence number = the account's current sequence + 1, and a fee
//! bid of `base_fee_stroops × operation_count`.
//!
//! # Why N operations (`docs/shared-economics.md` §5)
//!
//! A Stellar transaction is **atomic**: either every operation applies or none
//! does. That is what makes an N-way split a single settlement event rather
//! than N payments that can partly fail — the property `patala_core`'s
//! single-recipient seam cannot express, so it lives here, beneath it, once,
//! instead of in each consumer.
//!
//! Two consequences that callers must plan for, both stated rather than
//! papered over:
//!
//! * **One unprepared payee kills the whole bundle.** A `PAYMENT` to an
//!   account with no trustline for the asset fails (`op_no_trust`), and
//!   because operations are atomic that failure takes every other leg with it.
//!   This is a griefing vector when payees are not all under the payer's
//!   control. A `check_trustlines` pre-flight would be the fix (not yet
//!   implemented); [`CLAIMABLE_BALANCE_RESERVE_STROOPS`] documents the
//!   fallback and why it is not the default.
//! * **The fee scales with the operation count.** stellar-core requires
//!   `fee ≥ base_fee × operation_count`, so [`build_payment_transaction`]
//!   multiplies (see [`total_fee`]) instead of reusing a single-operation bid
//!   that would be rejected as `txINSUFFICIENT_FEE`.
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

/// The most operations one classic Stellar transaction may carry — a protocol
/// limit, not a choice made here: `Transaction::operations` is
/// `VecM<Operation, 100>` in the XDR definition itself, so a 101st operation
/// cannot even be encoded. Every builder in this module refuses to exceed it
/// with a named error rather than letting `VecM::try_from` fail anonymously.
pub const MAX_OPERATIONS: usize = 100;

/// What a `CREATE_CLAIMABLE_BALANCE` operation locks up, per leg, in stroops
/// (0.5 XLM) — **documented, not used**: this crate builds no claimable
/// balances. See the `check_trustlines` note above (not yet implemented) for
/// why a caller might want one, and the `Trustlines` section of `crate`'s
/// docs for exactly what implementing it would take.
///
/// This is *today's* value of a **network parameter** — the ledger's base
/// reserve, currently 0.5 XLM, which is what one claimable balance entry with
/// one claimant costs its sponsor (more claimants cost more). The authoritative
/// value lives in the ledger header, so a constant here would be wrong the day
/// the network votes it up: treat it as an order of magnitude for capacity
/// planning, never as an amount to debit anybody.
pub const CLAIMABLE_BALANCE_RESERVE_STROOPS: i64 = 5_000_000;

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

/// Domain-separated 32-byte binding for an **atomic split**: `SHA256(
/// "patala-stellar-split-v1" || rail_id || source || reference || leg_count ||
/// (destination, amount) for every leg, in order)`, every field
/// length-prefixed and every amount an 8-byte little-endian `i64`. Carried
/// on-chain as the transaction's [`Memo::Hash`], exactly as [`memo_hash`] is for
/// a single payment.
///
/// **Every leg is inside the hash, and so is their order.** That is what makes
/// the 32 bytes Stellar lets a transaction carry sufficient to bind an N-way
/// split: a receipt cannot have a leg added, removed, re-pointed, re-priced or
/// re-ordered without the memo — and therefore the transaction hash, and
/// therefore the signature — ceasing to match.
///
/// The domain string differs from [`memo_hash`]'s, so a single-payment binding
/// and a one-leg split binding can never be confused for one another even
/// though they move identical money.
///
/// ```
/// use patala_stellar::tx::{memo_hash, split_memo_hash};
///
/// let legs = [("GDEV", 700i64), ("GOPS", 300i64)];
/// let a = split_memo_hash("stellar", "GSOURCE", "order-42", &legs);
/// assert_eq!(a, split_memo_hash("stellar", "GSOURCE", "order-42", &legs));
///
/// // Order is bound: the same legs paid in the other order is a different split.
/// let swapped = [("GOPS", 300i64), ("GDEV", 700i64)];
/// assert_ne!(a, split_memo_hash("stellar", "GSOURCE", "order-42", &swapped));
///
/// // So is every amount, even when the total is unchanged (699 + 301 = 1000).
/// let reweighted = [("GDEV", 699i64), ("GOPS", 301i64)];
/// assert_ne!(a, split_memo_hash("stellar", "GSOURCE", "order-42", &reweighted));
///
/// // A one-leg split is never the same binding as the single payment it mirrors.
/// let one = [("GDEV", 1000i64)];
/// assert_ne!(
///     split_memo_hash("stellar", "GSOURCE", "order-42", &one),
///     memo_hash("stellar", "GSOURCE", "GDEV", "order-42"),
/// );
/// ```
pub fn split_memo_hash(
    rail_id: &str,
    source: &str,
    reference: &str,
    legs: &[(&str, i64)],
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"patala-stellar-split-v1");
    for field in [rail_id, source, reference] {
        h.update((field.len() as u64).to_le_bytes());
        h.update(field.as_bytes());
    }
    h.update((legs.len() as u64).to_le_bytes());
    for (destination, amount) in legs {
        h.update((destination.len() as u64).to_le_bytes());
        h.update(destination.as_bytes());
        h.update(amount.to_le_bytes());
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

/// One leg of a payment transaction: **one payee, one asset, one integer
/// amount**. N of these in one [`Transaction`] is what makes an atomic split
/// (`docs/shared-economics.md` §5).
///
/// The asset is *per leg*, not per transaction: that is what lets one atomic
/// bundle pay one payee in USDC and another in EURC, with no conversion and no
/// market involved. (Converting between assets inside the same transaction is
/// what `PATH_PAYMENT_STRICT_RECEIVE` would add, and this crate deliberately
/// does not build one — see the `PathPaymentStrictReceive` section of `crate`'s
/// docs.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentLeg {
    /// The payee's raw Ed25519 public key (a `G…` StrKey address decoded).
    pub dest_pk: [u8; 32],
    /// The asset this leg moves.
    pub asset: Asset,
    /// The amount, in the asset's own fixed-point `int64` units (10^-7). Never
    /// a float; must be strictly positive.
    pub amount: i64,
}

impl PaymentLeg {
    /// A leg paying `amount` of `asset` to `dest_pk`.
    pub fn new(dest_pk: [u8; 32], asset: Asset, amount: i64) -> Self {
        Self {
            dest_pk,
            asset,
            amount,
        }
    }
}

/// The `PAYMENT` [`Operation`] for one leg, with **no operation-level source
/// override** — the transaction's own source account pays every leg, which is
/// the shape [`decode_payments`] is the only accepted shape of.
///
/// Unvalidated on purpose (see [`build_payment_transaction`] for the checked
/// entry point): rebuilding a transaction byte-for-byte from an untrusted
/// receipt — which is what `crate::StellarRail::verify` does offline — has to be
/// able to encode whatever a tampered proof claimed, so that the *hash* is what
/// disagrees. A builder that rejected a hostile value here would have to panic
/// or complicate every call site instead.
fn payment_op(leg: &PaymentLeg) -> Operation {
    Operation {
        source_account: None,
        body: OperationBody::Payment(PaymentOp {
            destination: muxed(leg.dest_pk),
            asset: leg.asset.clone(),
            amount: leg.amount,
        }),
    }
}

/// Assemble a [`Transaction`] from already-built operations: the payer's
/// account as the source, [`Preconditions::None`] (no time bounds — see
/// `crate`'s docs on offline signing), the given sequence number, the given
/// **total** fee bid, and a [`Memo::Hash`] carrying `memo`.
fn transaction_from_ops(
    source_pk: [u8; 32],
    ops: Vec<Operation>,
    seq_num: i64,
    fee: u32,
    memo: [u8; 32],
) -> Result<Transaction, StellarError> {
    let operations = VecM::try_from(ops).map_err(|_| {
        StellarError::Xdr(format!(
            "a stellar transaction carries at most {MAX_OPERATIONS} operations"
        ))
    })?;
    Ok(Transaction {
        source_account: muxed(source_pk),
        fee,
        seq_num: SequenceNumber(seq_num),
        cond: Preconditions::None,
        memo: Memo::Hash(Hash(memo)),
        operations,
        ext: TransactionExt::V0,
    })
}

/// The exact sum of every leg's amount, in stroops — **integer only**, and
/// `checked_add` at every step so an overflowing bundle is a refusal instead of
/// a wrapped-around amount. Callers compare this against the total they believe
/// they are paying; `crate::SplitRequest::validate` makes that comparison
/// mandatory.
pub fn total_amount(legs: &[PaymentLeg]) -> Result<i64, StellarError> {
    let mut total: i64 = 0;
    for (i, leg) in legs.iter().enumerate() {
        total = total.checked_add(leg.amount).ok_or_else(|| {
            StellarError::Config(format!(
                "leg {i}: summing payment amounts overflows Stellar's i64 amount range"
            ))
        })?;
    }
    Ok(total)
}

/// The fee bid for `op_count` operations at `base_fee_stroops` each.
///
/// stellar-core charges `base_fee × operation_count` and rejects a transaction
/// bidding less as `txINSUFFICIENT_FEE`, so a multi-operation transaction that
/// reused a single-operation bid would simply never be included. Integer
/// `checked_mul`: an overflowing bid is a refusal, never a silently truncated
/// one.
pub fn total_fee(base_fee_stroops: u32, op_count: usize) -> Result<u32, StellarError> {
    let ops = u32::try_from(op_count)
        .map_err(|_| StellarError::Config(format!("absurd operation count {op_count}")))?;
    base_fee_stroops.checked_mul(ops).ok_or_else(|| {
        StellarError::Config(format!(
            "fee bid overflows u32: {base_fee_stroops} stroops × {ops} operations"
        ))
    })
}

/// Build the (unsigned) payment [`Transaction`] for **1 to [`MAX_OPERATIONS`]
/// legs**, atomically: [`Preconditions::None`] (no time bounds), the given
/// sequence number, a fee bid of `base_fee_stroops × legs.len()` (see
/// [`total_fee`]), and a [`Memo::Hash`] carrying `memo`.
///
/// This is the general constructor; [`build_transaction`] is the
/// single-recipient special case of it, and both produce byte-identical XDR for
/// one leg.
///
/// Refused, rather than built and left to fail on-chain:
///
/// * **no legs** — a transaction that moves nothing still burns a sequence
///   number and a fee;
/// * **more than [`MAX_OPERATIONS`] legs** — unencodable;
/// * **a non-positive amount on any leg** — stellar-core rejects a `PAYMENT` of
///   `amount <= 0`, so one such leg would take the whole atomic bundle down
///   with it;
/// * **a fee bid that overflows `u32`**.
///
/// The error names the offending leg's index, because with 100 legs "invalid
/// amount" is not an actionable message.
pub fn build_payment_transaction(
    source_pk: [u8; 32],
    legs: &[PaymentLeg],
    seq_num: i64,
    base_fee_stroops: u32,
    memo: [u8; 32],
) -> Result<Transaction, StellarError> {
    if legs.is_empty() {
        return Err(StellarError::Config(
            "a payment transaction needs at least one leg".into(),
        ));
    }
    if legs.len() > MAX_OPERATIONS {
        return Err(StellarError::Config(format!(
            "{} legs exceeds Stellar's limit of {MAX_OPERATIONS} operations per transaction; \
             split the bundle across several transactions (each is then separately atomic — \
             atomicity does NOT extend across them)",
            legs.len()
        )));
    }
    for (i, leg) in legs.iter().enumerate() {
        if leg.amount <= 0 {
            return Err(StellarError::Config(format!(
                "leg {i}: payment amount must be strictly positive, got {}",
                leg.amount
            )));
        }
    }
    // Computed for its overflow check: a bundle whose legs sum past i64 can
    // never settle, and finding that out here is cheaper than finding it out
    // from Horizon.
    total_amount(legs)?;

    let fee = total_fee(base_fee_stroops, legs.len())?;
    let ops: Vec<Operation> = legs.iter().map(payment_op).collect();
    transaction_from_ops(source_pk, ops, seq_num, fee, memo)
}

/// Build the (unsigned) single-payment [`Transaction`] — one `PAYMENT`
/// operation, [`Preconditions::None`] (no time bounds), the given sequence
/// number and per-operation fee, and a [`Memo::Hash`] carrying `memo`.
///
/// Kept infallible and unvalidated: `crate::StellarRail::verify` rebuilds a
/// transaction from an **untrusted** receipt's own scalar fields in order to
/// re-derive its hash, so this has to be able to encode a hostile value (a zero
/// amount, say) and let the hash be what disagrees. See `payment_op` (private
/// to this module).
/// [`build_payment_transaction`] is the validating, N-leg generalisation, and
/// for one leg the two produce byte-identical XDR.
pub fn build_transaction(
    source_pk: [u8; 32],
    dest_pk: [u8; 32],
    asset: Asset,
    amount: i64,
    seq_num: i64,
    fee: u32,
    memo: [u8; 32],
) -> Transaction {
    let leg = PaymentLeg::new(dest_pk, asset, amount);
    transaction_from_ops(source_pk, vec![payment_op(&leg)], seq_num, fee, memo)
        .expect("one operation is well under the 100 limit")
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

/// One payment leg pulled back out of a decoded envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedLeg {
    /// The payee's raw Ed25519 public key.
    pub dest_pk: [u8; 32],
    /// The asset this leg moves.
    pub asset: Asset,
    /// The amount, in the asset's fixed-point `int64` units.
    pub amount: i64,
}

/// Every payment leg plus the transaction-level fields, decoded out of a v1
/// envelope for `crate::StellarRail::verify` to compare against a receipt's
/// binding — see [`decode_payments`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedPayments {
    /// The payer — the transaction's own source account.
    pub source_pk: [u8; 32],
    /// The sequence number consumed.
    pub seq_num: i64,
    /// The **total** fee bid carried on the transaction (all legs together).
    pub fee: u32,
    /// The `MEMO_HASH` bytes.
    pub memo: [u8; 32],
    /// The legs, **in transaction order** — which is load-bearing, because
    /// [`split_memo_hash`] binds that order.
    pub legs: Vec<DecodedLeg>,
}

/// Pull **every** payment leg + the memo hash out of a decoded v1 envelope.
///
/// Rejects (with a descriptive [`StellarError::Xdr`]) anything that is not the
/// 1-to-[`MAX_OPERATIONS`]-operation, memo-hash shape this crate itself only
/// ever builds: `TxV0`/fee-bump envelopes, zero operations, more than
/// [`MAX_OPERATIONS`], a non-`Payment` operation *anywhere* in the bundle, an
/// operation-level source override on *any* leg, a non-Ed25519 destination on
/// *any* leg, a non-positive amount on *any* leg, or a non-`Hash` memo.
///
/// **Every leg is checked, and one bad leg rejects the whole envelope** — which
/// is the only correct reading of an atomic transaction: there is no such thing
/// as a partly-acceptable bundle. Errors name the leg index, because with up to
/// 100 legs "operation is not PAYMENT" is not an actionable message.
pub fn decode_payments(env: &TransactionEnvelope) -> Result<DecodedPayments, StellarError> {
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
    if ops.is_empty() {
        return Err(StellarError::Xdr(
            "transaction carries no operations at all".into(),
        ));
    }
    // Belt and braces: the XDR type is `VecM<Operation, 100>`, so a decoder
    // that honoured the definition cannot hand us more than this. Checked
    // anyway rather than assumed, because everything downstream indexes by leg.
    if ops.len() > MAX_OPERATIONS {
        return Err(StellarError::Xdr(format!(
            "{} operations exceeds the protocol limit of {MAX_OPERATIONS}",
            ops.len()
        )));
    }

    let mut legs = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        if op.source_account.is_some() {
            return Err(StellarError::Xdr(format!(
                "operation {i} carries its own source override"
            )));
        }
        let OperationBody::Payment(pay) = &op.body else {
            return Err(StellarError::Xdr(format!("operation {i} is not PAYMENT")));
        };
        let MuxedAccount::Ed25519(Uint256(dest_pk)) = pay.destination else {
            return Err(StellarError::Xdr(format!(
                "operation {i}: destination is not a plain Ed25519 key"
            )));
        };
        if pay.amount <= 0 {
            return Err(StellarError::Xdr(format!(
                "operation {i}: payment amount {} is not strictly positive",
                pay.amount
            )));
        }
        legs.push(DecodedLeg {
            dest_pk,
            asset: pay.asset.clone(),
            amount: pay.amount,
        });
    }

    Ok(DecodedPayments {
        source_pk,
        seq_num: tx.seq_num.0,
        fee: tx.fee,
        memo: *memo,
        legs,
    })
}

/// The single payment leg + memo hash of a decoded v1 envelope — the
/// one-recipient view, kept exactly as strict as it has always been.
///
/// This is [`decode_payments`] plus "and there is exactly one leg": an envelope
/// carrying zero or several payments is still refused here, so no caller written
/// against the single-recipient shape silently starts accepting a bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    let decoded = decode_payments(env)?;
    let [leg] = decoded.legs.as_slice() else {
        return Err(StellarError::Xdr(format!(
            "expected exactly one operation, found {}",
            decoded.legs.len()
        )));
    };
    Ok(DecodedPayment {
        source_pk: decoded.source_pk,
        seq_num: decoded.seq_num,
        fee: decoded.fee,
        dest_pk: leg.dest_pk,
        asset: leg.asset.clone(),
        amount: leg.amount,
        memo: decoded.memo,
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

    fn leg(dest: u8, amount: i64) -> PaymentLeg {
        PaymentLeg::new(
            [dest; 32],
            usdc_asset("USDC", [42u8; 32]).expect("a fixed valid asset"),
            amount,
        )
    }

    #[test]
    fn one_leg_through_the_multi_builder_is_byte_identical_to_the_single_builder() {
        // The generalisation must not be a second, subtly different encoder:
        // the single-recipient path is the N=1 case of the same one.
        let asset = usdc_asset("USDC", [42u8; 32]).unwrap();
        let memo = memo_hash("stellar", "GSRC", "GDST", "r-1");
        let single = build_transaction([1u8; 32], [9u8; 32], asset.clone(), 5_000, 77, 100, memo);
        let multi = build_payment_transaction(
            [1u8; 32],
            &[PaymentLeg::new([9u8; 32], asset, 5_000)],
            77,
            100,
            memo,
        )
        .unwrap();
        assert_eq!(single, multi);
        assert_eq!(
            single.to_xdr(Limits::none()).unwrap(),
            multi.to_xdr(Limits::none()).unwrap()
        );
    }

    #[test]
    fn the_fee_bid_scales_with_the_operation_count() {
        // stellar-core charges base_fee × op_count and rejects less as
        // txINSUFFICIENT_FEE, so a bundle that reused a one-op bid would never
        // be included.
        let legs: Vec<PaymentLeg> = (0..7).map(|i| leg(i, 10)).collect();
        let tx = build_payment_transaction([1u8; 32], &legs, 5, 100, [0u8; 32]).unwrap();
        assert_eq!(tx.fee, 700);
        assert_eq!(tx.operations.len(), 7);
        assert_eq!(total_fee(100, 7).unwrap(), 700);
        // Integer only, and an overflowing bid is a refusal not a wrap.
        assert!(total_fee(u32::MAX, 2).is_err());
        assert_eq!(total_fee(0, MAX_OPERATIONS).unwrap(), 0);
    }

    #[test]
    fn the_builder_accepts_exactly_the_protocol_limit_and_refuses_one_more() {
        let ok: Vec<PaymentLeg> = (0..MAX_OPERATIONS).map(|i| leg(i as u8, 1)).collect();
        let tx = build_payment_transaction([1u8; 32], &ok, 1, 100, [0u8; 32]).unwrap();
        assert_eq!(tx.operations.len() as usize, MAX_OPERATIONS);

        let too_many: Vec<PaymentLeg> = (0..=MAX_OPERATIONS).map(|i| leg(i as u8, 1)).collect();
        let err = build_payment_transaction([1u8; 32], &too_many, 1, 100, [0u8; 32])
            .expect_err("101 operations cannot even be encoded");
        assert!(format!("{err}").contains("101 legs exceeds"), "{err}");
        // And the message must say atomicity does not span transactions.
        assert!(format!("{err}").contains("does NOT extend"), "{err}");
    }

    #[test]
    fn the_builder_refuses_an_empty_bundle_and_a_non_positive_leg() {
        let err = build_payment_transaction([1u8; 32], &[], 1, 100, [0u8; 32]).unwrap_err();
        assert!(format!("{err}").contains("at least one leg"), "{err}");

        for bad in [0i64, -1, i64::MIN] {
            let legs = [leg(1, 10), leg(2, bad), leg(3, 10)];
            let err = build_payment_transaction([1u8; 32], &legs, 1, 100, [0u8; 32])
                .expect_err("one unpayable leg must take the whole bundle down here, not on-chain");
            // Names WHICH leg — with 100 legs, "invalid amount" is useless.
            assert!(format!("{err}").contains("leg 1:"), "{err}");
        }
    }

    #[test]
    fn summing_legs_is_integer_only_and_refuses_to_overflow() {
        assert_eq!(total_amount(&[]).unwrap(), 0);
        assert_eq!(total_amount(&[leg(1, 700), leg(2, 300)]).unwrap(), 1_000);
        let err = total_amount(&[leg(1, i64::MAX), leg(2, 1)]).unwrap_err();
        assert!(format!("{err}").contains("overflow"), "{err}");
        // ...and the builder refuses such a bundle rather than encoding it.
        assert!(build_payment_transaction(
            [1u8; 32],
            &[leg(1, i64::MAX), leg(2, 1)],
            1,
            100,
            [0u8; 32]
        )
        .is_err());
    }

    #[test]
    fn split_memo_binds_every_leg_its_amount_and_their_order() {
        let base = split_memo_hash("stellar", "GSRC", "r-1", &[("GA", 700), ("GB", 300)]);
        assert_eq!(
            base,
            split_memo_hash("stellar", "GSRC", "r-1", &[("GA", 700), ("GB", 300)])
        );
        // Re-pointed leg, re-priced leg, re-ordered legs, dropped leg, added
        // leg, different reference, different payer: all different bindings.
        for other in [
            split_memo_hash("stellar", "GSRC", "r-1", &[("GA", 700), ("GC", 300)]),
            split_memo_hash("stellar", "GSRC", "r-1", &[("GA", 699), ("GB", 301)]),
            split_memo_hash("stellar", "GSRC", "r-1", &[("GB", 300), ("GA", 700)]),
            split_memo_hash("stellar", "GSRC", "r-1", &[("GA", 700)]),
            split_memo_hash(
                "stellar",
                "GSRC",
                "r-1",
                &[("GA", 700), ("GB", 300), ("GC", 1)],
            ),
            split_memo_hash("stellar", "GSRC", "r-2", &[("GA", 700), ("GB", 300)]),
            split_memo_hash("stellar", "GOTHER", "r-1", &[("GA", 700), ("GB", 300)]),
        ] {
            assert_ne!(base, other);
        }
        // Length prefixing: ("AB","C") and ("A","BC") must not collide.
        assert_ne!(
            split_memo_hash("stellar", "GSRC", "r", &[("AB", 1), ("C", 1)]),
            split_memo_hash("stellar", "GSRC", "r", &[("A", 1), ("BC", 1)]),
        );
        // Domain separation from the single-payment binding.
        assert_ne!(
            split_memo_hash("stellar", "GSRC", "r-1", &[("GA", 700)]),
            memo_hash("stellar", "GSRC", "GA", "r-1"),
        );
    }

    #[test]
    fn decoding_rejects_a_bundle_with_one_bad_leg_and_names_it() {
        // Hand-assemble what this crate would never build: leg 1 carries its
        // own source override, and separately, leg 1 is not a PAYMENT at all.
        let asset = usdc_asset("USDC", [42u8; 32]).unwrap();
        let good = payment_op(&PaymentLeg::new([9u8; 32], asset.clone(), 10));

        let mut overridden = good.clone();
        overridden.source_account = Some(muxed([5u8; 32]));
        let tx = transaction_from_ops([1u8; 32], vec![good.clone(), overridden], 1, 200, [0u8; 32])
            .unwrap();
        let env = envelope(tx, [1u8; 32], [0u8; 64]).unwrap();
        let err = decode_payments(&env).unwrap_err();
        assert!(format!("{err}").contains("operation 1"), "{err}");

        let not_payment = Operation {
            source_account: None,
            body: OperationBody::Inflation,
        };
        let tx =
            transaction_from_ops([1u8; 32], vec![good, not_payment], 1, 200, [0u8; 32]).unwrap();
        let env = envelope(tx, [1u8; 32], [0u8; 64]).unwrap();
        let err = decode_payments(&env).unwrap_err();
        assert!(
            format!("{err}").contains("operation 1 is not PAYMENT"),
            "{err}"
        );
    }

    #[test]
    fn the_single_recipient_decoder_still_refuses_a_bundle() {
        // The preserved invariant: a caller written against the one-payment
        // shape must not silently start accepting N payments.
        let asset = usdc_asset("USDC", [42u8; 32]).unwrap();
        let legs = [
            PaymentLeg::new([9u8; 32], asset.clone(), 10),
            PaymentLeg::new([8u8; 32], asset, 20),
        ];
        let tx = build_payment_transaction([1u8; 32], &legs, 1, 100, [0u8; 32]).unwrap();
        let env = envelope(tx, [1u8; 32], [0u8; 64]).unwrap();

        let err = decode_single_payment(&env).unwrap_err();
        assert!(
            format!("{err}").contains("expected exactly one operation, found 2"),
            "{err}"
        );
        // ...while the N-leg decoder reads it in order.
        let decoded = decode_payments(&env).unwrap();
        assert_eq!(decoded.legs.len(), 2);
        assert_eq!(decoded.legs[0].amount, 10);
        assert_eq!(decoded.legs[1].dest_pk, [8u8; 32]);
        assert_eq!(decoded.fee, 200, "total fee, both legs");
    }

    #[test]
    fn a_bundle_may_pay_each_leg_in_a_different_asset() {
        // Per-leg asset choice, atomically, with no conversion and no market —
        // the reason PATH_PAYMENT_STRICT_RECEIVE is not needed for this.
        let usdc = usdc_asset("USDC", [42u8; 32]).unwrap();
        let eurc = usdc_asset("EURC", [43u8; 32]).unwrap();
        let legs = [
            PaymentLeg::new([9u8; 32], usdc.clone(), 700),
            PaymentLeg::new([8u8; 32], eurc.clone(), 300),
        ];
        let tx = build_payment_transaction([1u8; 32], &legs, 1, 100, [0u8; 32]).unwrap();
        let env = envelope(tx, [1u8; 32], [0u8; 64]).unwrap();
        let decoded = decode_payments(&env).unwrap();
        assert!(asset_is(&decoded.legs[0].asset, "USDC", [42u8; 32]));
        assert!(asset_is(&decoded.legs[1].asset, "EURC", [43u8; 32]));
        assert!(!asset_is(&decoded.legs[1].asset, "USDC", [42u8; 32]));
    }

    #[test]
    fn network_id_is_sha256_of_the_passphrase() {
        let want: [u8; 32] = Sha256::digest(b"Test SDF Network ; September 2015").into();
        assert_eq!(network_id("Test SDF Network ; September 2015"), want);
    }
}
