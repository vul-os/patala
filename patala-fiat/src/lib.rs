//! # patala-fiat
//!
//! The fiat foundation: the ISO-4217 minor-unit currency table, the
//! provider-registry pattern, the always-on `manual` rail, and twenty
//! feature-gated `RailClass::CustodialReversible` processor adapters — all
//! ported from cackle's Go payment package (`internal/payments/` +
//! `internal/money/`). See `README.md` for what a consumer needs, and
//! `PORTING.md` for the precise, repeatable recipe a future agent follows to
//! port ONE more cackle adapter onto this same foundation.
//!
//! | Module | What it is | Ported from |
//! |---|---|---|
//! | [`currency`] | ISO-4217 exponent table (147 currencies, checksum-pinned) + minor/major conversions | `internal/money/currency.go` + `internal/payments/currency.go` |
//! | `httpshared` | Bounded reads + shared webhook-HMAC verify | `internal/payments/httpshared.go` |
//! | [`registry`] | Provider-registry pattern (`manual` always-on) | `internal/payments/registry.go` |
//! | [`manual`] | The offline, always-available default rail | `internal/payments/manual.go` |
//! | [`destination`] | Offline `validate_destination` for every rail here — no cackle precursor (new with `patala_core`'s pre-flight seam) | — |
//!
//! ## `destination` on a fiat rail is not a payout address
//!
//! Worth stating up front because it is the thing a caller most often assumes
//! wrongly: on every rail in this crate `PayRequest::destination` is a
//! post-checkout **redirect URL**, the **buyer's email**, or simply
//! **unread** — never a place money goes. So no
//! [`patala_core::PaymentRail::validate_destination`] here ever reports
//! `StructurallyValid`; the honest ceiling is `Unknown`, and what each rail
//! *can* still decide offline (is this a URL at all, is this an email at all,
//! is this a blockchain address someone pasted into the wrong field) lives in
//! [`destination`]. Giving a customer their money back on these rails is
//! [`patala_core::PaymentRail::refund`] — the money goes back the way it came
//! and no destination is involved.
//!
//! One module per processor, each behind a Cargo feature of the same name and
//! each ported from the cackle adapter of the same name: `adyen`, `btcpay`,
//! `checkoutcom`, `coinbasecommerce`, `flutterwave`, `iyzico`, `lnbits`,
//! `mercadopago`, `midtrans`, `mollie`, `opennode`, `payfast`, `paypal`,
//! `paystack`, `payu`, `razorpay`, `square`, `stripe`, `xendit`, `yoco`.
//! Each is a `<name>::<Name>Rail` implementing [`patala_core::PaymentRail`]
//! plus a `<name>::webhook` module its
//! [`patala_core::PaymentRail::verify_webhook`] wraps.
//!
//! ## Honesty conventions (binding — `PATALA.md` §8, carried into this crate)
//!
//! - **UNVERIFIED AGAINST LIVE.** Exactly like `patala-hyperswitch`: no live
//!   merchant account for ANY of these processors was reachable from the
//!   environment this crate was written in. Every request/response shape was
//!   checked against cackle's own adapter (which cites the processor's
//!   published docs) and every unit test here mocks HTTP with `wiremock`. A
//!   green `cargo test --all-features -p patala-fiat` proves this crate
//!   builds the requests those docs describe and parses the responses they
//!   describe — it is not proof this works against a live merchant sandbox.
//! - **Non-custodial invariant.** `patala` itself never holds funds
//!   (`PATALA.md` §1, §8). Every processor rail sets
//!   `RailCapabilities::holds_funds: true` — that describes the
//!   **processor's** own custody of funds in flight, never this crate's or
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
//! - **Fail-closed webhooks.** Every [`patala_core::PaymentRail::verify_webhook`]
//!   here returns `Err` on a missing, malformed, stale or mismatched
//!   signature — reaching `Ok` means the rail is satisfied the delivery came
//!   from its own processor. A scheme that authenticates a notification
//!   without asserting anything about money reports
//!   [`patala_core::WebhookStatus::Unconfirmed`], never `NotSettled`.
//! - **Never fabricate.** No receipt, balance, or "success" a processor
//!   didn't actually return.
//!
//! ## Offline-by-default build (`PATALA.md` §8, §9)
//!
//! This crate's `default` Cargo feature set is EMPTY: [`currency`],
//! [`registry`], and [`manual`] compile and test with zero optional
//! dependencies — no `reqwest`, no `hmac`/`sha2`/`hex`. Each processor
//! feature opts into exactly the network/crypto deps its own scheme needs,
//! independently of every other. The workspace root `Cargo.toml` keeps this crate in
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
pub mod destination;
#[cfg(feature = "flutterwave")]
pub mod flutterwave;
#[cfg(feature = "_adapter")]
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
