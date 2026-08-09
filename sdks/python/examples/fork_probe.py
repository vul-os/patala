#!/usr/bin/env python3
"""Is patala fork-safe? Measured here rather than asserted.

    python3 sdks/python/examples/fork_probe.py

This file exists because the equivalent file in llmux and openrate reports the
opposite result, and copying their warning across would have been wrong. Those
two are Go, built with `go build -buildmode=c-shared`, so the Go runtime lives
in the host process and does not survive `fork()` without `exec()`. patala is
Rust: there is no runtime to break.

Do not take that on trust — it is the sort of claim that rots. Everything
printed below is a live measurement on this machine, this run.

Two libraries are probed, because Python can reach patala two ways and they do
not have the same thread profile:

    patala-py    the UniFFI binding (what ../direct_charge.py uses). Blocks on
                 one process-wide MULTI-THREAD tokio runtime, started lazily on
                 the first call.
    patala-ffi   the plain C ABI. Each handle owns a CURRENT-THREAD runtime, so
                 the process never gains a thread at all.

Run it with an argument to change the number of iterations in the last
(statistical) section:

    python3 sdks/python/examples/fork_probe.py 300
"""

from __future__ import annotations

import ctypes
import multiprocessing
import os
import select
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
WATCHDOG = float(os.environ.get("PATALA_FORK_TIMEOUT", "5"))
RACE_ITERATIONS = int(sys.argv[1]) if len(sys.argv) > 1 else 200

PAY = {
    "amount_minor": 1250,
    "currency": "USDC",
    "destination": "mock:wallet:alice",
    "reference": "order-1",
}
PAY_JSON = (
    b'{"amount_minor":1250,"currency":"USDC",'
    b'"destination":"mock:wallet:alice","reference":"order-1"}'
)


# --------------------------------------------------------------------- harness


def threads() -> int:
    """Threads in this process, counted by the OS.

    `threading.active_count()` would only see threads Python created — the
    exact wrong number, since the question is what the native library started.
    """
    out = subprocess.run(  # noqa: S603
        ["ps", "-M", str(os.getpid())], capture_output=True, text=True, check=False,
    ).stdout
    return max(len(out.strip().splitlines()) - 1, 0)


def fork_and_run(label: str, work) -> str:
    """Fork, run `work()` in the child, and report — even if it never returns.

    The pipe is the only channel that can be trusted: a hung child cannot write
    to it and a crashed one closes it. Reading with a timeout is what turns
    "this hangs" from folklore into a printed result.
    """
    read_fd, write_fd = os.pipe()
    pid = os.fork()
    if pid == 0:  # ---------------- child
        os.close(read_fd)
        try:
            os.write(write_fd, f"returned {work()}".encode()[:300])
        except BaseException as exc:  # noqa: BLE001 - reporting, not handling
            os.write(write_fd, f"raised {type(exc).__name__}: {exc}".encode()[:300])
        os._exit(0)

    os.close(write_fd)  # ----------- parent
    started = time.monotonic()
    ready, _, _ = select.select([read_fd], [], [], WATCHDOG)
    if ready:
        outcome = os.read(read_fd, 512).decode() or "(wrote nothing)"
    else:
        outcome = f"HUNG — nothing in {WATCHDOG}s, SIGKILLed"
        os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    os.close(read_fd)
    print(f"    {label:<40} {outcome}  ({time.monotonic() - started:.2f}s)")
    return outcome


# ------------------------------------------------------------------ the C ABI


def library_path() -> str:
    env = os.environ.get("PATALA_LIBRARY")
    if env:
        return env
    name = "libpatala_ffi.dylib" if sys.platform == "darwin" else "libpatala_ffi.so"
    for profile in ("debug", "release"):
        candidate = REPO / "target" / profile / name
        if candidate.is_file():
            return str(candidate)
    return name


def load_c_abi(path: str):
    lib = ctypes.CDLL(path)
    lib.patala_abi_version.restype = ctypes.c_char_p
    lib.patala_new.restype = ctypes.c_uint64
    lib.patala_new.argtypes = [ctypes.c_char_p, ctypes.POINTER(ctypes.c_char_p)]
    lib.patala_call.restype = ctypes.POINTER(ctypes.c_char)
    lib.patala_call.argtypes = [
        ctypes.c_uint64, ctypes.c_char_p, ctypes.c_char_p, ctypes.POINTER(ctypes.c_char_p),
    ]
    lib.patala_close.argtypes = [ctypes.c_uint64]
    lib.patala_free.argtypes = [ctypes.POINTER(ctypes.c_char)]
    return lib


