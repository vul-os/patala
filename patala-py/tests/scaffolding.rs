//! What this crate still has to be true for, now that it holds no binding
//! definitions of its own.
//!
//! `patala-py` is three lines of `pub use` over [`patala_uniffi`]. Two things
//! about that arrangement are load-bearing and neither is obvious from
//! reading the source, so both are pinned here:
//!
//! 1. **The UniFFI namespace is `patala`, not `patala_py`.** That is the whole
//!    reason `patala-uniffi` was split out: the namespace becomes the module
//!    name in every generated binding, and ten languages were about to inherit
//!    `patala_py`. If someone "simplifies" `setup_scaffolding!("patala")` back
//!    to `setup_scaffolding!()`, the namespace silently reverts to the crate
//!    name and every binding's package clause changes with it.
//! 2. **The re-export is what keeps the surface reachable** through this
//!    crate's name, so nothing that referred to `patala_py::PatalaRail` had to
//!    change.
//!
//! What is NOT pinned here, because a Rust test cannot see it: that
//! `libpatala_py.{dylib,so}` re-exports `patala-uniffi`'s `#[no_mangle]`
//! scaffolding symbols. That is a property of the linked *cdylib*, not of this
//! test binary. The root `Makefile`'s `smoke-python` target is what proves it,
//! in CI, the only way it can honestly be proven: a real `python3` loads that
//! exact cdylib through the generated ctypes wrapper and drives a charge →
//! verify round trip through it. If the symbols were not re-exported, that job
//! fails at import.

use patala_py::{PatalaRail, PayRequest, RailClass};

extern "C" {
    /// Emitted by `uniffi::setup_scaffolding!("patala")` in `patala-uniffi`:
    /// UniFFI names this symbol `UNIFFI_META_NAMESPACE_<NAMESPACE>`.
    ///
    /// Referencing it is a **link-time** assertion that the namespace is
    /// exactly `patala`. With `setup_scaffolding!()` (namespace derived from
    /// the crate name) the symbol would be
    /// `UNIFFI_META_NAMESPACE_PATALA_UNIFFI` and this test target would fail
    /// to link — which is the point. The declared type is a lie of
    /// convenience: the real static is a byte blob of unknown length and this
    /// test only ever takes its address, never reads through it.
    static UNIFFI_META_NAMESPACE_PATALA: u8;
}

#[test]
fn the_uniffi_namespace_is_patala_not_patala_py() {
    // Reaching the symbol at all is the assertion; this keeps the reference
    // from being optimised away and gives the test something to check.
    let addr = std::ptr::addr_of!(UNIFFI_META_NAMESPACE_PATALA);
    assert!(
        !addr.is_null(),
        "UNIFFI_META_NAMESPACE_PATALA resolved to a null address"
    );
}

#[test]
fn the_surface_is_reachable_through_this_crates_name() {
    let rail = PatalaRail::new_mock(
        "mock".into(),
        RailClass::NonCustodialFinal,
        vec!["USDC".into()],
        0,
        false,
    );
    assert_eq!(rail.id(), "mock");

    let req = PayRequest {
        amount_minor: 500,
        currency: "USDC".into(),
        destination: "dest-anything".into(),
        reference: "patala-py-reexport-1".into(),
    };
    let receipt = rail.charge(req).expect("charge");
    assert_eq!(receipt.amount_minor, 500);
    assert!(
        rail.verify(receipt).expect("verify"),
        "a genuine receipt must verify through the re-exported surface"
    );
}

#[test]
fn a_tampered_receipt_still_fails_closed_through_the_reexport() {
    // The re-export must not be a place where anything is re-implemented; the
    // fail-closed contract is patala-core's and arrives here untouched.
    let rail = PatalaRail::new_mock(
        "mock".into(),
        RailClass::NonCustodialFinal,
        vec!["USDC".into()],
        0,
        false,
    );
    let mut receipt = rail
        .charge(PayRequest {
            amount_minor: 500,
            currency: "USDC".into(),
            destination: "dest-anything".into(),
            reference: "patala-py-reexport-2".into(),
        })
        .expect("charge");
    receipt.amount_minor = 999_999;
    assert!(
        !rail.verify(receipt).expect("verify"),
        "a tampered receipt must never verify"
    );
}
