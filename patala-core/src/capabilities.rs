//! The capability model — `PATALA.md` §3, implemented exactly.

use serde::{Deserialize, Serialize};

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
        };
        let fiat = RailCapabilities {
            class: RailClass::CustodialReversible,
            reversible: true,
            requires_kyc: true,
            holds_funds: true,
            currencies: vec!["USD".into(), "NGN".into()],
            settlement: Settlement::Days(2),
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
}
