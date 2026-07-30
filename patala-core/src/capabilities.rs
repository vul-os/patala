//! The capability model — `PATALA.md` §3, implemented exactly.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The settlement class of a rail — **in the type**, on purpose.
///
/// A consumer must be able to read this before it decides what to show the
/// payer: a refundable "pending" state plus a card form
/// ([`RailClass::CustodialReversible`]), or a wallet address plus a signed
/// final receipt ([`RailClass::NonCustodialFinal`]). Never flatten these two
/// into a bool — that would erase exactly the distinction the whole crate
/// exists to preserve. See `PATALA.md` §3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RailClass {
    /// Custodial, reversible (chargebacks possible), usually KYC'd, delayed
    /// settlement. Any fiat processor behind Hyperswitch is this class.
    CustodialReversible,
    /// Non-custodial, final (no reversal), wallet-to-wallet, near-instant.
    /// Solana/Stellar USDC is this class.
    NonCustodialFinal,
}

/// How long a rail takes to reach final settlement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Settlement {
    /// Final at broadcast/acceptance — typical of a non-custodial chain rail.
    Instant,
    /// Final after a bounded number of seconds.
    Seconds(u32),
    /// Final after a number of days — card-network style T+2/T+3.
    Days(u8),
}

/// What a rail can and cannot do, and under what guarantees. This — plus
/// [`crate::PaymentRail`] — is the entire surface a consumer is allowed to
/// program against; nothing names a provider-specific type (`PATALA.md` §3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RailCapabilities {
    /// The settlement class. See [`RailClass`].
    pub class: RailClass,
    /// Whether a completed payment can be reversed (chargeback/refund) at the
    /// rail level.
    pub reversible: bool,
    /// Whether the rail requires KYC/identity verification of the payer.
    pub requires_kyc: bool,
    /// Whether the **rail's own processor** custodies funds in flight.
    ///
    /// This describes the rail, never patala: no code path in this crate may
    /// make the substrate itself hold funds (`PATALA.md` §1, §8). A fiat rail
    /// sets this `true` because its processor (Stripe/Paystack/… via
    /// Hyperswitch) does; a non-custodial chain rail sets it `false`.
    pub holds_funds: bool,
    /// Currencies/assets this rail can move, e.g. `["USDC"]` or
    /// `["USD", "NGN"]`.
    pub currencies: Vec<String>,
    /// How long finality takes. See [`Settlement`].
    pub settlement: Settlement,
    /// Can this rail settle **N payouts as one atomic event** — either every
    /// leg lands or none does — rather than N independent operations that can
    /// partly fail?
    ///
    /// `patala_core`'s own seam (see [`crate::PaymentRail`]) is
    /// single-recipient: one [`crate::PayRequest`] is one payee. An atomic
    /// N-way split, where one exists at all, is built *beneath* the seam, per
    /// rail (`docs/shared-economics.md` §5) — this field is how a rail
    /// declares whether it has one.
    ///
    /// **Always `false` for every fiat processor rail, structurally, not as a
    /// gap to close.** A payout through Stripe/Paystack/Hyperswitch/etc. is N
    /// independent HTTP calls; there is no way to make N API calls atomic.
    /// A chain rail can be `true` in principle, but only once this crate
    /// actually exposes an atomic multi-party operation for it — a chain's
    /// theoretical capability is not this rail implementation's capability,
    /// so this stays `false` until such an operation is built and reachable.
    ///
    /// A consumer that needs atomic settlement calls
    /// [`RailCapabilities::require_atomic_multi_party`] and gets a refusal on
    /// any rail that cannot provide it, rather than the request silently
    /// degrading into N separate payments with no shared success/failure.
    pub atomic_multi_party: bool,
}

