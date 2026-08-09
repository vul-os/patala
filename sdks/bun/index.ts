/**
 * patala for Bun — both modes in one module, no runtime dependencies.
 *
 * DIRECT (in-process, the C ABI in `patala-ffi/include/patala.h`):
 *
 * ```ts
 * import { Rail } from "./index.ts";
 *
 * using rail = Rail.open({ rail: "mock", currencies: ["USDC"] });
 * const receipt = rail.charge({
 *   amount_minor: 1250, currency: "USDC",
 *   destination: "mock:wallet:alice", reference: "order-1",
 * });
 * rail.verify(receipt).valid;                  // true — THIS is the entitlement
 * ```
 *
 * SIDECAR (out-of-process, the `patala-sidecar` binary over loopback):
 *
 * ```ts
 * await using side = await Sidecar.start();
 * const receipt = await side.charge({ … });
 * (await side.verify(receipt)).valid;
 * ```
 *
 * WHAT DIRECT MODE COSTS THIS PROCESS: measurably nothing. patala is Rust, so
 * there is no language runtime in the host — no GC, no scheduler, no signal
 * handlers replaced, no threads started, and an 844,656-byte library. If you
 * have read the equivalent module for llmux or openrate, those carry a list of
 * Go-runtime caveats that are true there and false here; they have deliberately
 * not been copied. README.md has the measurements.
 *
 * `bun:ffi` has no asynchronous call mode — no `nonblocking` like Deno's, no
 * threadpool variant like koffi's — so every direct call runs on the thread
 * that made it. On the mock rail that is microseconds and does not matter. On a
 * real rail `charge` is a network round trip, and README.md's "Off the main
 * thread" says what to do about it: a Bun `Worker`, which was measured
 * terminating cleanly with this library loaded.
 *
 * There is deliberately NO STREAMING, here or in the ABI: patala has no
 * streaming operation. Nothing it does produces a sequence of chunks, so there
 * is nothing to iterate — no `patala_stream`, and no async iterator on `Rail`.
 * (llmux, which shares this ABI shape, does have `llmux_stream`. Do not go
 * looking for patala's.)
 *
 * JSON in, JSON out — the same JSON `patala-sidecar` serves.
 *
 * EVERY EXAMPLE IN THIS PACKAGE USES MockRail. patala is a payments library and
 * an example that moves real value is not an example.
 *
 * @module
 */

import { CString, dlopen, FFIType, ptr } from "bun:ffi";
import type { Pointer } from "bun:ffi";

// ===========================================================================
// The JSON patala speaks
// ===========================================================================
// These are the `patala-core` types verbatim — the same shapes both transports
// carry, so neither gets to drift from the other.
//
// MONEY IS INTEGER MINOR UNITS. Every `*_minor` is a whole number of the
// currency's smallest unit, never a float. `number` appears below only because
// JSON has one numeric type.

/**
 * Custodial-and-reversible, or non-custodial-and-final. **Not a bool**, on
 * purpose: the difference decides your whole UX. `CustodialReversible` wants a
 * card form and a refundable pending state; `NonCustodialFinal` wants a wallet
 * address and a signed, irreversible receipt.
 */
export type RailClass = "CustodialReversible" | "NonCustodialFinal";

/** Final at broadcast, after N seconds, or after N days (card-network T+2). */
export type Settlement = "Instant" | { Seconds: number } | { Days: number };

/** What a rail can and cannot do. Decide your UX from `class`. */
export interface RailCapabilities {
  class: RailClass;
  reversible: boolean;
  requires_kyc: boolean;
  /** patala itself never holds funds; this says whether the *rail* does. */
  holds_funds: boolean;
  currencies: string[];
  settlement: Settlement;
  atomic_multi_party: boolean;
}

/** A request to move money. `reference` is your idempotency/correlation key. */
export interface PayRequest {
  /** Integer minor units. Never a float. */
  amount_minor: number;
  currency: string;
  /** A wallet address on a crypto rail; an opaque token on a fiat one. */
  destination: string;
  reference: string;
}

/** Fees, fx and expiry. No money moves. */
export interface Quote {
  rail_id: string;
  amount_minor: number;
  currency: string;
  fee_minor: number;
  total_minor: number;
  settlement: Settlement;
  expires_at_unix: number;
}

/**
 * Proof that a charge executed — **this is the entitlement**, and it is the
 * thing to store. Gate on `verify` returning `{valid: true}` later, never on
 * `charge` having returned without throwing.
 */
