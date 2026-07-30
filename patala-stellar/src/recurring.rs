//! B4 — the recurring primitive (`docs/shared-economics.md`, backlog B4):
//! **N pre-signed, time-bounded transactions on a dedicated source
//! account**, non-custodial, no contract, cancellable.
//!
//! # Feasibility — verified against the pinned `stellar-xdr`, not assumed
//!
//! `patala-stellar/Cargo.toml` pins `stellar-xdr = "22"`, resolved by
//! `Cargo.lock` to `22.2.0`. Read directly from that crate's own generated
//! source
//! (`~/.cargo/registry/src/index.crates.io-*/stellar-xdr-22.2.0/src/curr/generated.rs`,
//! `struct PreconditionsV2` around line 31792):
//!
//! ```text
//! pub struct PreconditionsV2 {
//!     pub time_bounds: Option<TimeBounds>,
//!     pub ledger_bounds: Option<LedgerBounds>,
//!     pub min_seq_num: Option<SequenceNumber>,
//!     pub min_seq_age: Duration,
//!     pub min_seq_ledger_gap: u32,
//!     pub extra_signers: VecM<SignerKey, 2>,
//! }
//! ```
//!
//! All three of B4's named fields — `min_seq_num`, `min_seq_age`,
//! `min_seq_ledger_gap` — exist, in the `curr` module this crate already
//! imports from (`stellar_xdr::curr`), not gated behind a feature this
//! crate lacks. **B4 is buildable on the pinned XDR version.**
//! `tx::tests::preconditions_v2_is_actually_available_on_the_pinned_xdr_version`
//! pins this as a running test (constructs one, round-trips it through the
//! SDF's own spec-generated decoder byte-for-byte), not just a doc claim.
//!
//! # The mechanism, and the honest reading of "relax"
//!
//! The XDR doc comment carried into `generated.rs` (reproducing the same
//! `.x` source stellar-core itself is generated from) states the validity
//! rule exactly:
//!
//! ```text
//! // If NULL, only valid when sourceAccount's sequence number
//! // is seqNum - 1.  Otherwise, valid when sourceAccount's
//! // sequence number n satisfies minSeqNum <= n < tx.seqNum.
//! ...
//! // For the transaction to be valid, the current ledger time must
//! // be at least minSeqAge greater than sourceAccount's seqTime.
//! ...
//! // For the transaction to be valid, the current ledger number
//! // must be at least minSeqLedgerGap greater than sourceAccount's
//! // seqLedger.
//! ```
//!
//! `min_seq_age`/`min_seq_ledger_gap` exist **only** inside
//! `PreconditionsV2` — `Preconditions::None` and `Preconditions::Time` (an
//! absolute calendar window) have no field that can express "at least this
//! much real time must pass since this account's sequence last changed".
//! That relative gate is the genuinely new capability this primitive needs,
//! and it is real.
//!
//! **`min_seq_num` is deliberately left `None` in this build — no ordering
//! relaxation is claimed.** A loose, shared `min_seq_num` (every instalment
//! sharing the plan's original sequence number, so a later instalment stays
//! valid even if an earlier one is skipped) is what the doc comment's
//! `minSeqNum <= n < tx.seqNum` window literally allows, and it is a real,
//! documented use of the field. It was modelled and **not** built here: the
//! same wide window that tolerates a skipped instalment also lets whoever
//! holds all the pre-signed envelopes jump straight to a much later
//! instalment (burning every instalment in between — the source account's
//! sequence advances past their `seqNum`, invalidating them) as soon as
//! only ONE spacing period has elapsed rather than all of them, because
//! `min_seq_age` is measured relative to the source account's own
//! `seqTime`, which resets to "now" every time ANY instalment executes.
//! Getting real per-hop temporal spacing without also opening that
//! cherry-pick/burn path needs more careful modelling (and more elaborate
//! test infrastructure than an in-memory validity oracle) than this pass
//! is doing unilaterally. **What ships here instead**: `min_seq_num: None`
//! (identical, by the spec text above, to the plain "must be exactly
//! `seqNum - 1`" rule every ordinary Stellar transaction already has — a
//! strict, in-order chain, no skipping, no reordering, no forgiveness for a
//! missed instalment), plus a `min_seq_age`/`min_seq_ledger_gap` floor that
//! is constant across every instalment after the first. That constant floor
//! is what makes "N pre-signed transactions cannot all be redeemed back to
//! back" literally true here: instalment *i* cannot be submitted until the
//! source account's sequence is *exactly* what instalment *i − 1* left it
//! at (ordinary Stellar sequencing — unchanged), **and** at least
//! `min_seq_age_step` seconds / `min_seq_ledger_gap_step` ledgers have
//! passed since instalment *i − 1* actually executed (the one thing
//! ordinary sequencing cannot express at all). [`would_be_valid`] is a pure,
//! offline reimplementation of the spec rule above, used to prove exactly
//! this property by simulation (see its own docs for what it is not
//! evidence of).
//!
//! # Cancellation — mechanically, not by wishful naming
//!
//! Stellar has no message-recall: once a transaction is signed, the
//! signature exists and nothing un-signs it. "Cancellable" here means
//! [`RecurringPlan::build_cancel_transaction`] — one ordinary, normally
//! sequenced `BUMP_SEQUENCE` operation
//! ([`tx::build_bump_sequence_transaction`]) that raises the **dedicated**
//! source account's on-chain sequence number past every remaining
//! instalment's own `seqNum`. Because every instalment's validity rule is
//! `n < tx.seqNum` (see above), once the account's sequence `n` is bumped
//! at or beyond the highest outstanding instalment's `seqNum`, **every**
//! not-yet-executed instalment becomes permanently unsatisfiable in a
//! single on-chain step — no per-instalment revocation needed, no
//! contract, and the payer never gives up custody of the signed envelopes
//! (there is nothing to "give up"; they simply become worthless). This does
//! **not** undo an instalment that already executed — Stellar payments do
//! not reverse (see `crate`'s existing `refund` docs for the identical
//! point about one-off payments).
//!
//! # What this module does NOT do
//!
//! - **Not wired into `PaymentRail`/`StellarRail::charge`/`verify`.** Those
//!   hard-code `Preconditions::None` end to end, and `verify`'s offline step
//!   rebuilds a transaction from a `StellarBinding`'s scalar fields via
//!   `tx::build_transaction` to re-derive the signed hash. Threading
//!   preconditions through that struct and function would change what
//!   `verify` signs/rebuilds for every *existing* receipt shape — exactly
//!   the risk this backlog item's own brief called out. Nothing in `tx.rs`'s
//!   existing public surface (`build_transaction`, `StellarBinding`,
//!   `decode_payments`, `StellarRail::charge`/`verify`) was changed in
//!   behaviour; `tx::tests::build_transaction_is_the_preconditions_none_case_of_the_generalised_builder`
//!   and the pre-existing KAT regression test both still pass unmodified.
//!   This module has its own binding type and its own offline verify
//!   function instead.
//! - **Not submitted anywhere.** Building and signing N instalments (and
//!   the cancel transaction) is entirely offline. Submitting one, when it
//!   is due, is the caller's job via the already-existing
//!   `StellarRpc::submit_transaction`/`get_transaction` (unchanged) —
//!   exactly the seam `StellarRail::charge` already uses for a one-off
//!   payment.
//! - **No schedule has ever executed, on any network.** Everything in this
//!   module is offline-tested only (the `patala-stellar` README/`src/lib.rs`
//!   honesty section's "one payment settled on testnet" claim is about B7's
//!   single-leg payment; it says nothing about recurring, and this module
//!   does not change that).

