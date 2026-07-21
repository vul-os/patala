#!/usr/bin/env python3
"""Smoke test for the patala-py UniFFI binding.

Imports the *built* module (see patala-py/README.md "Build & run" for how
`bindings/python/patala_py.py` + its sibling `.dylib`/`.so` get there) and
drives a full charge -> verify round trip against `MockRail`, entirely
offline, entirely from Python. This is not a mock of the binding — it is the
actual generated Python module calling into the actual compiled Rust cdylib
over ctypes/UniFFI's FFI, exercising the same `PatalaRail` object
`patala-py/src/lib.rs` defines.

Run (from the workspace root, after generating bindings — see the README):

    PYTHONPATH=patala-py/bindings/python python3 patala-py/examples/smoke_test.py
"""
import sys

from patala_py import PatalaRail, PayRequest, RailClass, PatalaError


def main() -> None:
    rail = PatalaRail.new_mock(
        id="mock",
        _class=RailClass.NON_CUSTODIAL_FINAL,
        currencies=["USDC", "USD"],
        fee_minor=0,
        failing=False,
    )

    assert rail.id() == "mock", f"unexpected rail id: {rail.id()!r}"

    caps = rail.capabilities()
    # `class` is a Python keyword, so the UniFFI-generated record names this
    # field `_class` (same pattern as the `_class` constructor kwarg above).
    assert caps._class == RailClass.NON_CUSTODIAL_FINAL, (
        "capabilities._class must be readable from Python and match the "
        f"constructor's class, got {caps._class!r}"
    )
    assert caps.holds_funds is False, "a NonCustodialFinal rail must not hold funds"
    assert caps.reversible is False, "a NonCustodialFinal rail must not be reversible"
    assert caps.currencies == ["USDC", "USD"], f"unexpected currencies: {caps.currencies!r}"
    print(f"capabilities OK: class={caps._class}, currencies={caps.currencies}")

    req = PayRequest(
        amount_minor=1_250,
        currency="USDC",
        destination="dest-anything",
        reference="py-smoke-order-1",
    )

    quote = rail.quote(req)
    assert isinstance(quote.amount_minor, int), "amount_minor must be an int, never a float"
    assert quote.total_minor == 1_250, f"unexpected total_minor: {quote.total_minor!r}"
    print(f"quote OK: total_minor={quote.total_minor} (int, not float)")

    receipt = rail.charge(req)
    assert receipt.amount_minor == 1_250
    assert receipt.rail_id == "mock"
    assert receipt.reference == "py-smoke-order-1"
    print(f"charge OK: receipt rail_id={receipt.rail_id!r} amount_minor={receipt.amount_minor}")

    valid = rail.verify(receipt)
    assert valid is True, "a genuine receipt must verify"
    print("verify OK: genuine receipt verified true")

    # Fail-closed: a tampered receipt must never verify.
    tampered = receipt
    tampered.amount_minor = 999_999
    tampered_valid = rail.verify(tampered)
    assert tampered_valid is False, "a tampered receipt must never verify"
    print("verify OK: tampered receipt verified false (fail-closed)")

    # An unsupported currency surfaces as a typed PatalaError, not a crash.
    try:
        rail.charge(
            PayRequest(
                amount_minor=100,
                currency="EUR",
                destination="dest",
                reference="py-smoke-order-2",
            )
        )
        raise AssertionError("expected PatalaError.InvalidRequest for an unsupported currency")
    except PatalaError.InvalidRequest as e:
        print(f"error mapping OK: unsupported currency raised {e!r}")

    print("\nALL PYTHON SMOKE ASSERTIONS PASSED")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"SMOKE TEST FAILED: {e}", file=sys.stderr)
        sys.exit(1)
