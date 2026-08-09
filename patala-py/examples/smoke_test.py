#!/usr/bin/env python3
"""Smoke test for the patala-py UniFFI binding.

Imports the *built* module (see patala-py/README.md "Build & run" for how
`bindings/python/patala.py` + its sibling `libpatala_py.dylib`/`.so` get
there — the module is named after patala's UniFFI namespace, `patala`, while
the library it loads is still this crate's own `libpatala_py`) and
drives a full charge -> verify round trip against `MockRail`, entirely
offline, entirely from Python. This is not a mock of the binding — it is the
actual generated Python module calling into the actual compiled Rust cdylib
over ctypes/UniFFI's FFI, exercising the same `PatalaRail` object
`patala-uniffi/src/lib.rs` defines.

If the cdylib was built with `--features solana`/`stellar`/`hyperswitch`
(README.md "Build & run"), this script also *constructs* the matching real
rail(s) from Python and reads back `capabilities()` — proving `RailClass`/
`RailCapabilities` and the real-rail constructors are reachable from Python,
not just `MockRail`. No live network call is made against any real rail here.

Run (from the workspace root, after generating bindings — see the README):

    PYTHONPATH=patala-py/bindings/python python3 patala-py/examples/smoke_test.py
"""
import hashlib
import hmac
import json
import sys

from patala import (
    DestinationStatus,
    PatalaError,
    PatalaRail,
    PayRequest,
    RailClass,
    WebhookDelivery,
    WebhookStatus,
    exchange_deposit_caveat,
)


def check_destination_surface() -> None:
    """Drive `validate_destination` from Python — every verdict, offline.

    This is the pre-flight half of the two-party payout flow (see
    `docs/compensating-payments.md`): on a final rail there is no reversal, so
    paying a customer back is a second, independent `charge()` to an address
    the **customer** supplies — never the address the payment came from, which
    is very often an exchange withdrawal address where the funds cannot be
    credited back to them.

    Never skipped: MockRail's synthetic `<network>:<kind>:<label>` grammar
    makes all five verdicts reachable with no chain, no processor and no
    feature flags, so this runs on every build.
    """
    rail = PatalaRail.new_mock(
        id="mock",
        _class=RailClass.NON_CUSTODIAL_FINAL,
        currencies=["USDC"],
        fee_minor=0,
        failing=False,
    )
    # The offline stand-in for a rail that cannot check at all — the shape of
    # every fiat rail, whose destination is an opaque processor-side token.
    opaque = PatalaRail.new_mock_without_destination_checks(
        id="opaque",
        _class=RailClass.CUSTODIAL_REVERSIBLE,
        currencies=["USD"],
        fee_minor=0,
        failing=False,
    )

    cases = [
        (rail, "mock:wallet:alice", DestinationStatus.STRUCTURALLY_VALID, False),
        (rail, "mock:program:vault", DestinationStatus.NOT_A_WALLET, True),
        (rail, "stellar:wallet:alice", DestinationStatus.WRONG_NETWORK, True),
        (rail, "definitely-not-an-address", DestinationStatus.MALFORMED, True),
        # Guards fail closed: the one defect decidable with no rail knowledge.
        (rail, "", DestinationStatus.MALFORMED, True),
        (opaque, "cus_opaque_token", DestinationStatus.UNKNOWN, False),
    ]

    seen = set()
    for which, dest, want_status, want_refusal in cases:
        verdict = which.validate_destination(dest)
        assert verdict.status == want_status, (
            f"validate_destination({dest!r}) status={verdict.status!r}, want {want_status!r}"
        )
        assert verdict.is_refusal is want_refusal, (
            f"validate_destination({dest!r}) is_refusal={verdict.is_refusal!r}, want {want_refusal!r}"
        )
        assert verdict.rail_id == which.id(), "a verdict must name the rail that formed it"
        assert verdict.reason.strip(), f"{dest!r} produced a verdict with nothing to show a person"
        # On EVERY verdict, including the most positive one. patala cannot tell
        # whether an address belongs to an exchange and will not guess, so the
        # human confirmation step is unconditional.
        assert verdict.human_must_confirm is True, (
            f"{dest!r} must still require a human to confirm"
        )
        assert "exchange" in verdict.exchange_deposit_caveat, (
            f"{dest!r} must carry the exchange-deposit caveat verbatim"
        )
        seen.add(verdict.status)

    assert len(seen) == 5, (
        f"only {len(seen)} distinct verdicts reached Python; all five must survive "
        "the FFI boundary as distinct values, or the design is flattened"
    )

    # The caveat is reachable before there is a verdict to render — for the form
    # where a customer is first asked for a payout address.
    assert exchange_deposit_caveat() == rail.validate_destination("mock:wallet:alice").exchange_deposit_caveat, (
        "the standalone caveat and the one on a verdict must be the same text"
    )

    print(
        f"validate_destination OK: all 5 verdicts reached Python distinctly "
        f"({', '.join(sorted(s.name for s in seen))}); every one requires human confirmation"
    )


