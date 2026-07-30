//! §3 THE SEAM, pre-flight side: [`DestinationVerdict`] — everything a rail can
//! *honestly* say about a destination address before any money moves.
//!
//! # Why this exists: paying a customer back is a two-party flow
//!
//! **Never send a crypto refund to the address the payment came from.** BitPay,
//! Coinbase Commerce and OpenNode all ask the customer for a destination
//! instead, and for one concrete reason: a *sending* address is very often an
//! exchange **withdrawal** address, and an exchange does not credit funds
//! arriving there to the customer who withdrew from it. Money sent there is
//! usually unrecoverable — by the customer, by the merchant and by patala.
//!
//! So asking the customer is not a fallback, it is the design. The flow has
//! three steps and two parties, and the middle one is not automatable:
//!
//! 1. the merchant initiates a payout,
//! 2. the **customer** supplies an address they actually control, and a human
//!    confirms it — [`crate::PaymentRail::validate_destination`] is what that
//!    human is shown,
//! 3. the merchant approves, and the money moves as an ordinary
//!    [`crate::PaymentRail::charge`] whose `destination` is that address.
//!
//! Step 3 is a **compensating payment**, not a reversal: a second, independent
//! payment in the opposite direction with its own transaction, its own fee and
//! its own confirmation, able to fail on its own. That is why
//! [`crate::PaymentRail::refund`] stays [`crate::Error::Unsupported`] on a
//! `NonCustodialFinal` rail — see its docs.
//!
//! # What this can and cannot decide
//!
//! A rail can decide, offline, whether a string is a well-formed address *for
//! its own network*: alphabet, length, checksum, and — where the network
//! encodes it, as Solana does by whether the point is on the ed25519 curve —
//! whether it is a plain wallet or a program/contract account.
//!
//! It cannot decide who owns it. In particular:
//!
//! **patala never tries to detect whether an address belongs to an exchange.**
//! That needs commercial address-attribution data (Chainalysis, TRM) — hosted
//! services, which would break the rule that nothing here depends on a third
//! party, and which the default offline build exists to avoid. A *heuristic*
//! would be worse than nothing: a host who trusts "looks safe" and loses a
//! customer's money is worse off than one told plainly that this cannot be
//! known. So this module validates what is decidable and carries the rest —
//! [`EXCHANGE_DEPOSIT_CAVEAT`] — on **every** verdict, including the most
//! positive one.
//!
//! That is also why the best status here is called
//! [`DestinationStatus::StructurallyValid`] and not `Valid`, and why there is
//! deliberately **no** `is_valid()` / `is_safe()` predicate on
//! [`DestinationVerdict`]: there is no answer this crate can give that means
//! "safe to send to", so it offers no method that could be mistaken for one.
//!
//! # The purity contract
//!
//! [`crate::PaymentRail::validate_destination`] is **pure and offline**: no
//! network, no clock, no filesystem, no global state. It has to run in a
//! browser through wasm, on a gate device with no uplink, and in a unit test
//! with no RPC configured. Anything that needs to ask a chain a question — does
//! this account exist, is it rent-exempt, does it have a trustline for this
//! asset — is a *different* method, and not this one.

use serde::{Deserialize, Serialize};

/// The one thing no rail can determine offline, in a sentence a UI can show
/// verbatim.
///
/// Carried on **every** [`DestinationVerdict`], including
/// [`DestinationStatus::StructurallyValid`] ones, and it travels in the
/// serialized form on purpose: the consumers that most need it (a Go merchant
/// backend through the sidecar, a Python script, a browser) never read these
/// rustdocs.
pub const EXCHANGE_DEPOSIT_CAVEAT: &str = "patala cannot tell whether this address belongs to an \
     exchange. A structurally perfect address may still be an exchange deposit or withdrawal \
     address, and an exchange will not credit funds arriving there to the person who gave you \
     the address. Determining that needs commercial address-attribution data patala deliberately \
     does not depend on, so only the person who owns the wallet can confirm they control this \
     address and can receive on it.";

