// patala, direct — libpatala_ffi loaded into your Node process over the C ABI
// in patala-ffi/include/patala.h.
//
//   import { Rail } from "patala/direct";
//
//   using rail = Rail.open({ rail: "mock", currencies: ["USDC"] });
//   const receipt = rail.charge({ amount_minor: 1250, currency: "USDC",
//                                 destination: "mock:wallet:alice", reference: "order-1" });
//   rail.verify(receipt).valid;                       // true — THIS is the entitlement
//
// WHAT LOADING THIS COSTS YOUR PROCESS: measurably nothing. patala is Rust, so
// there is no language runtime in the host — no GC, no scheduler, no signal
// handlers replaced, no threads started. Measured from Node on this machine,
// 7 threads before dlopen and 7 after a full charge -> verify round trip; the
// Go-based C ABI in the sibling product openrate takes the same process from 7
// to 13. README.md has the table, and examples/direct.ts prints the count.
//
// The practical consequence, and it is a big one: **Node worker threads and
// koffi's off-thread `.async` path both work here and both let the process
// exit.** Neither does against llmux's or openrate's Go libraries. So unlike
// those two SDKs, this one is not condemned to be synchronous — see
// `callAsync` below and README.md's "Off the main thread".
//
// There is deliberately no streaming entry point, here or in the ABI: patala
// has no streaming operation. Nothing it does produces a sequence of chunks, so
// there is nothing to iterate. (llmux, which shares this ABI shape, does have
// `llmux_stream`. Do not go looking for patala's.)
//
// JSON in, JSON out — the same JSON patala-sidecar serves.

import * as fs from "fs";
import * as path from "path";
import type { KoffiFunc, LibraryHandle } from "koffi";

import type {
  CaveatResult,
  DestinationVerdict,
  IdResult,
  PayRequest,
  ProvidersResult,
  Quote,
  RailCapabilities,
  RailConfig,
  Receipt,
  ResultOf,
  VerifyResult,
  WebhookDelivery,
  WebhookEvent,
} from "./types.js";
import { narrowVerdict, narrowVerify } from "./types.js";

export * from "./types.js";

// ---------------------------------------------------------------------------
// koffi, loaded lazily
// ---------------------------------------------------------------------------
// koffi is an OPTIONAL dependency, so `require("patala")` — the sidecar path,
// which needs no native code at all — works without it. See README.md for why
// koffi and not a hand-written N-API addon.

/** Anything koffi hands back that is a raw C pointer. Never dereference it in JS. */
type CPtr = unknown;
/** koffi's `_Out_ void**` convention: a one-element array koffi writes into. */
type ErrOut = [CPtr];

interface Koffi {
  load(filename: string): LibraryHandle;
  decode(value: CPtr, type: string, len: number): unknown;
}

let _koffi: Koffi | null = null;
function koffi(): Koffi {
  if (_koffi) return _koffi;
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports -- optional dependency, resolved at first direct-mode use
    _koffi = require("koffi") as Koffi;
  } catch {
    throw new Error(
      'patala direct mode needs the optional "koffi" dependency: npm install koffi. ' +
        'Or use the sidecar — `require("patala").Sidecar.start()` — which needs no native FFI.'
    );
  }
  return _koffi;
}

// ---------------------------------------------------------------------------
// Finding libpatala_ffi
// ---------------------------------------------------------------------------

function libFileName(): string {
  // cargo names the cdylib libpatala_ffi.{dylib,so} / patala_ffi.dll.
  if (process.platform === "darwin") return "libpatala_ffi.dylib";
  if (process.platform === "win32") return "patala_ffi.dll";
  return "libpatala_ffi.so";
}

/**
 * Where the shared library will be loaded from.
 *
 * `PATALA_LIBRARY` wins. Otherwise a repo checkout's `target/release/` is
 * tried, then `target/debug/`, and failing both the bare name goes to the
 * system loader. Release first because that is the artifact whose size patala
 * advertises; if you built only a debug one, that is what you get.
 *
 * A stale library earlier on the load path is the classic way for a binding to
 * misbehave in ways that look like patala bugs, which is why the ABI carries
 * `patala_abi_check` — pass `expectVersion` to {@link Rail.open} and the
 * library itself does the comparison.
 *
 * **Built and executed here: darwin/arm64 only.** See README.md; nothing in
 * this package implies a Linux .so or a Windows .dll exists.
 */
export function resolveLibrary(explicit?: string): string {
  if (explicit) return explicit;
  if (process.env.PATALA_LIBRARY) return process.env.PATALA_LIBRARY;
  const root = path.join(__dirname, "..", "..");
  for (const profile of ["release", "debug"]) {
    const candidate = path.join(root, "target", profile, libFileName());
    if (fs.existsSync(candidate)) return candidate;
  }
  return libFileName();
}

