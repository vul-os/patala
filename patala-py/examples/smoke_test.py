#!/usr/bin/env python3
"""Smoke test for the patala-py UniFFI binding.

Imports the *built* module (see patala-py/README.md "Build & run" for how
`bindings/python/patala_py.py` + its sibling `.dylib`/`.so` get there) and
drives a full charge -> verify round trip against `MockRail`, entirely
offline, entirely from Python. This is not a mock of the binding — it is the
actual generated Python module calling into the actual compiled Rust cdylib
over ctypes/UniFFI's FFI, exercising the same `PatalaRail` object
`patala-py/src/lib.rs` defines.

If the cdylib was built with `--features solana`/`stellar`/`hyperswitch`
(README.md "Build & run"), this script also *constructs* the matching real
rail(s) from Python and reads back `capabilities()` — proving `RailClass`/
`RailCapabilities` and the real-rail constructors are reachable from Python,
not just `MockRail`. No live network call is made against any real rail here.

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

    # --- Real rails (TASK 1: patala-py exposes more than MockRail) ---
    #
    # `new_solana`/`new_stellar`/`new_hyperswitch` only exist on `PatalaRail`
    # when the cdylib this module was generated from was built with the
    # matching cargo feature (`cargo build -p patala-py --features solana`,
    # etc — see README.md "Build & run"). This smoke test runs unmodified
    # against every build: it *exercises* whichever real-rail constructors
    # are present, and simply notes which ones are absent, rather than
    # failing a plain MockRail-only build.
    #
    # No live network call is made here (no live Solana RPC / Horizon /
    # Hyperswitch instance is reachable from this environment) — this proves
    # the rail is constructible and its capability/class model is readable
    # from Python, exactly what PATALA.md §3's "consumer reads class, never a
    # provider-specific type" contract requires; a live `quote`/`charge`
    # against these constructed rails remains UNVERIFIED AGAINST LIVE, same as
    # the underlying Rust crates' own honesty notes.
    real_rails_checked = []

    if hasattr(PatalaRail, "new_solana"):
        solana_rail = PatalaRail.new_solana(
            rpc_url="https://api.devnet.solana.com",
            cluster="devnet",
            keypair_seed=None,  # verify-only rail; no signer attached
        )
        assert solana_rail.id() == "solana"
        solana_caps = solana_rail.capabilities()
        assert solana_caps._class == RailClass.NON_CUSTODIAL_FINAL
        assert solana_caps.holds_funds is False
        assert solana_caps.currencies == ["USDC"]
        print(
            f"real rail OK: solana constructed from Python, class={solana_caps._class}, "
            f"currencies={solana_caps.currencies}, holds_funds={solana_caps.holds_funds}"
        )
        real_rails_checked.append("solana")

    if hasattr(PatalaRail, "new_stellar"):
        stellar_rail = PatalaRail.new_stellar(
            horizon_url="https://horizon-testnet.stellar.org",
            network="public",  # "public" needs no usdc_issuer (well-known Circle issuer)
            usdc_issuer=None,
            keypair_seed=None,
        )
        assert stellar_rail.id() == "stellar"
        stellar_caps = stellar_rail.capabilities()
        assert stellar_caps._class == RailClass.NON_CUSTODIAL_FINAL
        assert stellar_caps.holds_funds is False
        assert stellar_caps.currencies == ["USDC"]
        print(
            f"real rail OK: stellar constructed from Python, class={stellar_caps._class}, "
            f"currencies={stellar_caps.currencies}, holds_funds={stellar_caps.holds_funds}"
        )
        real_rails_checked.append("stellar")

    if hasattr(PatalaRail, "new_hyperswitch"):
        hyperswitch_rail = PatalaRail.new_hyperswitch(
            base_url="https://hyperswitch.internal.example.org",
            api_key="snd_test_from_python_smoke",
            connector="paystack",
            webhook_secret=None,
            requires_kyc=True,
            currencies=["USD", "NGN"],
            settlement_days=2,
            timeout_secs=30,
        )
        assert hyperswitch_rail.id() == "hyperswitch"
        hs_caps = hyperswitch_rail.capabilities()
        assert hs_caps._class == RailClass.CUSTODIAL_REVERSIBLE
        assert hs_caps.holds_funds is True, "the fronted processor custodies funds"
        assert hs_caps.currencies == ["USD", "NGN"]
        print(
            f"real rail OK: hyperswitch constructed from Python, class={hs_caps._class}, "
            f"currencies={hs_caps.currencies}, holds_funds={hs_caps.holds_funds}"
        )
        real_rails_checked.append("hyperswitch")

    if real_rails_checked:
        print(f"\nREAL RAILS REACHABLE FROM PYTHON: {', '.join(real_rails_checked)}")
    else:
        print(
            "\n(no real-rail features were compiled into this build — only MockRail "
            "was exercised; rebuild with --features solana,stellar,hyperswitch to "
            "cover the real rails too)"
        )

    print("\nALL PYTHON SMOKE ASSERTIONS PASSED")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"SMOKE TEST FAILED: {e}", file=sys.stderr)
        sys.exit(1)