export interface Receipt {
  rail_id: string;
  amount_minor: number;
  currency: string;
  reference: string;
  /** Rail-specific binding blob, as JSON's only byte encoding: a number array. */
  proof: number[];
  settled_at_unix: number;
}

/**
 * `{valid: false}` is an ANSWER, not a failure — the rail's fail-closed verdict
 * that a receipt does not hold. Never retry it as though it were transient.
 */
export interface VerifyResult {
  valid: boolean;
}

/** `{rail_id}` — what `id` returns. */
export interface IdResult {
  rail_id: string;
}

/** **No status here means "safe to send to".** */
export type DestinationStatus =
  /** Positively defective — wrong alphabet, bad checksum, empty. Do not send. */
  | "Malformed"
  /** Well-formed, wrong network. Common and expensive. Do not send. */
  | "WrongNetwork"
  /** A real address nobody holds a key for — a contract, a PDA, a mint. */
  | "NotAWallet"
  /** Every offline check passed. That is the absence of a defect, not safety. */
  | "StructurallyValid"
  /** This rail cannot check it and refuses to guess. A human must decide. */
  | "Unknown";

/**
 * What one rail could decide about one address, offline, plus what it could
 * not. This call NEVER fails — "I cannot check this" arrives as
 * `status: "Unknown"`, because an error is too easy to swallow.
 */
export interface DestinationVerdict {
  rail_id: string;
  status: DestinationStatus;
  reason: string;
  /** `true` on EVERY verdict, including `StructurallyValid`. */
  human_must_confirm: boolean;
  exchange_deposit_caveat: string;
  /**
   * Do not send. Derived by patala rather than by you: a `switch` that has not
   * heard of a status added later falls through to "not a refusal", which fails
   * OPEN on the one question that decides where a customer's money goes.
   */
  is_refusal: boolean;
}

/** `{exchange_deposit_caveat}` — the same sentence, with no verdict attached. */
export interface CaveatResult {
  exchange_deposit_caveat: string;
}

/** `{providers}` — fiat builds only; a default build refuses by feature name. */
export interface ProvidersResult {
  providers: string[];
}

/**
 * One inbound webhook delivery, forwarded VERBATIM. Every scheme signs the
 * bytes the processor actually sent, so a body that has been through a JSON
 * round-trip on your side is no longer what was signed. Give exactly one of
 * `body` / `body_hex`.
 */
export interface WebhookDelivery {
  body?: string;
  body_hex?: string;
  /** Matched case-insensitively. */
  headers?: Record<string, string>;
  /** Only schemes that put their secret in the URL (LNbits) read this. */
  query?: Record<string, string>;
  /** An explicit clock for replay windows, so a delivery can be reproduced. */
  now_unix: number;
}

/** An AUTHENTICATED delivery. A forged one is an error, never an event. */
export interface WebhookEvent {
  rail_id: string;
  event_id: string;
  reference: string;
  object_id: string;
  /** Gate entitlement on `"Settled"` and nothing else. */
  status: "Settled" | "NotSettled" | "Unconfirmed";
  /** Processor-reported. Reconcile against your own order before trusting it. */
  amount_minor: number;
  currency: string;
}

/**
 * The `{"rail": …}` document `patala_new` takes. Unknown fields are REFUSED,
 * not ignored: a misspelled `"currencys"` is an error rather than a rail
 * quietly built with a currency list you did not choose.
 */
export type RailConfig =
  | {
    rail: "mock";
    id?: string;
    class?: "non-custodial-final" | "custodial-reversible";
    currencies?: string[];
    fee_minor?: number;
    /** Make every operation fail, for exercising your error path. */
    failing?: boolean;
    /** `false` answers "Unknown" to every destination — a fiat rail's shape. */
    destination_checks?: boolean;
  }
  | { rail: "fiat"; provider: string; config?: Record<string, string> }
  | { rail: "solana"; rpc_url: string; cluster: "devnet" | "mainnet"; keypair_seed_hex?: string }
  | {
    rail: "stellar";
    horizon_url: string;
    network: "testnet" | "public";
    usdc_issuer?: string;
    keypair_seed_hex?: string;
  }
  | {
    rail: "hyperswitch";
    base_url: string;
    api_key: string;
    connector?: string;
    webhook_secret?: string;
    requires_kyc?: boolean;
    currencies?: string[];
    settlement_days?: number;
    timeout_secs?: number;
  };

