//! [`FailoverRail`] — try candidate rails in order, falling through on error,
//! **without ever crossing the settlement-class boundary silently**
//! (`PATALA.md` §3).

use async_trait::async_trait;

use crate::capabilities::RailClass;
use crate::error::{Error, Result};
use crate::rail::{PayRequest, PaymentRail, Quote, Receipt};

/// Orders candidate rails for a [`FailoverRail`] attempt.
///
/// Only [`InOrderPolicy`] ships today — it tries rails in the order they were
/// given to [`FailoverRail::new`]. This is a trait rather than a closed enum
/// so a later wave can add cheapest-quote or currency-match routing without
/// changing [`FailoverRail`] itself: a cheapest policy needs to call
/// [`PaymentRail::quote`] on each candidate to compare fees, which is why this
/// is `async` and fallible-shaped from the start rather than a plain
/// synchronous comparator that would need to be redesigned later.
#[async_trait]
pub trait RoutingPolicy: Send + Sync {
    /// Return indices into `rails`, in the order [`FailoverRail`] should try
    /// them for `req`.
    async fn order(&self, rails: &[Box<dyn PaymentRail>], req: &PayRequest) -> Vec<usize>;
}

/// The only policy implemented today: try rails in construction order.
#[derive(Clone, Copy, Debug, Default)]
pub struct InOrderPolicy;

#[async_trait]
impl RoutingPolicy for InOrderPolicy {
    async fn order(&self, rails: &[Box<dyn PaymentRail>], _req: &PayRequest) -> Vec<usize> {
        (0..rails.len()).collect()
    }
}

/// The eligible attempt order for one request, plus a record of the first
/// settlement-class boundary the policy order ran into (if any) so the caller
/// can report *why* nothing eligible was tried, rather than just that nothing
/// was.
struct Plan {
    order: Vec<usize>,
    blocked: Option<(RailClass, RailClass)>,
}

/// Tries a list of rails in policy order and falls through on error.
///
/// **The class boundary is enforced here, not left to callers to remember.**
/// Whichever rail a given attempt reaches first fixes that attempt's
/// settlement class; any later candidate whose [`RailClass`] differs is
/// skipped — never silently tried — unless [`FailoverRail::allow_cross_class`]
/// opted in. Completing a `NonCustodialFinal` request on a
/// `CustodialReversible` rail (or the reverse) without the caller asking for
/// that would hand them a payment with different guarantees than the one they
/// believe they hold — refundable-and-KYC'd instead of final-and-anonymous,
/// or vice versa. See `PATALA.md` §3.
pub struct FailoverRail {
    rails: Vec<Box<dyn PaymentRail>>,
    policy: Box<dyn RoutingPolicy>,
    allow_cross_class: bool,
}

impl FailoverRail {
    /// Wrap `rails`, tried in the given order (see [`InOrderPolicy`]).
    /// Cross-class failover is OFF by default — see the struct docs.
    pub fn new(rails: Vec<Box<dyn PaymentRail>>) -> Self {
        Self::with_policy(rails, Box::new(InOrderPolicy))
    }

    /// Wrap `rails`, tried in the order `policy` returns.
    pub fn with_policy(rails: Vec<Box<dyn PaymentRail>>, policy: Box<dyn RoutingPolicy>) -> Self {
        Self {
            rails,
            policy,
            allow_cross_class: false,
        }
    }

    /// Explicit opt-in to let this failover chain cross a settlement-class
    /// boundary (e.g. fall from a failed non-custodial rail to a custodial
    /// one). Off by default — see the struct docs and `PATALA.md` §3.
    pub fn allow_cross_class(mut self, allow: bool) -> Self {
        self.allow_cross_class = allow;
        self
    }

    /// The wrapped rails, in construction order (not policy order).
    pub fn rails(&self) -> &[Box<dyn PaymentRail>] {
        &self.rails
    }

    /// Resolve this request's eligible attempt order under the current
    /// policy and cross-class setting.
    async fn plan(&self, req: &PayRequest) -> Plan {
        let order = self.policy.order(&self.rails, req).await;
        let mut eligible = Vec::with_capacity(order.len());
        let mut intended: Option<RailClass> = None;
        let mut blocked = None;

        for idx in order {
            let Some(rail) = self.rails.get(idx) else {
                continue;
            };
            let class = rail.capabilities().class;
            match intended {
                None => {
                    intended = Some(class);
                    eligible.push(idx);
                }
                Some(want) if want == class => eligible.push(idx),
                Some(want) if self.allow_cross_class => {
                    let _ = want;
                    eligible.push(idx);
                }
                Some(want) => {
                    blocked.get_or_insert((want, class));
                }
            }
        }

        Plan {
            order: eligible,
            blocked,
        }
    }

    /// Turn a plan that produced no successful attempt into the error to
    /// surface. A blocked cross-class candidate is reported in preference to
    /// the last same-class rail's own failure: "there is a rail that could
    /// have handled this, opt in if you mean it" is more actionable than the
    /// specific reason the in-class rail happened to fail.
    fn terminal_error(last: Option<Error>, blocked: Option<(RailClass, RailClass)>) -> Error {
        match blocked {
            Some((from, to)) => Error::CrossClassFailover { from, to },
            None => last.unwrap_or(Error::AllRailsFailed),
        }
    }