use stellar_xdr::curr::{Asset, Duration, Preconditions, PreconditionsV2, Transaction};

use crate::tx;
use crate::StellarError;

/// A conservative sanity cap on how many instalments one plan may hold —
/// **not a Stellar protocol limit** (there is none for this: each
/// instalment is an independent, separately-signed transaction, unlike
/// `tx::MAX_OPERATIONS`, which really is a protocol limit on operations
/// *within one* transaction). This exists only to reject an obviously
/// absurd input (e.g. a typo of `36500` meaning daily-for-a-century)
/// before doing real work.
pub const MAX_INSTALMENTS: u32 = 3650;

/// One fully-specified B4 recurring schedule: N instalments of the same
/// amount, in the same asset, from one dedicated source account to one
/// destination, offline-buildable and -signable in full before the first
/// instalment ever executes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecurringPlan {
    /// The payer's dedicated source account — see the module docs on why
    /// "dedicated" (never used for anything else) is load-bearing for the
    /// `min_seq_age` spacing guarantee.
    pub source_pk: [u8; 32],
    /// The payee.
    pub dest_pk: [u8; 32],
    /// The asset every instalment moves (same asset every time — see
    /// `tx::PaymentLeg` for the per-leg-asset case this plan deliberately
    /// does not generalise to).
    pub asset: Asset,
    /// The amount each instalment moves, in the asset's fixed-point `int64`
    /// units. Must be strictly positive.
    pub amount: i64,
    /// The source account's sequence number **at plan creation time**,
    /// i.e. one less than the first instalment's own `seqNum`. Every
    /// instalment chains off this.
    pub base_seq: i64,
    /// How many instalments this plan has (`N`). Must be 1..=[`MAX_INSTALMENTS`].
    pub count: u32,
    /// Per-instalment fee bid, in stroops (one `PAYMENT` operation per
    /// instalment, so this is the whole transaction's fee — no
    /// `tx::total_fee` scaling needed).
    pub base_fee_stroops: u32,
    /// The minimum real seconds that must pass, between one instalment's
    /// execution and the next one becoming valid — `0` for the first
    /// instalment (which needs no predecessor to wait on) and this value
    /// for every instalment after it. See the module docs for exactly what
    /// this does and does not prevent.
    pub min_seq_age_step_seconds: u64,
    /// The minimum ledger count that must close between one instalment's
    /// execution and the next becoming valid — the ledger-count analogue
    /// of `min_seq_age_step_seconds`, enforced independently (both must be
    /// satisfied). `0` disables this half of the gate.
    pub min_seq_ledger_gap_step: u32,
    /// This rail's id (`"stellar"`), bound into every instalment's memo —
    /// see `tx::recurring_memo_hash`.
    pub rail_id: String,
    /// The source account's StrKey address, for memo binding only (never
    /// used to derive the signing key).
    pub source_strkey: String,
    /// The destination's StrKey address, for memo binding only.
    pub dest_strkey: String,
    /// The caller-chosen reference for this whole plan (one value shared by
    /// every instalment; `tx::recurring_memo_hash` also binds the
    /// instalment index/count/`base_seq` so instalments never collide with
    /// each other despite sharing it).
    pub reference: String,
}