// ---------------------------------------------------------------------------
// The six symbols
// ---------------------------------------------------------------------------

/**
 * koffi decorates every bound function with an off-thread `.async` variant that
 * takes a Node-style callback. Its own typings do not model it, so this does.
 */
type AsyncCapable<Args extends unknown[], R> = KoffiFunc<(...args: Args) => R> & {
  async(...args: [...Args, (err: Error | null, value: R) => void]): void;
};

interface Binding {
  path: string;
  abi_version: KoffiFunc<() => string>;
  abi_check: KoffiFunc<(expected: string, err: ErrOut) => number>;
  new_: KoffiFunc<(config: string | null, err: ErrOut) => number>;
  call: AsyncCapable<[h: number, method: string, req: string | null, err: ErrOut], CPtr>;
  close: KoffiFunc<(h: number) => void>;
  free: KoffiFunc<(p: CPtr) => void>;
}

const _bindings = new Map<string, Binding>();

function bind(libPath: string): Binding {
  const cached = _bindings.get(libPath);
  if (cached) return cached;
  const lib = koffi().load(libPath);
  const b: Binding = {
    path: libPath,
    // `const char*`: koffi decodes it and does not free it. Correct here and
    // only here — patala_abi_version returns a static string the library owns.
    abi_version: lib.func("const char *patala_abi_version()"),
    abi_check: lib.func("int patala_abi_check(const char *expected, _Out_ void **err)"),
    new_: lib.func("uint64_t patala_new(const char *config_json, _Out_ void **err)"),
    // Declared `void*`, not `char*`, ON PURPOSE: koffi would decode a `char*`
    // result into a JS string and drop the pointer, leaving nothing to hand
    // patala_free. That is the leak this line prevents.
    call: lib.func(
      "void *patala_call(uint64_t h, const char *method, const char *request_json, _Out_ void **err)"
    ) as Binding["call"],
    close: lib.func("void patala_close(uint64_t h)"),
    free: lib.func("void patala_free(void *p)"),
  };
  _bindings.set(libPath, b);
  return b;
}

/** The patala version the loaded shared library was built from. */
export function abiVersion(libraryPath?: string): string {
  return bind(resolveLibrary(libraryPath)).abi_version();
}

/**
 * Ask the library whether it is the version you generated against, and throw
 * its own message if not.
 *
 * This is `patala_abi_check`, not a comparison written here. The ABI exports it
 * precisely so twelve bindings do not each reimplement — and each forget — the
 * check.
 */
export function abiCheck(expected: string, libraryPath?: string): void {
  const b = bind(resolveLibrary(libraryPath));
  const err: ErrOut = [null];
  if (b.abi_check(expected, err) !== 0) throw takeError(b, err, `patala_abi_check(${expected}) failed`);
}

/** Read a C string patala allocated, then free it. Freeing is not optional. */
function takeString(b: Binding, p: CPtr): string | null {
  if (!p) return null;
  try {
    return koffi().decode(p, "char", -1) as string;
  } finally {
    b.free(p);
  }
}

/** Turn a populated `char** err` into an Error, freeing the message. */
function takeError(b: Binding, err: ErrOut, fallback: string): Error {
  // Error strings are plain UTF-8 text, NOT JSON. Do not parse them.
  const msg = takeString(b, err[0]);
  err[0] = null;
  return new Error(msg ?? fallback);
}

function encodeRequest(request: unknown): string | null {
  if (request == null) return null;
  return typeof request === "string" ? request : JSON.stringify(request);
}

// ---------------------------------------------------------------------------
// Rail
// ---------------------------------------------------------------------------

export interface RailOptions {
  /** Override the shared library path (otherwise {@link resolveLibrary}). */
  libraryPath?: string;
  /**
   * Refuse to open unless the loaded library reports this version. Checked by
   * `patala_abi_check` inside the library, not by a comparison here.
   */
  expectVersion?: string;
}

/**
 * One rail, behind one handle.
 *
 * Opening a rail talks to nothing: no socket, no thread, no file. Only
 * {@link call} reaches a network, and only for a rail that has one — the
 * default `mock` rail has none at all, which is why every example in this
 * package uses it. This is a payments library; an example that moves real
 * value is not an example.
 *
 * Disposable, so `using rail = Rail.open()` closes the handle on every exit
 * path out of the block, including a throw.
 */
export class Rail implements Disposable {
  private readonly b: Binding;
  private h: number;
  private closed = false;

  private constructor(b: Binding, h: number) {
    this.b = b;
    this.h = h;
  }

  /**
   * Build a rail. `undefined` means the offline default — a deterministic
   * `MockRail` on USDC, needing no credentials and no network.
   */
  static open(config?: RailConfig, opts: RailOptions = {}): Rail {
    const libPath = resolveLibrary(opts.libraryPath);
    const b = bind(libPath);
    if (opts.expectVersion !== undefined) abiCheck(opts.expectVersion, libPath);
    const err: ErrOut = [null];
    const h = b.new_(config === undefined ? null : JSON.stringify(config), err);
    // 0 is patala_new's failure sentinel, because its success value is a
    // handle and handles start at 1.
    if (h === 0) throw takeError(b, err, "patala_new failed");
    return new Rail(b, h);
  }

