//! # patala-fiat
//!
//! The fiat foundation this task's brief asks for: the ISO-4217 minor-unit
//! currency table, the provider-registry pattern, the always-on `manual`
//! rail, and two pilot `RailClass::CustodialReversible` adapters (Stripe,
//! Paystack) — all ported from cackle's Go payment package
//! (`internal/payments/` + `internal/money/`). See `PORTING.md` for the
//! precise, repeatable recipe a future agent follows to port ONE more
//! cackle adapter onto this same foundation.
//!
//! | Module | What it is | Ported from |
//! |---|---|---|
//! | [`currency`] | ISO-4217 exponent table + minor/major conversions | `internal/money/currency.go` + `internal/payments/currency.go` |
//! | [`httpshared`] | Bounded reads + shared webhook-HMAC verify | `internal/payments/httpshared.go` |
//! | [`registry`] | Provider-registry pattern (`manual` always-on) | `internal/payments/registry.go` |
//! | [`manual`] | The offline, always-available default rail | `internal/payments/manual.go` |
//! | `stripe` (feature `stripe`) | Pilot 1: Checkout Sessions | `internal/payments/stripe.go` |
//! | `paystack` (feature `paystack`) | Pilot 2: Transaction API | `internal/payments/paystack.go` |
//! | `adyen` (feature `adyen`) | Pay by Link | `internal/payments/adyen.go` |
//! | `checkoutcom` (feature `checkoutcom`) | Hosted Payments Page | `internal/payments/checkoutcom.go` |
//! | `mollie` (feature `mollie`) | Payments API | `internal/payments/mollie.go` |
//! | `mercadopago` (feature `mercadopago`) | Checkout Pro (Preferences) | `internal/payments/mercadopago.go` |
//!
//! ## Honesty conventions (binding — `PATALA.md` §8, carried into this crate)
//!
//! - **UNVERIFIED AGAINST LIVE.** Exactly like `patala-hyperswitch`: no live
//!   Stripe or Paystack account was reachable from the environment this
//!   crate was written in. Every request/response shape was checked against
//!   cackle's own adapter (which cites Stripe's/Paystack's published docs)
//!   and every unit test here mocks HTTP with `wiremock`. A green `cargo
//!   test --all-features -p patala-fiat` proves this crate builds the
//!   requests those docs describe and parses the responses they describe —
//!   it is not proof this works against a live merchant sandbox.
//! - **Non-custodial invariant.** `patala` itself never holds funds
//!   (`PATALA.md` §1, §8). Both pilot rails set
//!   `RailCapabilities::holds_funds: true` — that describes **Stripe's**/
//!   **Paystack's** own custody of funds in flight, never this crate's or
//!   patala's. No function in this crate receives, stores, or forwards
//!   actual funds; every call here only ever moves JSON describing a
//!   request to move funds that the processor itself carries out.
//! - **Honest pending/redirect lifecycle.** A charge that has not settled
//!   (a Stripe Checkout Session awaiting the buyer, a Paystack transaction
//!   awaiting authorization) returns `Receipt { amount_minor: 0, .. }` —
//!   `charge()` returning `Ok` is never treated as settlement. Callers MUST
//!   gate on `PaymentRail::verify` returning `Ok(true)`, exactly as
//!   `patala_core::Receipt`'s own doc comment requires.
//! - **Fail-closed verify.** Every `verify()` in this crate re-fetches from
//!   the processor (never trusts a cached/embedded status) and returns
//!   `Ok(false)` on any doubt — wrong rail, malformed proof, unsettled
//!   status, amount/currency mismatch. `Err` is reserved for an operational
//!   failure to even perform the check.
//! - **Never fabricate.** No receipt, balance, or "success" a processor
//!   didn't actually return.
//!
//! ## Offline-by-default build (`PATALA.md` §8, §9)
//!
//! This crate's `default` Cargo feature set is EMPTY: [`currency`],
//! [`registry`], and [`manual`] compile and test with zero optional
//! dependencies — no `reqwest`, no `hmac`/`sha2`/`hex`. The `stripe` and
//! `paystack` features each opt into those network/crypto deps
//! independently. The workspace root `Cargo.toml` keeps this crate in
//! `default-members` precisely because its default build stays this lean —
//! see that file's own comment.