impl RailCapabilities {
    /// Refuse rather than silently degrade: call this before attempting an
    /// atomic multi-party settlement on a rail, and get [`Error::Unsupported`]
    /// back on any rail that cannot provide it (structurally, for a fiat
    /// processor, or simply not-yet-built for a chain rail) instead of the
    /// operation quietly turning into N independent payments with no shared
    /// success/failure. See [`Self::atomic_multi_party`]'s docs.
    ///
    /// ```
    /// use patala_core::{RailCapabilities, RailClass, Settlement};
    ///
    /// let atomic = RailCapabilities {
    ///     class: RailClass::NonCustodialFinal,
    ///     reversible: false,
    ///     requires_kyc: false,
    ///     holds_funds: false,
    ///     currencies: vec!["USDC".into()],
    ///     settlement: Settlement::Instant,
    ///     atomic_multi_party: true,
    /// };
    /// assert!(atomic.require_atomic_multi_party().is_ok());
    ///
    /// let mut fiat_shaped = atomic.clone();
    /// fiat_shaped.atomic_multi_party = false;
    /// assert!(fiat_shaped.require_atomic_multi_party().is_err());
    /// ```
    pub fn require_atomic_multi_party(&self) -> Result<()> {
        if self.atomic_multi_party {
            Ok(())
        } else {
            Err(Error::Unsupported(
                "atomic_multi_party: this rail settles N payouts as N independent operations, \
                 never as one atomic event",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settlement_class_is_readable_from_capabilities() {
        let crypto = RailCapabilities {
            class: RailClass::NonCustodialFinal,
            reversible: false,
            requires_kyc: false,
            holds_funds: false,
            currencies: vec!["USDC".into()],
            settlement: Settlement::Instant,
            atomic_multi_party: false,
        };
        let fiat = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: true,
            holds_funds: true,
            currencies: vec!["USD".into(), "NGN".into()],
            settlement: Settlement::Days(2),
            atomic_multi_party: false,
        };

        // A consumer decides UX purely from `class`, without needing to know
        // which provider is behind it.
        assert_eq!(crypto.class, RailClass::NonCustodialFinal);
        assert_eq!(fiat.class, RailClass::CustodialReversible);
        assert_ne!(crypto.class, fiat.class);

        // The two classes are never flattened into a single bool: a fiat rail
        // is reversible/KYC'd/custodial together, a crypto rail is none of
        // those together, and the type keeps them as separate readable facts.
        assert!(fiat.reversible && fiat.requires_kyc && fiat.holds_funds);
        assert!(!crypto.reversible && !crypto.requires_kyc && !crypto.holds_funds);
    }

    fn caps(atomic_multi_party: bool) -> RailCapabilities {
        RailCapabilities {
            class: RailClass::NonCustodialFinal,
            reversible: false,
            requires_kyc: false,
            holds_funds: false,
            currencies: vec!["USDC".into()],
            settlement: Settlement::Instant,
            atomic_multi_party,
        }
    }

    #[test]
    fn an_incapable_rail_is_refused_rather_than_silently_degraded() {
        // A rail that declares no atomic multi-party support must be refused
        // outright, never silently accepted and then split into N
        // independent operations with no shared success/failure.
        let err = caps(false).require_atomic_multi_party().unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(_)),
            "expected Unsupported, got {err:?}"
        );
        assert!(format!("{err}").contains("atomic_multi_party"));
    }

    #[test]
    fn a_capable_rail_satisfies_the_atomic_requirement() {
        assert!(caps(true).require_atomic_multi_party().is_ok());
    }

    #[test]
    fn every_fiat_processor_rail_in_this_workspace_declares_no_atomic_multi_party() {
        // Structural, not a gap: N payouts through any processor are N
        // independent API calls. This is a fast, offline canary against a
        // future rail accidentally claiming `true` for a processor that
        // cannot possibly back it — grep rather than a cross-crate dependency
        // (patala-fiat/patala-hyperswitch are not dependencies of
        // patala-core, and must never become one — PATALA.md §1).
        let fiat_dirs = [
            concat!(env!("CARGO_MANIFEST_DIR"), "/../patala-fiat/src"),
            concat!(env!("CARGO_MANIFEST_DIR"), "/../patala-hyperswitch/src"),
        ];
        let mut checked = 0usize;
        for dir in fiat_dirs {
            for entry in walk_rs_files(std::path::Path::new(dir)) {
                let src = std::fs::read_to_string(&entry)
                    .unwrap_or_else(|e| panic!("reading {entry:?}: {e}"));
                if !src.contains("RailCapabilities {") {
                    continue;
                }
                checked += 1;
                assert!(
                    src.contains("atomic_multi_party: false"),
                    "{entry:?} constructs RailCapabilities but does not declare \
                     atomic_multi_party: false — a fiat/processor rail can never \
                     honestly claim atomic multi-party settlement"
                );
            }
        }
        assert!(
            checked >= 20,
            "expected to check at least the 20 documented fiat processor rails plus \
             hyperswitch, only found {checked} files constructing RailCapabilities — \
             this canary may have stopped finding its targets"
        );
    }

    fn walk_rs_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk_rs_files(&path));
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
        out
    }
}
