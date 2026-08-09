#!/usr/bin/env python3
"""patala from Python over the sidecar — a separate process, no FFI at all.

    python3 sdks/python/examples/sidecar_charge.py

This spawns `patala-sidecar` on a free loopback port with a freshly generated
token, drives a full quote -> charge -> verify round trip against it with
nothing but the standard library, and shuts it down again. Nothing is left
running when it exits.

Everything runs against `MockRail` — deterministic, offline, no credentials.

Why you might pick this over ../../patala-py, even though patala's in-process
cost is unusually low (see README.md):

  - **Key isolation.** A non-custodial rail's signing key lives in whichever
    process calls charge. One sidecar means one process holds it, instead of
    every Python worker in the fleet.
  - No Rust toolchain, no wheel, no platform matrix on the calling side — just
    a binary and an HTTP client.
  - The exact same JSON the C ABI speaks, so a call site moves between the two
    modes as a transport change, not a rewrite.

Build the server first, from the workspace root:

    cargo build -p patala-sidecar
"""

from __future__ import annotations

import json
import os
import secrets
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
CHECKS = [0]


def check(condition: bool, message: str) -> None:
    CHECKS[0] += 1
    if not condition:
        sys.exit(f"FAILED: {message}")
    print(f"  ok  {message}")


def resolve_binary() -> str:
    """PATALA_SIDECAR_BIN, then a workspace build, then PATH."""
    env = os.environ.get("PATALA_SIDECAR_BIN")
    if env:
        return env
    for profile in ("debug", "release"):
        candidate = REPO / "target" / profile / "patala-sidecar"
        if candidate.is_file():
            return str(candidate)
    return "patala-sidecar"


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Sidecar:
    """Spawn `patala-sidecar`, wait for /healthz, terminate it on the way out."""

    def __init__(self) -> None:
        self.token = secrets.token_hex(32)
        self.port = free_port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.binary = resolve_binary()
        env = dict(os.environ, PATALA_SIDECAR_TOKEN=self.token,
                   PATALA_SIDECAR_PORT=str(self.port))
        try:
            self.proc = subprocess.Popen(  # noqa: S603
                [self.binary], env=env,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        except FileNotFoundError:
            sys.exit(
                f"could not run {self.binary!r}.\n"
                "Build it first: cargo build -p patala-sidecar"
            )

    def __enter__(self) -> "Sidecar":
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                sys.exit(f"patala-sidecar exited with {self.proc.returncode} before answering")
            try:
                with urllib.request.urlopen(f"{self.base}/healthz", timeout=0.5) as resp:
                    if resp.read() == b"ok":
                        return self
            except (urllib.error.URLError, OSError):
                time.sleep(0.05)
        self.__exit__(None, None, None)
        sys.exit("patala-sidecar never became healthy")

    def __exit__(self, *_exc: object) -> None:
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()

    def request(self, method: str, path: str, body: object = None,
                token: str | None = ...) -> tuple[int, object]:  # type: ignore[assignment]
        """One HTTP call. Returns (status, decoded-body-or-text).

        A 4xx/5xx is returned rather than raised: on this API a non-2xx is an
        answer with a body worth reading, not a transport failure.
        """
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(f"{self.base}{path}", data=data, method=method)
        if token is ...:
            token = self.token
        if token is not None:
            req.add_header("Authorization", f"Bearer {token}")
        if data is not None:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                return resp.status, _decode(resp.read())
        except urllib.error.HTTPError as exc:
            return exc.code, _decode(exc.read())


def _decode(raw: bytes) -> object:
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw.decode(errors="replace")


def main() -> int:
    with Sidecar() as sc:
        print(f"binary:   {sc.binary}")
        print(f"listening on {sc.base} (loopback only — the bind address is not configurable)")
        print(f"python:   {sys.version.split()[0]} on {sys.platform}\n")

        print("capabilities")
        status, caps = sc.request("GET", "/v1/rails/mock")
        check(status == 200, f"GET /v1/rails/mock -> {status}")
        check(
            caps["class"] == "NonCustodialFinal",
            f"class is {caps['class']!r} — decide the whole UX off this, not off a provider name",
        )
        check(caps["holds_funds"] is False, "holds_funds is false")

        print("\npre-flight: validate-destination, before any money moves")
        status, verdict = sc.request(
            "POST", "/v1/rails/mock/validate-destination",
            {"destination": "mock:wallet:alice"},
        )
        check(status == 200 and verdict["status"] == "StructurallyValid",
              f"a well-formed address -> 200 {verdict['status']!r}")
        check(verdict["is_refusal"] is False,
              "is_refusal is false — read the body, not just the status code")
        check(verdict["human_must_confirm"] is True,
              "human_must_confirm is true even on StructurallyValid")
        status, refused = sc.request(
            "POST", "/v1/rails/mock/validate-destination", {"destination": ""},
        )
        check(status == 200 and refused["status"] == "Malformed" and refused["is_refusal"],
              "an empty destination is a well-formed REQUEST -> 200 with a Malformed refusal")

        print("\nquote -> charge -> verify")
        pay = {"amount_minor": 1250, "currency": "USDC",
               "destination": "mock:wallet:alice", "reference": "order-1"}
        status, quote = sc.request("POST", "/v1/rails/mock/quote", pay)
        check(status == 200 and quote["total_minor"] == 1250,
              f"total_minor == {quote['total_minor']}")
        check(isinstance(quote["total_minor"], int),
              "the JSON number decodes to an int — minor units, never a float")

        status, receipt = sc.request("POST", "/v1/rails/mock/charge", pay)
        check(status == 200 and receipt["amount_minor"] == 1250,
              f"charge -> receipt for {receipt['amount_minor']} {receipt['currency']}")

        status, verdict = sc.request("POST", "/v1/rails/mock/verify", receipt)
        check(status == 200 and verdict == {"valid": True},
              "the genuine receipt verifies {'valid': true}")

        tampered = dict(receipt, amount_minor=receipt["amount_minor"] + 1)
        status, verdict = sc.request("POST", "/v1/rails/mock/verify", tampered)
        check(status == 200 and verdict == {"valid": False},
              "a tampered receipt is 200 {'valid': false} — fail-closed, and NOT an HTTP error")

        print("\nthe error surface, so you can tell these four apart")
        status, body = sc.request("POST", "/v1/rails/mock/charge", dict(pay, currency="EUR"))
        check(status == 400, f"an unsupported currency -> {status} {body['kind']!r}")
        status, body = sc.request("GET", "/v1/rails/nope")
        check(status == 404, f"an unknown rail_id -> {status} {body['kind']!r}")
        status, body = sc.request("POST", "/v1/rails/mock/webhook", {})
        check(status == 501,
              f"the mock has no push delivery -> {status} {body['kind']!r}, never an invented event")
        status, body = sc.request("GET", "/v1/rails/mock", token=None)
        check(status == 401, f"no Authorization header -> {status} on a READ-ONLY route too")

    print(f"\nsidecar terminated; nothing left running")
    print(f"\nALL {CHECKS[0]} PYTHON SIDECAR ASSERTIONS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