def check_webhook_surface() -> bool:
    """Verify a real, signed Stripe webhook delivery from Python.

    Skipped unless the cdylib was built with a fiat feature that compiles the
    Stripe adapter in (`--features fiat-stripe`, or `fiat-all`). Returns
    whether it ran, so the caller can say so out loud rather than passing
    silently.

    This is the check that matters most for this surface: webhook
    verification used to live in Rust free functions outside the trait, which
    a binding cannot reach at all, so a Python (or Go, or Swift) consumer
    could only ever confirm a payment by polling `verify`. Everything below
    runs over ctypes into the compiled Rust — nothing here is mocked.
    """
    secret = "whsec_fake_secret_for_unit_tests"
    now = 1_700_000_000

    if not hasattr(PatalaRail, "new_fiat"):
        # The whole `fiat` feature is off: `new_fiat` is not in this cdylib.
        return False
    try:
        rail = PatalaRail.new_fiat(
            "stripe",
            {
                "secret_key": "sk_test_from_python_smoke",
                "webhook_secret": secret,
                "requires_kyc": "false",
                "currencies": "USD",
                "settlement_days": "2",
                "timeout_secs": "5",
            },
        )
    except PatalaError.InvalidRequest:
        # `fiat` is on but the Stripe adapter itself was not compiled in;
        # `new_fiat` reports an unknown provider name that way.
        return False

    body = json.dumps(
        {
            "id": "evt_py_smoke_1",
            "type": "checkout.session.completed",
            "data": {
                "object": {
                    "id": "cs_py_smoke_1",
                    "payment_status": "paid",
                    "amount_total": 5_000,
                    "currency": "usd",
                    "client_reference_id": "py-smoke-order-webhook",
                }
            },
        },
        separators=(",", ":"),
    ).encode()

    signature = hmac.new(
        secret.encode(), f"{now}.".encode() + body, hashlib.sha256
    ).hexdigest()
    delivery = WebhookDelivery(
        raw_body=body,
        headers={"Stripe-Signature": f"t={now},v1={signature}"},
        query=None,
        now_unix=now,
    )

    event = rail.verify_webhook(delivery)
    assert event.rail_id == "stripe", f"unexpected rail_id: {event.rail_id!r}"
    assert event.event_id == "evt_py_smoke_1", "event_id is the replay-dedup key"
    assert event.reference == "py-smoke-order-webhook"
    assert event.status == WebhookStatus.SETTLED, f"unexpected status: {event.status!r}"
    assert isinstance(event.amount_minor, int), "amount_minor must be an int, never a float"
    assert event.amount_minor == 5_000
    assert event.currency == "USD"
    print(
        f"webhook OK: authenticated from Python, status={event.status}, "
        f"amount_minor={event.amount_minor} (int), reference={event.reference}"
    )

    # A tampered body must fail closed — the same bytes no longer match.
    tampered = WebhookDelivery(
        raw_body=body.replace(b"5000", b"1"),
        headers={"Stripe-Signature": f"t={now},v1={signature}"},
        query=None,
        now_unix=now,
    )
    try:
        rail.verify_webhook(tampered)
        raise AssertionError("a tampered webhook delivery must never verify")
    except PatalaError.InvalidRequest:
        pass

    # A rail with no push delivery raises rather than inventing an event.
    manual = PatalaRail.new_fiat("manual", {})
    try:
        manual.verify_webhook(delivery)
        raise AssertionError("manual has no processor and must report unsupported")
    except PatalaError.Unsupported:
        pass

    print("webhook OK: tampered delivery rejected; `manual` reports unsupported")
    return True


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

    # MockRail has no processor, so its webhook answer must be "unsupported"
    # — never a fabricated event. This part is never skipped.
    try:
        rail.verify_webhook(
            WebhookDelivery(raw_body=b"{}", headers={}, query=None, now_unix=1_700_000_000)
        )
        raise AssertionError("MockRail has no webhook surface and must say so")
    except PatalaError.Unsupported:
        print("webhook OK: MockRail reports unsupported rather than faking an event")

    check_destination_surface()

    if not check_webhook_surface():
        print(
            "\nWEBHOOK VERIFICATION NOT VERIFIED against a real adapter: this cdylib "
            "was built without a fiat feature, so only MockRail's `unsupported` answer "
            "was checked. Rebuild with `--features fiat-stripe` (or `fiat-all`) to "
            "exercise a genuine signed delivery end to end."
        )

    print("\nALL PYTHON SMOKE ASSERTIONS PASSED")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"SMOKE TEST FAILED: {e}", file=sys.stderr)
        sys.exit(1)
