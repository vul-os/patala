//! The iyzico adapter (Turkey — Checkout Form). One
//! `patala_core::PaymentRail` talking to iyzico's Checkout Form API. Ported
//! from cackle's `internal/payments/iyzico.go`. See `rail.rs`'s module docs
//! for the full `Provider` -> `PaymentRail` mapping and `PORTING.md` for the
//! general recipe this follows.
//!
//! Gated behind the `iyzico` Cargo feature — see the crate root docs and
//! `Cargo.toml`.
//!
//! **UNVERIFIED AGAINST LIVE**: same disclosure as every rail in this crate
//! beyond `manual` (`PORTING.md` §10). Cackle's own file header rates its
//! confidence SPLIT: MEDIUM-HIGH on the security-critical shape (the
//! Checkout Form callback carries no signature at all — see `webhook.rs`'s
//! module docs), but LOW-MEDIUM, EXPLICITLY FLAGGED, on the exact "IYZWS"
//! outbound request-signing byte sequence (see `rail.rs`'s `auth_headers`)
//! — cackle's own header notes iyzico has since introduced a newer
//! HMACSHA256-based "IYZWSv2" scheme for some merchants that this port does
//! NOT implement, exactly as cackle does not. **Also not attempted**
//! (mirroring cackle exactly): iyzico's mandatory buyer/address/basket-item
//! fields (`identityNumber`, `registrationAddress`, `city`, `ip`,
//! `shippingAddress`, `billingAddress`, `basketItems`, ...) are extensive
//! and neither cackle's `Order` nor `patala_core::PayRequest` carries most
//! of them — a real request will likely need extending before iyzico
//! accepts it. This is a functional gap, not a security one: an
//! incomplete/rejected request never reports a false "paid".

pub mod config;
mod models;
mod proof;
pub mod rail;
pub mod webhook;

pub use config::IyzicoConfig;
pub use rail::IyzicoRail;