impl RecurringPlan {
    /// Build the unsigned [`Transaction`] for `instalment_index` (1-based;
    /// `1..=self.count`).
    ///
    /// Refused, rather than built and left to fail on-chain or silently
    /// misused:
    ///
    /// * `instalment_index == 0` or `> self.count` — out of range for this
    ///   plan;
    /// * `self.count == 0` or `> MAX_INSTALMENTS`;
    /// * `self.amount <= 0` — stellar-core rejects a non-positive `PAYMENT`;
    /// * `self.base_seq + instalment_index` overflowing `i64` — an absurd
    ///   `base_seq`/`count` combination.
    pub fn build_instalment(&self, instalment_index: u32) -> Result<Transaction, StellarError> {
        self.validate_shape()?;
        if instalment_index == 0 || instalment_index > self.count {
            return Err(StellarError::Config(format!(
                "instalment {instalment_index} is out of range for a {}-instalment plan",
                self.count
            )));
        }
        let seq_num = self
            .base_seq
            .checked_add(i64::from(instalment_index))
            .ok_or_else(|| {
                StellarError::Config(format!(
                    "base_seq {} + instalment {instalment_index} overflows i64",
                    self.base_seq
                ))
            })?;
        // Instalment 1 needs no predecessor to wait on; every instalment
        // after it must wait the fixed per-hop floor since the PREVIOUS
        // instalment actually executed (not a cumulative multiple — see
        // module docs on why scaling this by index would be wrong).
        let (min_seq_age, min_seq_ledger_gap) = if instalment_index == 1 {
            (0u64, 0u32)
        } else {
            (self.min_seq_age_step_seconds, self.min_seq_ledger_gap_step)
        };
        let cond = Preconditions::V2(PreconditionsV2 {
            time_bounds: None,
            ledger_bounds: None,
            // Deliberately None — see module docs "The mechanism".
            min_seq_num: None,
            min_seq_age: Duration(min_seq_age),
            min_seq_ledger_gap,
            extra_signers: Default::default(),
        });
        let memo = tx::recurring_memo_hash(
            &self.rail_id,
            &self.source_strkey,
            &self.dest_strkey,
            &self.reference,
            self.base_seq,
            instalment_index,
            self.count,
        );
        Ok(tx::build_transaction_with_preconditions(
            self.source_pk,
            self.dest_pk,
            self.asset.clone(),
            self.amount,
            seq_num,
            self.base_fee_stroops,
            memo,
            cond,
        ))
    }