#[cfg(feature = "adyen")]
pub mod adyen;
#[cfg(feature = "btcpay")]
pub mod btcpay;
#[cfg(feature = "checkoutcom")]
pub mod checkoutcom;
#[cfg(feature = "coinbasecommerce")]
pub mod coinbasecommerce;
pub mod currency;
#[cfg(feature = "flutterwave")]
pub mod flutterwave;
#[cfg(any(
    feature = "stripe",
    feature = "paystack",
    feature = "adyen",
    feature = "checkoutcom",
    feature = "mollie",
    feature = "mercadopago",
    feature = "btcpay",
    feature = "coinbasecommerce",
    feature = "flutterwave",
    feature = "iyzico",
    feature = "lnbits",
    feature = "midtrans",
    feature = "opennode",
    feature = "payfast",
    feature = "paypal",
    feature = "payu",
    feature = "razorpay",
    feature = "square",
    feature = "xendit",
    feature = "yoco"
))]
pub mod httpshared;
#[cfg(feature = "iyzico")]
pub mod iyzico;
#[cfg(feature = "lnbits")]
pub mod lnbits;
pub mod manual;
#[cfg(feature = "mercadopago")]
pub mod mercadopago;
#[cfg(feature = "midtrans")]
pub mod midtrans;
#[cfg(feature = "mollie")]
pub mod mollie;
#[cfg(feature = "opennode")]
pub mod opennode;
#[cfg(feature = "payfast")]
pub mod payfast;
#[cfg(feature = "paypal")]
pub mod paypal;
#[cfg(feature = "paystack")]
pub mod paystack;
#[cfg(feature = "payu")]
pub mod payu;
#[cfg(feature = "razorpay")]
pub mod razorpay;
pub mod registry;
#[cfg(feature = "square")]
pub mod square;
#[cfg(feature = "stripe")]
pub mod stripe;
#[cfg(feature = "xendit")]
pub mod xendit;
#[cfg(feature = "yoco")]
pub mod yoco;

pub use currency::CurrencyError;
pub use manual::{ManualRail, ManualRecord, ManualStatus, RAIL_ID_MANUAL};
pub use registry::{CapabilityFilter, Registry, RegistryError, ENV_PATALA_FIAT_RAILS};

#[cfg(feature = "adyen")]
pub use adyen::{AdyenConfig, AdyenRail};
#[cfg(feature = "btcpay")]
pub use btcpay::{BTCPayConfig, BTCPayRail};
#[cfg(feature = "checkoutcom")]
pub use checkoutcom::{CheckoutComConfig, CheckoutComRail};
#[cfg(feature = "coinbasecommerce")]
pub use coinbasecommerce::{CoinbaseCommerceConfig, CoinbaseCommerceRail};
#[cfg(feature = "flutterwave")]
pub use flutterwave::{FlutterwaveConfig, FlutterwaveRail};
#[cfg(feature = "iyzico")]
pub use iyzico::{IyzicoConfig, IyzicoRail};
#[cfg(feature = "lnbits")]
pub use lnbits::{LNbitsConfig, LNbitsRail};
#[cfg(feature = "mercadopago")]
pub use mercadopago::{MercadoPagoConfig, MercadoPagoRail};
#[cfg(feature = "midtrans")]
pub use midtrans::{MidtransConfig, MidtransRail};
#[cfg(feature = "mollie")]
pub use mollie::{MollieConfig, MollieRail};
#[cfg(feature = "opennode")]
pub use opennode::{OpenNodeConfig, OpenNodeRail};
#[cfg(feature = "payfast")]
pub use payfast::{PayFastConfig, PayFastRail};
#[cfg(feature = "paypal")]
pub use paypal::{PayPalConfig, PayPalRail};
#[cfg(feature = "paystack")]
pub use paystack::{PaystackConfig, PaystackRail};
#[cfg(feature = "payu")]
pub use payu::{PayUConfig, PayURail};
#[cfg(feature = "razorpay")]
pub use razorpay::{RazorpayConfig, RazorpayRail};
#[cfg(feature = "square")]
pub use square::{SquareConfig, SquareRail};
#[cfg(feature = "stripe")]
pub use stripe::{StripeConfig, StripeRail};
#[cfg(feature = "xendit")]
pub use xendit::{XenditConfig, XenditRail};
#[cfg(feature = "yoco")]
pub use yoco::{YocoConfig, YocoRail};