    /// Try `quote` across eligible rails in order, returning the first
    /// success.
    pub async fn quote(&self, req: &PayRequest) -> Result<Quote> {
        let plan = self.plan(req).await;
        let mut last = None;
        for idx in &plan.order {
            match self.rails[*idx].quote(req).await {
                Ok(q) => return Ok(q),
                Err(e) => last = Some(e),
            }
        }
        Err(Self::terminal_error(last, plan.blocked))
    }

    /// Try `charge` across eligible rails in order, returning the first
    /// success.
    pub async fn charge(&self, req: &PayRequest) -> Result<Receipt> {
        let plan = self.plan(req).await;
        let mut last = None;
        for idx in &plan.order {
            match self.rails[*idx].charge(req).await {
                Ok(r) => return Ok(r),
                Err(e) => last = Some(e),
            }
        }
        Err(Self::terminal_error(last, plan.blocked))
    }

    /// Verify against the specific rail named on the receipt
    /// (`receipt.rail_id`) — a receipt is only ever meaningful to the rail
    /// that issued it, so this looks that one rail up rather than trying
    /// every wrapped rail. Fails closed the same way a single rail's `verify`
    /// does if the issuing rail isn't wrapped here at all.
    pub async fn verify(&self, receipt: &Receipt) -> Result<bool> {
        for rail in &self.rails {
            if rail.id() == receipt.rail_id {
                return rail.verify(receipt).await;
            }
        }
        Ok(false)
    }

    /// Refund against the specific rail named on the receipt.
    /// [`Error::Unsupported`] if that rail isn't wrapped here, or if it
    /// doesn't support refund itself.
    pub async fn refund(&self, receipt: &Receipt) -> Result<Receipt> {
        for rail in &self.rails {
            if rail.id() == receipt.rail_id {
                return rail.refund(receipt).await;
            }
        }
        Err(Error::Unsupported(
            "refund: the rail that issued this receipt is not wrapped by this FailoverRail",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::RailClass;
    use crate::mock::MockRail;

    fn req(reference: &str) -> PayRequest {
        PayRequest {
            amount_minor: 100,
            currency: "USDC".into(),
            destination: "dest".into(),
            reference: reference.into(),
        }
    }

    #[tokio::test]
    async fn falls_through_to_a_same_class_rail_on_failure() {
        let a = MockRail::new("a", RailClass::NonCustodialFinal, vec!["USDC".into()]).failing();
        let b = MockRail::new("b", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let failover = FailoverRail::new(vec![Box::new(a), Box::new(b)]);

        let r = failover
            .charge(&req("order-1"))
            .await
            .expect("same-class fallthrough must succeed on the second rail");
        assert_eq!(r.rail_id, "b", "the failing first rail must be skipped");
    }

    #[tokio::test]
    async fn refuses_to_cross_settlement_class_without_opt_in() {
        let crypto =
            MockRail::new("crypto", RailClass::NonCustodialFinal, vec!["USDC".into()]).failing();
        let fiat = MockRail::new("fiat", RailClass::CustodialReversible, vec!["USDC".into()]);
        let failover = FailoverRail::new(vec![Box::new(crypto), Box::new(fiat)]);

        let err = failover
            .charge(&req("order-2"))
            .await
            .expect_err("must not silently fail over across the settlement-class boundary");
        assert!(
            matches!(
                err,
                Error::CrossClassFailover {
                    from: RailClass::NonCustodialFinal,
                    to: RailClass::CustodialReversible,
                }
            ),
            "got {err:?}, expected a reported cross-class block"
        );
    }

    #[tokio::test]
    async fn cross_class_failover_works_once_explicitly_opted_in() {
        let crypto =
            MockRail::new("crypto", RailClass::NonCustodialFinal, vec!["USDC".into()]).failing();
        let fiat = MockRail::new("fiat", RailClass::CustodialReversible, vec!["USDC".into()]);
        let failover =
            FailoverRail::new(vec![Box::new(crypto), Box::new(fiat)]).allow_cross_class(true);

        let r = failover
            .charge(&req("order-3"))
            .await
            .expect("opted-in cross-class failover must be allowed to complete");
        assert_eq!(r.rail_id, "fiat");
    }

    #[tokio::test]
    async fn verify_dispatches_to_the_issuing_rail_only() {
        let a = MockRail::new("a", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let b = MockRail::new("b", RailClass::NonCustodialFinal, vec!["USDC".into()]);
        let a_receipt = a.charge(&req("order-4")).await.unwrap();

        let failover = FailoverRail::new(vec![Box::new(a), Box::new(b)]);
        assert!(failover.verify(&a_receipt).await.unwrap());

        // A receipt naming a rail this FailoverRail never wrapped fails
        // closed rather than being checked against an arbitrary member.
        let mut foreign = a_receipt.clone();
        foreign.rail_id = "somewhere-else".into();
        assert!(!failover.verify(&foreign).await.unwrap());
    }

    #[tokio::test]
    async fn empty_failover_reports_all_rails_failed() {
        let failover = FailoverRail::new(Vec::new());
        let err = failover.charge(&req("order-5")).await.unwrap_err();
        assert!(matches!(err, Error::AllRailsFailed));
    }
}
