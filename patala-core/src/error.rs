//! The error type every [`crate::PaymentRail`] operation returns through.
//!
//! Kept small and honest: a rail that cannot do something says
//! [`Error::Unsupported`] rather than faking success (`PATALA.md` §3, §8), and
//! [`crate::PaymentRail::verify`] fails closed — see that method's docs, not a
//! variant here, since "unverified" is expressed as `Ok(false)`, not an error.

use crate::capabilities::RailClass;

/// Errors surfaced by a rail, or by [`crate::FailoverRail`] routing across
/// several.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The rail has no implementation of this operation and will not fake
    /// one. This is the required response for, e.g., `refund` on a rail whose
    /// settlement is final.
    #[error("{0} is not supported by this rail")]
    Unsupported(&'static str),

    /// The rail attempted the operation and it failed: RPC unreachable,
    /// processor declined, insufficient funds, and the like.
    #[error("rail error: {0}")]
    Rail(String),

    /// The request itself is malformed — a zero amount, an empty currency, an
    /// empty destination — and was rejected before any rail did real work.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// A [`crate::FailoverRail`] had a same-class rail available and it failed,
    /// but the only remaining candidate sits on the other side of the
    /// settlement-class boundary and cross-class failover was not opted into.
    ///
    /// This is the load-bearing error of the whole seam: silently completing a
    /// `NonCustodialFinal` request on a `CustodialReversible` rail (or the
    /// reverse) would hand the caller a payment with different guarantees than
    /// the one they thought they were getting. See `PATALA.md` §3.
    #[error(
        "failover from a {from:?} rail to a {to:?} rail would cross the settlement-class \
         boundary; call `.allow_cross_class(true)` on the FailoverRail if this is intentional"
    )]
    CrossClassFailover {
        /// The settlement class this failover chain committed to.
        from: RailClass,
        /// The settlement class of the blocked candidate.
        to: RailClass,
    },

    /// No rail was available or eligible to even try (e.g. an empty
    /// [`crate::FailoverRail`]).
    #[error("no rail satisfied the request")]
    AllRailsFailed,
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;