    /// [`Self::build_instalment`] for every instalment `1..=self.count`, in
    /// order. Each is independent and separately signable; this is a
    /// convenience for building the whole plan up front.
    pub fn build_all_instalments(&self) -> Result<Vec<Transaction>, StellarError> {
        self.validate_shape()?;
        (1..=self.count).map(|i| self.build_instalment(i)).collect()
    }

    /// Build the (unsigned) **cancellation** transaction: a normally
    /// sequenced `BUMP_SEQUENCE` raising the source account's sequence to
    /// `self.base_seq + self.count` — at or beyond every instalment's own
    /// `seqNum`, so every not-yet-executed instalment becomes permanently
    /// unsatisfiable in this one step (see module docs). `current_seq` is
    /// the source account's *actual* current on-chain sequence (the caller
    /// must fetch it, exactly like `StellarRail::charge` fetches one before
    /// paying) — the cancel transaction itself is an ordinary transaction,
    /// so it needs the account's real `current_seq + 1` as its own `seqNum`,
    /// not anything derived from the plan.
    pub fn build_cancel_transaction(
        &self,
        current_seq: i64,
        fee: u32,
    ) -> Result<Transaction, StellarError> {
        self.validate_shape()?;
        let bump_to = self
            .base_seq
            .checked_add(i64::from(self.count))
            .ok_or_else(|| {
                StellarError::Config(format!(
                    "base_seq {} + count {} overflows i64",
                    self.base_seq, self.count
                ))
            })?;
        let cancel_seq_num = current_seq
            .checked_add(1)
            .ok_or_else(|| StellarError::Config("current_seq + 1 overflows i64".to_string()))?;
        let memo = tx::cancel_memo_hash(
            &self.rail_id,
            &self.source_strkey,
            &self.reference,
            self.base_seq,
        );
        Ok(tx::build_bump_sequence_transaction(
            self.source_pk,
            cancel_seq_num,
            fee,
            bump_to,
            memo,
        ))
    }

    fn validate_shape(&self) -> Result<(), StellarError> {
        if self.count == 0 {
            return Err(StellarError::Config(
                "a recurring plan needs at least one instalment".into(),
            ));
        }
        if self.count > MAX_INSTALMENTS {
            return Err(StellarError::Config(format!(
                "{} instalments exceeds this crate's sanity cap of {MAX_INSTALMENTS} \
                 (not a protocol limit — raise MAX_INSTALMENTS if you genuinely need more)",
                self.count
            )));
        }
        if self.amount <= 0 {
            return Err(StellarError::Config(format!(
                "instalment amount must be strictly positive, got {}",
                self.amount
            )));
        }
        Ok(())
    }
}