/// What a rail established about a destination address, offline.
///
/// Five states, not a bool and not a `Result`, because a UI has to render each
/// one differently: "you mistyped that", "that is a Stellar address and this is
/// a Solana payout", "that is a contract, not a wallet", "this looks
/// well-formed — now confirm you control it", and "this rail cannot tell you
/// anything, so you must be sure yourself".
///
/// **No variant means "safe to send to".** See the module docs and
/// [`EXCHANGE_DEPOSIT_CAVEAT`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DestinationStatus {
    /// Structurally invalid for this rail: wrong alphabet, wrong length, bad
    /// checksum, empty. The rail positively established a defect — a caller
    /// **must not** charge to this address, and should put the cursor back in
    /// the field.
    Malformed,
    /// A well-formed address, but for a **different network** than this rail
    /// moves money on — a Stellar `G...` pasted into a Solana payout, an
    /// Ethereum `0x...` pasted into a Sui one. Common, and expensive: the
    /// funds would land on a chain the recipient is not watching, or nowhere
    /// at all. A caller **must not** charge to this address.
    WrongNetwork,
    /// A valid address on this rail's network that is **not a plain wallet** —
    /// a program/contract account, a Solana PDA (off-curve, therefore
    /// unsignable), a token mint. Nobody holds a key for it, so funds sent
    /// there are typically unrecoverable. A caller **must not** charge to this
    /// address, even though it "exists".
    NotAWallet,
    /// Every check this rail can make offline passed: right alphabet, right
    /// length, checksum verifies, right network, and it looks like a plain
    /// wallet.
    ///
    /// **This is not "valid" and not "safe".** It is the *absence of a
    /// decidable defect*. Whether the person on the other end can actually
    /// receive on it — whether it is their own wallet or an exchange deposit
    /// address they cannot be credited on — is exactly what
    /// [`EXCHANGE_DEPOSIT_CAVEAT`] says cannot be known here. A human still
    /// has to confirm: see [`DestinationVerdict::requires_human_confirmation`].
    StructurallyValid,
    /// This rail cannot check this destination at all, and says so instead of
    /// guessing. The honest answer for a fiat rail (whose `destination` is an
    /// opaque processor-side token, meaningful only to the processor) and for
    /// any rail that has not implemented a parser yet — it is the trait's
    /// default, so no rail is ever forced to claim a check it does not
    /// perform.
    ///
    /// **Never treat this as valid.** It means "a human must decide", and it
    /// is the reason the default is this and not
    /// [`DestinationStatus::StructurallyValid`].
    Unknown,
}

/// The result of [`crate::PaymentRail::validate_destination`]: what one rail
/// could decide about one address, offline, plus what it could not.
///
/// Built through the constructors below rather than field-by-field so
/// [`Self::exchange_deposit_caveat`] and [`Self::human_must_confirm`] cannot
/// be forgotten by a rail, and so a caller that supplies no reason still gets
/// a sentence it can show a person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestinationVerdict {
    /// The rail that formed this verdict (matches [`crate::PaymentRail::id`]).
    /// A verdict is only ever about the network *that* rail moves money on:
    /// `mock`'s opinion of a Solana address is worth nothing.
    pub rail_id: String,
    /// What was established. See [`DestinationStatus`].
    pub status: DestinationStatus,
    /// Why, in one sentence, for a person to read. Never empty — a caller has
    /// to be able to tell someone *what* is wrong with what they typed, not
    /// just that something is.
    pub reason: String,
    /// Always `true`, for every [`DestinationStatus`] including
    /// [`DestinationStatus::StructurallyValid`].
    ///
    /// A field and not only a method because this requirement has to survive
    /// the JSON and FFI boundaries: the sidecar's Go caller and the Python
    /// binding see this struct's serialized form, where a Rust method does not
    /// exist. If you are writing a UI, this is the checkbox the human ticks
    /// after reading [`Self::exchange_deposit_caveat`], and there is no verdict
    /// that lets you skip it.
    pub human_must_confirm: bool,
    /// [`EXCHANGE_DEPOSIT_CAVEAT`], verbatim, on every verdict. Show it next
    /// to [`Self::reason`] whenever a human is about to approve a payout.
    pub exchange_deposit_caveat: String,
}