/** Method name -> the shape that method answers with. */
export interface ResultOf {
  id: IdResult;
  capabilities: RailCapabilities;
  quote: Quote;
  charge: Receipt;
  verify: VerifyResult;
  "validate-destination": DestinationVerdict;
  webhook: WebhookEvent;
  caveat: CaveatResult;
  providers: ProvidersResult;
}

/** Every method `patala_call` accepts. A closed set — see `patala.h`. */
export const METHODS: readonly (keyof ResultOf)[] = [
  "id",
  "capabilities",
  "quote",
  "charge",
  "verify",
  "validate-destination",
  "webhook",
  "caveat",
  "providers",
];


// ===========================================================================
// DIRECT — libpatala_ffi over bun:ffi
// ===========================================================================

function libFileName(): string {
  // cargo names the cdylib libpatala_ffi.{dylib,so} / patala_ffi.dll.
  if (process.platform === "darwin") return "libpatala_ffi.dylib";
  if (process.platform === "win32") return "patala_ffi.dll";
  return "libpatala_ffi.so";
}

/**
 * Where the shared library will be loaded from: `PATALA_LIBRARY`, else a repo
 * checkout's `target/release/`, else its `target/debug/`, else the bare name
 * for the system loader.
 *
 * Release first because that is the artifact whose size patala advertises; if
 * you built only a debug one, that is what you get. A stale library earlier on
 * the load path is the classic way for a binding to misbehave in ways that look
 * like patala bugs, which is why the ABI carries `patala_abi_check` — pass
 * `expectVersion` to {@link Rail.open} and the library does the comparison.
 *
 * **Built and executed here: darwin/arm64 only.** Nothing in this module
 * implies a Linux `.so` or a Windows `.dll` exists — see README.md.
 */
export function resolveLibrary(explicit?: string): string {
  if (explicit) return explicit;
  if (process.env.PATALA_LIBRARY) return process.env.PATALA_LIBRARY;
  let first: string | undefined;
  for (const profile of ["release", "debug"]) {
    const candidate = new URL(`../../target/${profile}/${libFileName()}`, import.meta.url).pathname;
    first ??= candidate;
    if (Bun.file(candidate).size > 0) return candidate;
  }
  return first ?? libFileName();
}

const SYMBOLS = {
  patala_abi_version: { args: [], returns: FFIType.cstring },
  patala_abi_check: { args: [FFIType.cstring, FFIType.ptr], returns: FFIType.i32 },
  patala_new: { args: [FFIType.cstring, FFIType.ptr], returns: FFIType.u64 },
  patala_call: {
    args: [FFIType.u64, FFIType.cstring, FFIType.cstring, FFIType.ptr],
    // FFIType.ptr, NOT cstring: bun would decode a cstring result into a JS
    // string and drop the pointer, leaving nothing to hand patala_free.
    returns: FFIType.ptr,
  },
  patala_close: { args: [FFIType.u64], returns: FFIType.void },
  patala_free: { args: [FFIType.ptr], returns: FFIType.void },
} as const;

type Lib = ReturnType<typeof dlopen<typeof SYMBOLS>>;

const _libs = new Map<string, Lib>();

function load(libPath: string): Lib {
  const cached = _libs.get(libPath);
  if (cached) return cached;
  const lib = dlopen(libPath, SYMBOLS);
  _libs.set(libPath, lib);
  return lib;
}

const encoder = new TextEncoder();
const cstr = (s: string) => encoder.encode(s + "\0");

/**
 * A raw address read out of a `char**` slot, as bun's branded Pointer. bun:ffi
 * models pointers as an opaque number type; a value that came back through a
 * BigUint64Array has lost that brand and has to be given it back.
 */
function asPointer(address: bigint): Pointer | null {
  return address === 0n ? null : (Number(address) as unknown as Pointer);
}

/** Read a C string patala allocated, then free it. Freeing is not optional. */
function takeString(lib: Lib, p: Pointer | null): string | null {
  if (!p) return null;
  try {
    return new CString(p).toString();
  } finally {
    lib.symbols.patala_free(p);
  }
}

/** Turn a populated `char** err` into an Error, freeing the message. */
function takeError(lib: Lib, slot: BigUint64Array, fallback: string): Error {
  // Error strings are plain UTF-8 text, NOT JSON. Do not parse them.
  const msg = takeString(lib, asPointer(slot[0] ?? 0n));
  slot[0] = 0n;
  return new Error(msg ?? fallback);
}