/// A pure, offline reimplementation of the `PreconditionsV2` validity rule
/// quoted in the module docs — **not** stellar-core's actual validator.
/// This is evidence that a *given, fixed* set of precondition values
/// behaves the way this module's docs say it does (i.e. that the
/// construction is internally consistent and does what it is claimed to
/// do); it is explicitly **not** evidence that real Horizon/stellar-core
/// enforces the identical outcome — only a live network round trip (which
/// this module does not attempt) could show that.
///
/// `account_seq` / `account_seqtime` / `account_ledger` model the source
/// account's current on-chain `seqNum`/`seqTime`/`seqLedger` fields;
/// `now_time` / `now_ledger` model the ledger closing this transaction
/// would be checked against.
pub fn would_be_valid(
    cond: &Preconditions,
    tx_seq_num: i64,
    account_seq: i64,
    account_seqtime: u64,
    account_seqledger: u32,
    now_time: u64,
    now_ledger: u32,
) -> bool {
    let Preconditions::V2(v2) = cond else {
        // This module only ever builds V2; anything else is out of scope
        // for this oracle.
        return false;
    };
    let seq_ok = match &v2.min_seq_num {
        Some(min) => min.0 <= account_seq && account_seq < tx_seq_num,
        None => account_seq == tx_seq_num - 1,
    };
    let age_ok = now_time >= account_seqtime.saturating_add(v2.min_seq_age.0);
    let gap_ok = now_ledger >= account_seqledger.saturating_add(v2.min_seq_ledger_gap);
    seq_ok && age_ok && gap_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx;

    fn plan(count: u32) -> RecurringPlan {
        RecurringPlan {
            source_pk: [1u8; 32],
            dest_pk: [9u8; 32],
            asset: tx::usdc_asset("USDC", [42u8; 32]).unwrap(),
            amount: 10_000_000,
            base_seq: 100,
            count,
            base_fee_stroops: 100,
            min_seq_age_step_seconds: 30,
            min_seq_ledger_gap_step: 2,
            rail_id: "stellar".into(),
            source_strkey: "GSRC".into(),
            dest_strkey: "GDST".into(),
            reference: "sub-42".into(),
        }
    }

    // ── Construction ────────────────────────────────────────────────────

    #[test]
    fn instalments_chain_sequence_numbers_off_base_seq() {
        let p = plan(3);
        let txs = p.build_all_instalments().unwrap();
        assert_eq!(txs.len(), 3);
        assert_eq!(txs[0].seq_num.0, 101);
        assert_eq!(txs[1].seq_num.0, 102);
        assert_eq!(txs[2].seq_num.0, 103);
    }

    #[test]
    fn the_first_instalment_needs_no_wait_but_later_ones_share_one_constant_floor() {
        let p = plan(3);
        let txs = p.build_all_instalments().unwrap();
        let precond = |t: &Transaction| {
            let Preconditions::V2(v2) = &t.cond else {
                panic!("expected V2 preconditions");
            };
            (
                v2.min_seq_age.0,
                v2.min_seq_ledger_gap,
                v2.min_seq_num.clone(),
            )
        };
        assert_eq!(precond(&txs[0]), (0, 0, None));
        // Instalment 2 and instalment 3 share the SAME floor — it is a
        // constant per-hop gate, not cumulative (see module docs on why
        // index * step would be wrong).
        assert_eq!(precond(&txs[1]), (30, 2, None));
        assert_eq!(precond(&txs[2]), (30, 2, None));
    }

    #[test]
    fn every_instalment_round_trips_through_the_official_xdr_decoder() {
        use stellar_xdr::curr::{Limits, ReadXdr, WriteXdr};
        let p = plan(4);
        for tx in p.build_all_instalments().unwrap() {
            let encoded = tx.to_xdr(Limits::none()).unwrap();
            let decoded = Transaction::from_xdr(&encoded, Limits::none()).unwrap();
            assert_eq!(tx, decoded);
        }
    }

    #[test]
    fn each_instalment_has_a_distinct_signing_hash() {
        // Distinct seq_num alone would already guarantee this, but the
        // point being tested is that the WHOLE mechanism (memo + cond +
        // seq_num together) produces N genuinely distinct signable
        // payloads, not that any one field alone does.
        let p = plan(5);
        let net_id = tx::network_id("Test SDF Network ; September 2015");
        let hashes: Vec<[u8; 32]> = p
            .build_all_instalments()
            .unwrap()
            .iter()
            .map(|t| tx::tx_hash(net_id, t).unwrap())
            .collect();
        for i in 0..hashes.len() {
            for j in (i + 1)..hashes.len() {
                assert_ne!(hashes[i], hashes[j], "instalments {i} and {j} collided");
            }
        }
    }

    #[test]
    fn a_signed_instalment_envelope_verifies_ed25519_against_the_source() {
        let p = plan(2);
        let signer = crate::keys::Keypair::from_seed([1u8; 32]);
        let net_id = tx::network_id("Test SDF Network ; September 2015");
        let txn = p.build_instalment(2).unwrap();
        let hash = tx::tx_hash(net_id, &txn).unwrap();
        let sig = signer.sign(&hash);
        assert!(crate::keys::Keypair::verify(&signer.pubkey(), &hash, &sig));
        let env = tx::envelope(txn, signer.pubkey().0, sig.0).unwrap();
        let decoded = tx::decode_single_payment(&env).unwrap();
        assert_eq!(decoded.amount, p.amount);
        assert_eq!(decoded.dest_pk, p.dest_pk);
    }

    // ── Refusals ────────────────────────────────────────────────────────

    #[test]
    fn refuses_zero_and_out_of_range_instalment_indices() {
        let p = plan(3);
        assert!(p.build_instalment(0).is_err());
        assert!(p.build_instalment(4).is_err());
        assert!(p.build_instalment(1).is_ok());
        assert!(p.build_instalment(3).is_ok());
    }

    #[test]
    fn refuses_an_empty_plan() {
        let p = plan(0);
        let err = p.build_all_instalments().unwrap_err();
        assert!(
            format!("{err}").contains("at least one instalment"),
            "{err}"
        );
    }

    #[test]
    fn refuses_an_absurd_instalment_count() {
        let mut p = plan(1);
        p.count = MAX_INSTALMENTS + 1;
        let err = p.build_instalment(1).unwrap_err();
        assert!(format!("{err}").contains("sanity cap"), "{err}");
    }

    #[test]
    fn refuses_a_non_positive_amount() {
        for bad in [0i64, -1, i64::MIN] {
            let mut p = plan(1);
            p.amount = bad;
            let err = p.build_instalment(1).unwrap_err();
            assert!(format!("{err}").contains("strictly positive"), "{err}");
        }
    }

    // ── Cancellation ────────────────────────────────────────────────────

    #[test]
    fn cancel_transaction_bumps_to_at_or_past_the_last_instalment() {
        let p = plan(5); // seqNums 101..=105
        let cancel = p.build_cancel_transaction(101, 100).unwrap();
        assert_eq!(cancel.cond, Preconditions::None);
        let stellar_xdr::curr::OperationBody::BumpSequence(b) = &cancel.operations[0].body else {
            panic!("expected a BUMP_SEQUENCE operation");
        };
        assert_eq!(
            b.bump_to.0, 105,
            "must bump to at least the LAST instalment's seqNum"
        );
    }

    #[test]
    fn min_seq_num_none_forbids_skipping_ahead_the_core_ordering_claim() {
        // This is the crux of the module's whole safety argument (see the
        // "no skipping, no reordering" claim in the module docs): with
        // `min_seq_num: None`, the spec text says a transaction is valid
        // ONLY when the account's sequence is EXACTLY `seqNum - 1` — not
        // "anywhere below it". Proven directly against the oracle: an
        // account sequence one step BEHIND the required value (which a
        // `<=` off-by-one would wrongly accept, since it is still "less
        // than tx_seq_num") must be invalid.
        let cond = Preconditions::V2(PreconditionsV2 {
            time_bounds: None,
            ledger_bounds: None,
            min_seq_num: None,
            min_seq_age: Duration(0),
            min_seq_ledger_gap: 0,
            extra_signers: Default::default(),
        });
        // tx_seq_num = 105: only account_seq == 104 may satisfy it.
        assert!(
            would_be_valid(&cond, 105, 104, 0, 0, 0, 0),
            "seqNum-1 must be accepted"
        );
        assert!(
            !would_be_valid(&cond, 105, 103, 0, 0, 0, 0),
            "an account sequence BEHIND seqNum-1 must still be rejected — \
             a permissive `<=` here would silently allow skipping ahead \
             before the account has caught up, which is exactly the \
             'no skipping' property this module claims to enforce"
        );
        assert!(
            !would_be_valid(&cond, 105, 105, 0, 0, 0, 0),
            "an account sequence AT tx_seq_num (already executed/bumped past) must be rejected"
        );
    }

    #[test]
    fn cancel_invalidates_every_outstanding_instalment_by_the_oracle() {
        // The offline validity oracle proves the actual mechanical claim:
        // after the cancel bump lands, no remaining instalment can ever
        // become valid again, at any future time or ledger.
        let p = plan(4); // seqNums 101..=104, base_seq 100
        let txs = p.build_all_instalments().unwrap();

        // Before cancellation: instalment 1 is valid right at base_seq,
        // with no wait.
        assert!(would_be_valid(
            &txs[0].cond,
            txs[0].seq_num.0,
            100,
            0,
            0,
            0,
            0
        ));

        // Simulate instalment 1 executing: account seq becomes 101,
        // seqtime/seqledger reset to "now" (say t=1000, ledger=50).
        let acc_seq_after_1 = 101;
        let acc_time_after_1 = 1000u64;
        let acc_ledger_after_1 = 50u32;

        // Instalment 2 is NOT valid immediately after (age/gap floor unmet)...
        assert!(!would_be_valid(
            &txs[1].cond,
            txs[1].seq_num.0,
            acc_seq_after_1,
            acc_time_after_1,
            acc_ledger_after_1,
            acc_time_after_1, // no time has passed
            acc_ledger_after_1,
        ));
        // ...but IS valid once the floor (30s / 2 ledgers) is met.
        assert!(would_be_valid(
            &txs[1].cond,
            txs[1].seq_num.0,
            acc_seq_after_1,
            acc_time_after_1,
            acc_ledger_after_1,
            acc_time_after_1 + 30,
            acc_ledger_after_1 + 2,
        ));

        // Now cancel: bump the account's sequence to 104 (>= every
        // remaining instalment's seqNum) while it was still at 101.
        let cancel = p.build_cancel_transaction(acc_seq_after_1, 100).unwrap();
        let stellar_xdr::curr::OperationBody::BumpSequence(b) = &cancel.operations[0].body else {
            panic!("expected a BUMP_SEQUENCE operation");
        };
        let acc_seq_after_cancel = b.bump_to.0;
        assert_eq!(acc_seq_after_cancel, 104);

        // Every remaining instalment (2, 3, 4) is now permanently invalid,
        // no matter how much time or how many ledgers pass.
        for txn in &txs[1..] {
            assert!(
                !would_be_valid(
                    &txn.cond,
                    txn.seq_num.0,
                    acc_seq_after_cancel,
                    acc_time_after_1,
                    acc_ledger_after_1,
                    u64::MAX, // even "the end of time"...
                    u32::MAX, // ...and every future ledger...
                ),
                "instalment with seqNum {} must be permanently invalid after cancellation",
                txn.seq_num.0
            );
        }
    }

    // ── Mutation-sensitivity guards (see module docs / B4 report) ──────

    #[test]
    fn the_oracle_itself_can_fail_a_valid_transaction_when_mutated() {
        // A control for `would_be_valid` itself: if the age check were
        // wrong (e.g. `>` instead of `>=`), this would catch it. Proven by
        // construction here rather than by mutating source, since this
        // function is test-only infrastructure, not shipped-guard code —
        // the guards that matter (the ones shipped in `RecurringPlan`) are
        // mutation-tested for real below and in the accompanying report.
        let cond = Preconditions::V2(PreconditionsV2 {
            time_bounds: None,
            ledger_bounds: None,
            min_seq_num: None,
            min_seq_age: Duration(30),
            min_seq_ledger_gap: 0,
            extra_signers: Default::default(),
        });
        // Exactly at the floor: valid.
        assert!(would_be_valid(&cond, 101, 100, 1000, 0, 1030, 0));
        // One second short: invalid.
        assert!(!would_be_valid(&cond, 101, 100, 1000, 0, 1029, 0));
    }

    #[test]
    fn the_ledger_gap_check_is_independently_enforced_not_just_the_age_check() {
        // Isolates min_seq_ledger_gap from min_seq_age: age set to 0 (always
        // satisfied) so ONLY the ledger-gap dimension can make this fail.
        // Without this test, deleting `gap_ok` from the final `seq_ok &&
        // age_ok && gap_ok` conjunction is invisible — every other test in
        // this module happens to set both floors together, so an age-only
        // check silently passes them all. (Found by mutation-testing the
        // shipped conjunction itself, not assumed.)
        let cond = Preconditions::V2(PreconditionsV2 {
            time_bounds: None,
            ledger_bounds: None,
            min_seq_num: None,
            min_seq_age: Duration(0),
            min_seq_ledger_gap: 5,
            extra_signers: Default::default(),
        });
        // Age floor (0) is trivially satisfied immediately...
        assert!(
            !would_be_valid(&cond, 101, 100, 1000, 50, 1000, 54),
            "one ledger short of the gap must still be invalid even though min_seq_age is 0"
        );
        // ...but the ledger-gap floor is not, until enough ledgers close.
        assert!(would_be_valid(&cond, 101, 100, 1000, 50, 1000, 55));

        // And the reverse isolation: gap floor 0 (trivial), age floor real —
        // confirms neither check can silently substitute for the other.
        let age_only = Preconditions::V2(PreconditionsV2 {
            time_bounds: None,
            ledger_bounds: None,
            min_seq_num: None,
            min_seq_age: Duration(30),
            min_seq_ledger_gap: 0,
            extra_signers: Default::default(),
        });
        assert!(!would_be_valid(&age_only, 101, 100, 1000, 50, 1029, 9999));
        assert!(would_be_valid(&age_only, 101, 100, 1000, 50, 1030, 50));
    }
}