/// The sentence used when a rail supplies no reason of its own. A verdict with
/// an empty reason would be a verdict a UI cannot explain, so there is always
/// one of these instead.
fn default_reason(status: DestinationStatus) -> &'static str {
    match status {
        DestinationStatus::Malformed => "this is not a well-formed address for this rail's network",
        DestinationStatus::WrongNetwork => {
            "this address is well-formed, but belongs to a different network than this rail pays on"
        }
        DestinationStatus::NotAWallet => {
            "this address is valid on this network but is not a plain wallet — nobody may hold a \
             key for it, so funds sent there can be unrecoverable"
        }
        DestinationStatus::StructurallyValid => {
            "every check this rail can make offline passed; whether the recipient can actually \
             receive on it is not something this rail can know"
        }
        DestinationStatus::Unknown => {
            "this rail cannot check this destination offline, so a human who controls the \
             receiving wallet must confirm it"
        }
    }
}

impl DestinationVerdict {
    /// A verdict from `rail_id` with `status` and a human-readable `reason`.
    ///
    /// An empty or whitespace-only `reason` is replaced with a per-status
    /// default: an unexplained refusal is nearly as unhelpful as no refusal.
    /// [`Self::human_must_confirm`] and [`Self::exchange_deposit_caveat`] are
    /// set here, for every status, and that is the whole reason this
    /// constructor exists.
    pub fn new(
        rail_id: impl Into<String>,
        status: DestinationStatus,
        reason: impl Into<String>,
    ) -> Self {
        let reason = reason.into();
        Self {
            rail_id: rail_id.into(),
            status,
            reason: if reason.trim().is_empty() {
                default_reason(status).to_string()
            } else {
                reason
            },
            // Never conditional on `status`. See the field's docs.
            human_must_confirm: true,
            exchange_deposit_caveat: EXCHANGE_DEPOSIT_CAVEAT.to_string(),
        }
    }

    /// [`DestinationStatus::Malformed`] — the address is structurally wrong.
    pub fn malformed(rail_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(rail_id, DestinationStatus::Malformed, reason)
    }

    /// [`DestinationStatus::WrongNetwork`] — well-formed, wrong chain.
    pub fn wrong_network(rail_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(rail_id, DestinationStatus::WrongNetwork, reason)
    }

    /// [`DestinationStatus::NotAWallet`] — a program/contract/PDA, not a
    /// keyholder.
    pub fn not_a_wallet(rail_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(rail_id, DestinationStatus::NotAWallet, reason)
    }

    /// [`DestinationStatus::StructurallyValid`] — no decidable defect found.
    /// Not "valid", not "safe": read that variant's docs before calling this.
    pub fn structurally_valid(rail_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(rail_id, DestinationStatus::StructurallyValid, reason)
    }

    /// [`DestinationStatus::Unknown`] — this rail cannot check, and will not
    /// pretend to. The trait's default verdict.
    pub fn unknown(rail_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(rail_id, DestinationStatus::Unknown, reason)
    }

    /// True when this rail positively established a defect:
    /// [`DestinationStatus::Malformed`], [`DestinationStatus::WrongNetwork`] or
    /// [`DestinationStatus::NotAWallet`].
    ///
    /// **Guards fail closed**: a refusal is a refusal, never a warning to click
    /// past. A caller must not charge to a destination whose verdict is a
    /// refusal, and must not offer a human the option to confirm it anyway —
    /// unlike the residual risk in [`Self::exchange_deposit_caveat`], these
    /// three are things the rail *knows*.
    ///
    /// `false` does **not** mean "go ahead": it is also `false` for
    /// [`DestinationStatus::Unknown`], where nothing at all was established.
    /// That is why there is no `is_valid()` here — see the module docs.
    pub fn is_refusal(&self) -> bool {
        // Exhaustive on purpose: a new status must be classified here
        // deliberately, and cannot default to "not a refusal" by omission.
        match self.status {
            DestinationStatus::Malformed
            | DestinationStatus::WrongNetwork
            | DestinationStatus::NotAWallet => true,
            DestinationStatus::StructurallyValid | DestinationStatus::Unknown => false,
        }
    }