  /** The registry key of this rail. Handles are retired, never recycled. */
  get handle(): number {
    return this.h;
  }

  private live(): number {
    if (this.closed) throw new Error("patala rail is closed");
    return this.h;
  }

  /**
   * Any method, typed by name. The nine are `id`, `capabilities`, `quote`,
   * `charge`, `verify`, `validate-destination`, `webhook`, `caveat` and
   * `providers`; the named methods below are thin wrappers over this.
   */
  call<M extends keyof ResultOf>(method: M, request?: unknown): ResultOf[M] {
    const h = this.live();
    const err: ErrOut = [null];
    const res = this.b.call(h, method, encodeRequest(request), err);
    if (!res) throw takeError(this.b, err, `patala_call(${method}) failed`);
    return JSON.parse(takeString(this.b, res) ?? "null") as ResultOf[M];
  }

  /**
   * The same call, run on a libuv threadpool thread instead of this one.
   *
   * Pointless for the mock rail, which answers in microseconds. Not pointless
   * for a real one: `charge` on Solana, Stellar or a fiat processor is a
   * network round trip, and a synchronous FFI call would block Node's event
   * loop for its whole duration.
   *
   * **That this works at all is patala-specific.** The identical construction
   * against llmux's or openrate's Go libraries returns the right answer and
   * then hangs the process forever, which is why both of those SDKs are
   * synchronous-only. patala has no runtime in the host, so there is no thread
   * stuck inside one at exit. Verified, with the failing control, in README.md.
   */
  async callAsync<M extends keyof ResultOf>(method: M, request?: unknown): Promise<ResultOf[M]> {
    const h = this.live();
    const err: ErrOut = [null];
    const body = encodeRequest(request);
    const res = await new Promise<CPtr>((resolve, reject) => {
      this.b.call.async(h, method, body, err, (e: Error | null, value: CPtr) => {
        if (e) reject(e);
        else resolve(value);
      });
    });
    if (!res) throw takeError(this.b, err, `patala_call(${method}) failed`);
    return JSON.parse(takeString(this.b, res) ?? "null") as ResultOf[M];
  }

  /** This rail's stable id — `"mock"`, `"solana"`, … */
  id(): IdResult {
    return this.call("id");
  }

  /**
   * What this rail is and is not able to do. Decide your whole UX from
   * `class` without knowing which provider answered.
   */
  capabilities(): RailCapabilities {
    return this.call("capabilities");
  }

  /** Fees, fx and expiry. Moves no money. */
  quote(req: PayRequest): Quote {
    return this.call("quote", req);
  }

  /**
   * Move money and return the receipt. **Store the receipt** — it, not this
   * call returning, is the entitlement.
   */
  charge(req: PayRequest): Receipt {
    return this.call("charge", req);
  }

  /**
   * Re-derive whether a receipt still holds. `{valid: false}` is a RESULT, not
   * an error: it is the rail's fail-closed answer. Gate on `valid === true` and
   * nothing else, and never retry a `false` as though it were transient.
   */
  verify(receipt: Receipt): VerifyResult {
    return narrowVerify(this.call("verify", receipt));
  }

  /**
   * The offline pre-flight check to run before any money moves. It never
   * fails — "I cannot check this" comes back as `status: "Unknown"`. Read
   * `is_refusal` (do not send) and `human_must_confirm`, which is `true` on
   * every verdict including `StructurallyValid`.
   */
  validateDestination(destination: string): DestinationVerdict {
    return narrowVerdict(this.call("validate-destination", { destination }));
  }

  /**
   * Authenticate an inbound delivery from a rail's processor. Forward it
   * VERBATIM: every scheme signs the exact bytes that were sent, so a body
   * that has been through a JSON round-trip on your side no longer matches its
   * own signature.
   *
   * A rail with no push delivery — the mock, fiat's `manual` — refuses rather
   * than inventing an event.
   */
  webhook(delivery: WebhookDelivery): WebhookEvent {
    return this.call("webhook", delivery);
  }

  /** The sentence to show a human before they confirm a payout address. */
  caveat(): CaveatResult {
    return this.call("caveat");
  }

  /** Every fiat provider this build has compiled in. Fiat builds only. */
  providers(): ProvidersResult {
    return this.call("providers");
  }

  /**
   * Release the rail. Idempotent, as `patala_close` is — closing an unknown or
   * already-closed handle is a no-op, so a cleanup path can be unconditional.
   */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.b.close(this.h);
  }

  [Symbol.dispose](): void {
    this.close();
  }
}