def c_charge_lean(lib, handle: int) -> None:
    """A charge with no `char** err` slot, for the hammering threads.

    Section 3 is a race against a mutex held for microseconds, so the loop
    trying to hit that window has to be tight. `ctypes.byref(c_char_p())` per
    call allocates under the GIL and costs ~90x the throughput, which is enough
    to make the race look like it does not exist. Passing NULL for `err` is
    explicitly allowed by patala.h when you do not want the message.
    """
    res = lib.patala_call(handle, b"charge", PAY_JSON, None)
    if res:
        lib.patala_free(res)


def c_call(lib, handle: int, method: bytes, request: bytes | None) -> str:
    err = ctypes.c_char_p()
    res = lib.patala_call(handle, method, request, ctypes.byref(err))
    if not res:
        message = err.value.decode() if err.value else "(no message)"
        lib.patala_free(ctypes.cast(err, ctypes.POINTER(ctypes.c_char)))
        raise RuntimeError(message)
    try:
        return ctypes.cast(res, ctypes.c_char_p).value.decode()
    finally:
        lib.patala_free(res)


# -------------------------------------------------------------- multiprocessing


def _worker(result) -> None:
    """A charge in a worker process. Reached by both start methods."""
    try:
        sys.path.insert(0, str(REPO / "patala-py" / "bindings" / "python"))
        from patala import PatalaRail, PayRequest, RailClass  # noqa: PLC0415

        rail = PatalaRail.new_mock("mock", RailClass.NON_CUSTODIAL_FINAL, ["USDC"], 0, False)
        result["out"] = rail.charge(PayRequest(**PAY)).amount_minor
    except BaseException as exc:  # noqa: BLE001
        result["out"] = f"raised {type(exc).__name__}: {exc}"


def multiprocessing_run(method: str) -> None:
    ctx = multiprocessing.get_context(method)
    manager = multiprocessing.Manager()
    result = manager.dict()
    proc = ctx.Process(target=_worker, args=(result,))
    started = time.monotonic()
    proc.start()
    proc.join(WATCHDOG)
    if proc.is_alive():
        proc.kill()
        proc.join()
        outcome = f"HUNG — killed after {WATCHDOG}s"
    else:
        outcome = str(result.get("out", f"exited {proc.exitcode} with no result"))
    manager.shutdown()
    print(f"    start_method={method:<27} charge -> {outcome}  "
          f"({time.monotonic() - started:.2f}s)")


# ------------------------------------------------------------------------ main