/** The patala version the loaded shared library was built from. */
export function abiVersion(libraryPath?: string): string {
  // Declared FFIType.cstring: bun decodes it and does not free it, which is
  // correct here and only here — it returns a static string the library owns.
  return String(load(resolveLibrary(libraryPath)).symbols.patala_abi_version());
}

/**
 * Ask the library whether it is the version you generated against, and throw
 * its own message if not.
 *
 * This is `patala_abi_check`, not a comparison written here. The ABI exports it
 * precisely so twelve bindings do not each reimplement — and each forget — it.
 */
export function abiCheck(expected: string, libraryPath?: string): void {
  const lib = load(resolveLibrary(libraryPath));
  const err = new BigUint64Array(1);
  if (lib.symbols.patala_abi_check(cstr(expected), ptr(err)) !== 0) {
    throw takeError(lib, err, `patala_abi_check(${expected}) failed`);
  }
}

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
 * Opening a rail talks to nothing: no socket, no thread, no file. Only a call
 * reaches a network, and only for a rail that has one — the default `mock` rail
 * has none at all.
 *
 * Disposable, so `using rail = Rail.open()` closes the handle on every exit
 * path out of the block, including a throw.
 */
export class Rail implements Disposable {
  readonly #lib: Lib;
  readonly #h: bigint;
  #closed = false;

  private constructor(lib: Lib, h: bigint) {
    this.#lib = lib;
    this.#h = h;
  }

  /**
   * Build a rail. `undefined` means the offline default — a deterministic
   * `MockRail` on USDC, needing no credentials and no network.
   */
  static open(config?: RailConfig, opts: RailOptions = {}): Rail {
    const libPath = resolveLibrary(opts.libraryPath);
    const lib = load(libPath);
    if (opts.expectVersion !== undefined) abiCheck(opts.expectVersion, libPath);
    const err = new BigUint64Array(1);
    const h = lib.symbols.patala_new(config === undefined ? null : cstr(JSON.stringify(config)), ptr(err));
    // 0 is patala_new's failure sentinel, because its success value is a handle
    // and handles start at 1.
    if (h === 0n) throw takeError(lib, err, "patala_new failed");
    return new Rail(lib, h);
  }

  /** The registry key of this rail. Handles are retired, never recycled. */
  get handle(): bigint {
    return this.#h;
  }

  #live(): bigint {
    if (this.#closed) throw new Error("patala rail is closed");
    return this.#h;
  }