    /// Always `true`, whatever the [`DestinationStatus`].
    ///
    /// The Rust-side twin of [`Self::human_must_confirm`]. It takes no
    /// branch and has no failure mode: there is no verdict this crate can
    /// produce that establishes an address is safe to send a customer's money
    /// to, so the human confirmation step is unconditional. If you are looking
    /// for the call that lets you skip it, it does not exist — that absence is
    /// the point.
    pub const fn requires_human_confirmation(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every status, so each assertion below is genuinely exhaustive rather
    /// than exhaustive-looking.
    const ALL: [DestinationStatus; 5] = [
        DestinationStatus::Malformed,
        DestinationStatus::WrongNetwork,
        DestinationStatus::NotAWallet,
        DestinationStatus::StructurallyValid,
        DestinationStatus::Unknown,
    ];

    #[test]
    fn no_status_waives_the_human_confirmation() {
        // The single most important property in this module: there is no
        // verdict — not even the most positive one — that a caller can read as
        // "safe, skip the human".
        for status in ALL {
            let v = DestinationVerdict::new("mock", status, "");
            assert!(
                v.human_must_confirm,
                "{status:?} must still require a human to confirm"
            );
            assert!(v.requires_human_confirmation());
        }
    }

    #[test]
    fn every_status_carries_the_exchange_deposit_caveat() {
        for status in ALL {
            let v = DestinationVerdict::new("mock", status, "some rail-specific reason");
            assert_eq!(
                v.exchange_deposit_caveat, EXCHANGE_DEPOSIT_CAVEAT,
                "{status:?} must carry the caveat verbatim, in the serialized form too"
            );
            assert!(v.exchange_deposit_caveat.contains("exchange"));
        }
    }

    #[test]
    fn refusals_are_exactly_the_three_established_defects() {
        assert!(DestinationVerdict::malformed("mock", "bad checksum").is_refusal());
        assert!(
            DestinationVerdict::wrong_network("mock", "that is a Stellar address").is_refusal()
        );
        assert!(DestinationVerdict::not_a_wallet("mock", "that is a program").is_refusal());

        // Not refusals — but neither is a green light, and `StructurallyValid`
        // and `Unknown` are not interchangeable either.
        let valid = DestinationVerdict::structurally_valid("mock", "parses");
        let unknown = DestinationVerdict::unknown("mock", "cannot check");
        assert!(!valid.is_refusal() && !unknown.is_refusal());
        assert_ne!(valid.status, unknown.status);
    }

    #[test]
    fn a_verdict_always_has_a_reason_a_person_can_read() {
        for status in ALL {
            for supplied in ["", "   ", "\n\t"] {
                let v = DestinationVerdict::new("mock", status, supplied);
                assert_eq!(v.reason, default_reason(status), "{status:?}");
                assert!(!v.reason.trim().is_empty());
            }
        }
        // A rail's own wording wins when it has one.
        let v = DestinationVerdict::malformed("mock", "expected 32 bytes, got 31");
        assert_eq!(v.reason, "expected 32 bytes, got 31");
    }

    #[test]
    fn a_verdict_names_the_rail_that_formed_it() {
        // A verdict is only about the network its own rail pays on, so the
        // caller must always be able to see whose opinion it is reading.
        let v = DestinationVerdict::structurally_valid("solana", "on-curve 32-byte base58");
        assert_eq!(v.rail_id, "solana");
    }
}