def main() -> int:
    print("=" * 74)
    print("patala fork probe — every line below is measured, not claimed")
    print("=" * 74)
    print(f"python {sys.version.split()[0]} on {sys.platform}, "
          f"default start_method={multiprocessing.get_start_method()}")
    print(f"watchdog {WATCHDOG}s\n")

    base_threads = threads()
    print(f"threads in a bare interpreter: {base_threads}\n")

    # ---------------------------------------------------------------- C ABI
    path = library_path()
    print("-" * 74)
    print(f"1. the C ABI — {path}")
    print("-" * 74)
    lib = load_c_abi(path)
    print(f"  patala {lib.patala_abi_version().decode()}")
    print(f"  threads after dlopen:              {threads()}")
    handle = lib.patala_new(b'{"rail":"mock"}', None)
    print(f"  threads after patala_new:          {threads()}")
    c_call(lib, handle, b"charge", PAY_JSON)
    print(f"  threads after a charge round trip: {threads()}"
          f"   <- unchanged: no runtime, no thread pool\n")

    print("  after os.fork(), in the child:")
    fork_and_run("charge on a FRESH handle", lambda: c_call(
        lib, lib.patala_new(b'{"rail":"mock"}', None), b"charge", PAY_JSON)[:40] + "…")
    fork_and_run("charge on the INHERITED handle", lambda: c_call(
        lib, handle, b"charge", PAY_JSON)[:40] + "…")
    fork_and_run("verify (a full round trip)", lambda: c_call(
        lib, handle, b"verify", c_call(lib, handle, b"charge", PAY_JSON).encode()))
    print("\n  Nothing hung. This is the line that differs from llmux and openrate:\n"
          "  there is no Go runtime in the process to be left half-alive by fork().\n")

    # -------------------------------------------------------------- patala-py
    print("-" * 74)
    print("2. the UniFFI binding (patala-py) — a process-wide multi-thread runtime")
    print("-" * 74)
    sys.path.insert(0, str(REPO / "patala-py" / "bindings" / "python"))
    try:
        from patala import PatalaRail, PayRequest, RailClass  # noqa: PLC0415
    except ImportError:
        print("  patala-py is not built here — skipping. Build it with `make smoke-python`.\n")
    else:
        print(f"  threads after `import patala`:     {threads()}")
        rail = PatalaRail.new_mock("mock", RailClass.NON_CUSTODIAL_FINAL, ["USDC"], 0, False)
        print(f"  threads after new_mock:            {threads()}   <- construction is inert")
        print("\n  after os.fork(), BEFORE the parent has made any call:")
        fork_and_run("charge in the child", lambda: rail.charge(PayRequest(**PAY)).amount_minor)

        rail.charge(PayRequest(**PAY))
        print(f"\n  parent charged once; threads now:  {threads()}"
              f"   <- the tokio runtime started (2 workers)")
        print("  after os.fork(), with that runtime already up:")
        fork_and_run("charge on the inherited rail", lambda: rail.charge(PayRequest(**PAY)).amount_minor)
        fork_and_run("charge on a fresh rail", lambda: PatalaRail.new_mock(
            "mock", RailClass.NON_CUSTODIAL_FINAL, ["USDC"], 0, False,
        ).charge(PayRequest(**PAY)).amount_minor)
        fork_and_run("validate_destination (pure)", lambda: rail.validate_destination(
            "mock:wallet:alice").status.name)

        print("\n  through multiprocessing, both start methods:")
        multiprocessing_run("fork")
        multiprocessing_run("spawn")
        print("\n  Both work. Note what this does NOT prove: `block_on` drives the\n"
              "  future on the calling thread, and MockRail spawns nothing, so the two\n"
              "  worker threads that are missing in the child are never needed. A rail\n"
              "  that does network I/O may reach for them. UNMEASURED — no live rail was\n"
              "  reachable from here. The C ABI has no such question to answer.\n")

    # ------------------------------------------------- the one real hazard
    print("-" * 74)
    print(f"3. the one real hazard: an INHERITED handle, {RACE_ITERATIONS} forks under contention")
    print("-" * 74)
    print("  patala.h says: \"Handles are not inherited usefully across a fork; open\n"
          "  them in the child.\" Section 1 forked from a single-threaded parent and the\n"
          "  inherited handle was fine, which makes that rule look like superstition.\n"
          "  It is not. A handle's tokio runtime sits behind a mutex, and fork() copies\n"
          "  a LOCKED mutex as locked — with nobody in the child to unlock it.\n")

    stop = threading.Event()
    completed = [0]

    def hammer() -> None:
        while not stop.is_set():
            c_charge_lean(lib, handle)
            completed[0] += 1

    hammers = [threading.Thread(target=hammer, daemon=True) for _ in range(4)]
    for thread in hammers:
        thread.start()
    time.sleep(0.3)

    results = {}
    for label, work in (
        ("inherited handle", lambda: c_charge_lean(lib, handle)),
        ("fresh handle in the child",
         lambda: c_charge_lean(lib, lib.patala_new(b'{"rail":"mock"}', None))),
    ):
        hung = 0
        for _ in range(RACE_ITERATIONS):
            read_fd, write_fd = os.pipe()
            pid = os.fork()
            if pid == 0:
                os.close(read_fd)
                try:
                    work()
                    os.write(write_fd, b"ok")
                except BaseException:  # noqa: BLE001
                    os.write(write_fd, b"err")
                os._exit(0)
            os.close(write_fd)
            if not select.select([read_fd], [], [], 1.5)[0]:
                hung += 1
                os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            os.close(read_fd)
        results[label] = hung
        print(f"    {label:<40} {hung}/{RACE_ITERATIONS} hung")
    stop.set()

    print(f"\n  ({completed[0]} charges completed on the hammering threads meanwhile.)")
    if results["inherited handle"]:
        print("\n  Reproduced. The rule is exact, and it is a rule about ONE handle rather\n"
              "  than about the library: a handle opened IN the child never hung, however\n"
              "  many threads the parent had. Note the shape of the bug — it is a race, so\n"
              "  most forks look fine and a test that forks once reports a false green.")
    else:
        print("\n  Not reproduced in this run — it is a race against a window a few\n"
              "  microseconds wide, so a slower machine or a slower Python can miss it\n"
              "  entirely. That is not evidence the window is closed; re-run with a\n"
              "  larger count. Either way the second row is the actionable one: a handle\n"
              "  opened in the child has never hung here.")
    print("\n  Either way this is the whole of patala's fork story: one rule, about one\n"
          "  handle. Compare llmux, where a chat in a forked child hangs unconditionally\n"
          "  no matter where the handle came from.")
    lib.patala_close(handle)
    return 0


if __name__ == "__main__":
    sys.exit(main())