  /**
   * Any method, typed by name. The nine are `id`, `capabilities`, `quote`,
   * `charge`, `verify`, `validate-destination`, `webhook`, `caveat` and
   * `providers`; the named methods below are thin wrappers over this.
   *
   * There is no `callAsync` here, and its absence is bun's rather than
   * patala's: `bun:ffi` has no `nonblocking` option the way `Deno.dlopen` does
   * and no threadpool variant the way koffi does. See README.md.
   */
  call<M extends keyof ResultOf>(method: M, request?: unknown): ResultOf[M] {
    const h = this.#live();
    const err = new BigUint64Array(1);
    const body = request == null
      ? null
      : cstr(typeof request === "string" ? request : JSON.stringify(request));
    const res = this.#lib.symbols.patala_call(h, cstr(method), body, ptr(err));
    if (!res) throw takeError(this.#lib, err, `patala_call(${method}) failed`);
    return JSON.parse(takeString(this.#lib, res) ?? "null") as ResultOf[M];
  }

  /** This rail's stable id — `"mock"`, `"solana"`, … */
  id(): IdResult {
    return this.call("id");
  }

  /**
   * What this rail is and is not able to do. Decide your whole UX from `class`
   * without knowing which provider answered.
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
   * an error. Gate on `valid === true` and nothing else, and never retry a
   * `false` as though it were transient.
   */
  verify(receipt: Receipt): VerifyResult {
    return this.call("verify", receipt);
  }

  /**
   * The offline pre-flight check to run before any money moves. It never
   * fails — "I cannot check this" comes back as `status: "Unknown"`. Read
   * `is_refusal` (do not send) and `human_must_confirm`, which is `true` on
   * every verdict including `StructurallyValid`.
   */
  validateDestination(destination: string): DestinationVerdict {
    return this.call("validate-destination", { destination });
  }

  /**
   * Authenticate an inbound delivery. Forward it VERBATIM: every scheme signs
   * the exact bytes that were sent, so a body that has been through a JSON
   * round-trip on your side no longer matches its own signature.
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
    if (this.#closed) return;
    this.#closed = true;
    this.#lib.symbols.patala_close(this.#h);
  }

  [Symbol.dispose](): void {
    this.close();
  }
}

// ===========================================================================
// SIDECAR — the patala-sidecar binary over loopback
// ===========================================================================

export interface SidecarOptions {
  /** Fixed port; default is an ephemeral free port on 127.0.0.1. */
  port?: number;
  /**
   * The bearer token. Default: 32 fresh random bytes, hex. Supply your own only
   * to share a sidecar with something else that already knows it.
   */
  token?: string;
  /** Extra environment for the child; overrides what {@link Sidecar.start} sets. */
  env?: Record<string, string>;
  /** How long to wait for the child to start LISTENING, ms (default 10000). */
  timeoutMs?: number;
  /** Binary to run; default `PATALA_SIDECAR_BINARY`, else `patala-sidecar` on PATH. */
  binary?: string;
  /** Where the child's stdout/stderr go (default "inherit"). */
  stdio?: "inherit" | "ignore";
}

/**
 * A non-2xx answer from the sidecar, with the status and the `kind`
 * discriminant from its error body.
 *
 * `kind` is worth branching on rather than the status: `"unsupported"` (501) is
 * a rail honestly declining an operation it cannot do — the mock rail's answer
 * to `webhook` — and is a different thing from `"rail_error"` (502), which is
 * an operational failure worth retrying.
 */
export class SidecarHttpError extends Error {
  readonly status: number;
  readonly kind: string;
  constructor(status: number, kind: string, message: string) {
    super(message);
    this.name = "SidecarHttpError";
    this.status = status;
    this.kind = kind;
  }
}

function freePort(): number {
  // Bind :0, read what the OS gave us, close. A tiny race against another
  // process, and the alternative — a fixed port — races every other run.
  const server = Bun.listen({ hostname: "127.0.0.1", port: 0, socket: { data() {} } });
  const { port } = server;
  server.stop(true);
  return port;
}

function randomToken(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

const seconds = (ms: number): string => `${Math.round(ms / 100) / 10}s`;

/**
 * A running `patala-sidecar`, owned by this process.
 *
 * `start()` mints a token, picks a free loopback port, launches the binary and
 * polls `/healthz` until it answers. There is no second readiness question:
 * `/healthz` answering means the router is up, and the mock rail needs no
 * warm-up.
 *
 * The token goes in the child's ENVIRONMENT, never in argv — argv is
 * world-readable through `ps`, and this token authorises `charge`. The sidecar
 * refuses to start at all without one: no unauthenticated mode, no generated
 * fallback.
 */
export class Sidecar implements AsyncDisposable {
  readonly baseURL: string;
  /** The bearer token this sidecar was started with. */
  readonly token: string;
  /** The only rail id the sidecar's registry currently has. */
  readonly railId = "mock";
  #child: Bun.Subprocess;
  #stopped = false;

  private constructor(child: Bun.Subprocess, baseURL: string, token: string) {
    this.#child = child;
    this.baseURL = baseURL;
    this.token = token;
  }

  static async start(opts: SidecarOptions = {}): Promise<Sidecar> {
    const port = opts.port ?? freePort();
    const binary = opts.binary ?? process.env.PATALA_SIDECAR_BINARY ?? "patala-sidecar";
    const token = opts.token ?? randomToken();
    const stdio = opts.stdio ?? "inherit";

    const child = Bun.spawn([binary], {
      env: {
        ...process.env,
        PATALA_SIDECAR_TOKEN: token,
        PATALA_SIDECAR_PORT: String(port),
        ...opts.env,
      },
      stdin: "ignore",
      stdout: stdio,
      stderr: stdio,
    });

    const base = `http://127.0.0.1:${port}`;
    const deadline = Date.now() + (opts.timeoutMs ?? 10_000);
    for (;;) {
      // The sidecar EXITS 1 when PATALA_SIDECAR_TOKEN is missing or empty, so a
      // child that is gone is a real outcome to report, not just a slow start.
      if (child.exitCode !== null) {
        throw new Error(
          `patala-sidecar exited with code ${child.exitCode} before it listened on ${base} ` +
            "(it refuses to start without PATALA_SIDECAR_TOKEN; check the output above)",
        );
      }
      try {
        // /healthz is the one unauthenticated route and reveals nothing about
        // which rails are configured. Everything else is behind the token.
        const res = await fetch(base + "/healthz");
        await res.text();
        if (res.status === 200) return new Sidecar(child, base, token);
      } catch {
        // not listening yet
      }
      if (Date.now() > deadline) {
        child.kill();
        throw new Error(
          `patala-sidecar did not start listening on ${base} within ${seconds(opts.timeoutMs ?? 10_000)}`,
        );
      }
      await Bun.sleep(50);
    }
  }

  async #request(method: "GET" | "POST", pathname: string, body?: unknown): Promise<unknown> {
    const res = await fetch(this.baseURL + pathname, {
      method,
      headers: {
        Authorization: `Bearer ${this.token}`,
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    const text = await res.text();
    if (!res.ok) {
      // The sidecar's error body is {"error", "kind"}. A 401 from the auth
      // middleware is deliberately detail-free — missing, malformed and wrong
      // tokens are indistinguishable — so fall back to the status line.
      let kind = "http";
      let message = text.trim().slice(0, 300);
      try {
        const parsed = JSON.parse(text) as { error?: string; kind?: string };
        if (parsed.kind) kind = parsed.kind;
        if (parsed.error) message = parsed.error;
      } catch {
        if (!message) message = res.statusText;
      }
      throw new SidecarHttpError(res.status, kind, `patala-sidecar: HTTP ${res.status}: ${message}`);
    }
    return JSON.parse(text) as unknown;
  }

  /** Liveness. The one route that needs no token. */
  async healthz(): Promise<string> {
    const res = await fetch(this.baseURL + "/healthz");
    return (await res.text()).trim();
  }

  /** `GET /v1/rails/:rail_id`. */
  async capabilities(railId: string = this.railId): Promise<RailCapabilities> {
    return (await this.#request("GET", `/v1/rails/${encodeURIComponent(railId)}`)) as RailCapabilities;
  }

  /** `POST /v1/rails/:rail_id/quote`. Moves no money. */
  async quote(req: PayRequest, railId: string = this.railId): Promise<Quote> {
    return (await this.#request("POST", `/v1/rails/${encodeURIComponent(railId)}/quote`, req)) as Quote;
  }

  /** `POST /v1/rails/:rail_id/charge`. Store the receipt it returns. */
  async charge(req: PayRequest, railId: string = this.railId): Promise<Receipt> {
    return (await this.#request("POST", `/v1/rails/${encodeURIComponent(railId)}/charge`, req)) as Receipt;
  }

  /**
   * `POST /v1/rails/:rail_id/verify`. A `200` with `{valid: false}` is the
   * honest answer, never an HTTP error — so "verified false" can never be
   * mistaken for "the sidecar broke".
   */
  async verify(receipt: Receipt, railId: string = this.railId): Promise<VerifyResult> {
    return (await this.#request(
      "POST",
      `/v1/rails/${encodeURIComponent(railId)}/verify`,
      receipt,
    )) as VerifyResult;
  }

  /**
   * `POST /v1/rails/:rail_id/validate-destination`. Every verdict — including
   * the refusals — is a `200`. Branch on `status` and `is_refusal`, not on the
   * status code.
   */
  async validateDestination(destination: string, railId: string = this.railId): Promise<DestinationVerdict> {
    return (await this.#request(
      "POST",
      `/v1/rails/${encodeURIComponent(railId)}/validate-destination`,
      { destination },
    )) as DestinationVerdict;
  }

  /**
   * `POST /v1/rails/:rail_id/webhook`. The body must be the processor's bytes
   * verbatim. A rail with no push delivery answers 501, which arrives here as a
   * {@link SidecarHttpError} with `kind === "unsupported"`.
   */
  async webhook(delivery: WebhookDelivery, railId: string = this.railId): Promise<WebhookEvent> {
    return (await this.#request(
      "POST",
      `/v1/rails/${encodeURIComponent(railId)}/webhook`,
      delivery,
    )) as WebhookEvent;
  }

  /** Kill the child and wait for it. Idempotent. */
  async stop(): Promise<void> {
    if (this.#stopped) return;
    this.#stopped = true;
    this.#child.kill();
    await this.#child.exited;
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.stop();
  }
}
