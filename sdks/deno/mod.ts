/**
 * patala for Deno — both modes in one dependency-free module.
 *
 * DIRECT (in-process, the C ABI in `patala-ffi/include/patala.h`):
 *
 * ```ts
 * import { Rail } from "./mod.ts";
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
 * There is deliberately NO STREAMING, here or in the ABI: patala has no
 * streaming operation. Nothing it does produces a sequence of chunks, so there
 * is nothing to iterate — no `patala_stream`, and no async iterator on `Rail`.
 * (llmux, which shares this ABI shape, does have `llmux_stream`. Do not go
 * looking for patala's.)
 *
 * Sync where it is free, async where it might not be: every method is
 * synchronous, because on the mock rail they answer in microseconds and a
 * promise would cost more than the call. {@linkcode Rail.callAsync} is the
 * escape hatch for a real rail, whose `charge` is a network round trip — its
 * symbol is declared `nonblocking: true`, so Deno runs it on a blocking-task
 * thread and this isolate keeps going.
 *
 * JSON in, JSON out — the same JSON `patala-sidecar` serves.
 *
 * EVERY EXAMPLE IN THIS PACKAGE USES MockRail. patala is a payments library and
 * an example that moves real value is not an example.
 *
 * @module
 */

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

// ---------------------------------------------------------------------------
// Boundary narrowing for the two documents a caller makes a decision on
// ---------------------------------------------------------------------------
//
// Everywhere else here a response is `as`-cast to its interface, which is fine
// for a Quote or a Receipt: the caller reads fields and reconciles them against
// its own record. It is not fine for the two documents that ARE the decision.
// `as` is a compile-time assertion about a value that arrived at runtime over a
// socket or across a C ABI, and `JSON.parse` hands back whatever shape it was
// given — an absent `valid` is `undefined`, and a `valid` some proxy
// stringified is `"false"`, which is TRUTHY. `if (result.valid)` then grants
// entitlement against a receipt no rail confirmed.
//
// Narrowed once, here, so the value a caller is handed is the type it was
// promised and both directions fail closed:
//   valid       true only for the JSON boolean true
//   is_refusal  false only for the JSON boolean false

/** `valid` is `true` only when the wire said the JSON boolean `true`. */
export function narrowVerify(body: unknown): VerifyResult {
  const valid = (body as { valid?: unknown } | null)?.valid;
  return { ...(body as VerifyResult), valid: valid === true };
}

/**
 * `is_refusal` is `false` only when the wire said the JSON boolean `false`,
 * and `human_must_confirm` likewise — every verdict patala can produce carries
 * both, and a document missing either is not a verdict this SDK can read.
 * "I could not read the verdict" and "do not send" are the same answer.
 */
export function narrowVerdict(body: unknown): DestinationVerdict {
  const v = body as { is_refusal?: unknown; human_must_confirm?: unknown } | null;
  return {
    ...(body as DestinationVerdict),
    is_refusal: v?.is_refusal !== false,
    human_must_confirm: v?.human_must_confirm !== false,
  };
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
// DIRECT — libpatala_ffi over Deno.dlopen
// ===========================================================================

function libFileName(): string {
  // cargo names the cdylib libpatala_ffi.{dylib,so} / patala_ffi.dll.
  if (Deno.build.os === "darwin") return "libpatala_ffi.dylib";
  if (Deno.build.os === "windows") return "patala_ffi.dll";
  return "libpatala_ffi.so";
}

/**
 * Where the shared library will be loaded from: `PATALA_LIBRARY`, else a repo
 * checkout's `target/release/`, else its `target/debug/`, else the bare name
 * for the system loader.
 *
 * Written for the permission set `examples/direct.ts` actually runs under,
 * which is `--allow-ffi` and nothing else. The env lookup is skipped unless env
 * permission is already granted, so it neither prompts nor throws; and when
 * `Deno.statSync` fails with anything other than `NotFound` — which under
 * `--allow-ffi` alone means "no read permission, cannot look" — the checkout
 * path is returned anyway and `dlopen` delivers the verdict, rather than
 * silently skipping a library sitting right there.
 *
 * **Built and executed here: darwin/arm64 only.** Nothing in this module
 * implies a Linux `.so` or a Windows `.dll` exists — see README.md.
 */
export function resolveLibrary(explicit?: string): string {
  if (explicit) return explicit;
  const fromEnv = Deno.permissions.querySync({ name: "env", variable: "PATALA_LIBRARY" }).state === "granted"
    ? Deno.env.get("PATALA_LIBRARY")
    : undefined;
  if (fromEnv) return fromEnv;
  const canRead = Deno.permissions.querySync({ name: "read" }).state === "granted";
  let firstCandidate: string | undefined;
  for (const profile of ["release", "debug"]) {
    const candidate = new URL(`../../target/${profile}/${libFileName()}`, import.meta.url).pathname;
    firstCandidate ??= candidate;
    if (!canRead) return candidate; // cannot look; let dlopen decide
    try {
      Deno.statSync(candidate);
      return candidate;
    } catch (e) {
      if (!(e instanceof Deno.errors.NotFound)) return candidate;
    }
  }
  return firstCandidate ?? libFileName();
}

// `patala_call` appears twice: once blocking, once `nonblocking`. Deno keys
// symbols by the JS name and takes the C name from `name`, so both entries
// resolve the same export.
const SYMBOLS = {
  patala_abi_version: { parameters: [], result: "pointer" },
  patala_abi_check: { parameters: ["buffer", "buffer"], result: "i32" },
  patala_new: { parameters: ["buffer", "buffer"], result: "u64" },
  patala_call: { parameters: ["u64", "buffer", "buffer", "buffer"], result: "pointer" },
  patala_call_async: {
    name: "patala_call",
    parameters: ["u64", "buffer", "buffer", "buffer"],
    result: "pointer",
    nonblocking: true,
  },
  patala_close: { parameters: ["u64"], result: "void" },
  patala_free: { parameters: ["pointer"], result: "void" },
} as const;

type Lib = Deno.DynamicLibrary<typeof SYMBOLS>;

const _libs = new Map<string, Lib>();

function load(libPath: string): Lib {
  const cached = _libs.get(libPath);
  if (cached) return cached;
  const lib = Deno.dlopen(libPath, SYMBOLS);
  _libs.set(libPath, lib);
  return lib;
}

const encoder = new TextEncoder();

/**
 * A NUL-terminated UTF-8 copy of `s`, for a `const char*` parameter.
 *
 * Backed by a plain ArrayBuffer rather than `encoder.encode`'s
 * `ArrayBufferLike`: Deno's FFI refuses a buffer that might be shared, and it
 * is right to — the library reads it from another thread on a nonblocking call.
 */
function cstr(s: string): Uint8Array<ArrayBuffer> {
  const buf = new Uint8Array(new ArrayBuffer(s.length * 3 + 1));
  const { written } = encoder.encodeInto(s, buf);
  buf[written] = 0;
  return buf.subarray(0, written + 1);
}

/** A `char** err` slot: eight bytes for the library to write a pointer into. */
function errSlot(): BigUint64Array<ArrayBuffer> {
  return new BigUint64Array(new ArrayBuffer(8));
}

/** Read a C string patala allocated, then free it. Freeing is not optional. */
function takeString(lib: Lib, p: Deno.PointerValue): string | null {
  if (p === null) return null;
  try {
    return Deno.UnsafePointerView.getCString(p);
  } finally {
    lib.symbols.patala_free(p);
  }
}

/** Turn a populated `char** err` into an Error, freeing the message. */
function takeError(lib: Lib, slot: BigUint64Array<ArrayBuffer>, fallback: string): Error {
  // Error strings are plain UTF-8 text, NOT JSON. Do not parse them.
  const msg = takeString(lib, Deno.UnsafePointer.create(slot[0] ?? 0n));
  slot[0] = 0n;
  return new Error(msg ?? fallback);
}

/** The patala version the loaded shared library was built from. */
export function abiVersion(libraryPath?: string): string {
  // A static string the library owns. Do NOT free it.
  const p = load(resolveLibrary(libraryPath)).symbols.patala_abi_version();
  return p === null ? "" : Deno.UnsafePointerView.getCString(p);
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
  const err = errSlot();
  if (lib.symbols.patala_abi_check(cstr(expected), err) !== 0) {
    throw takeError(lib, err, `patala_abi_check(${expected}) failed`);
  }
}

function body(request: unknown): Uint8Array<ArrayBuffer> | null {
  if (request == null) return null;
  return cstr(typeof request === "string" ? request : JSON.stringify(request));
}

export interface RailOptions {
  /** Override the shared library path (otherwise {@linkcode resolveLibrary}). */
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
    const err = errSlot();
    const h = lib.symbols.patala_new(config === undefined ? null : cstr(JSON.stringify(config)), err);
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
   */
  call<M extends keyof ResultOf>(method: M, request?: unknown): ResultOf[M] {
    const h = this.#live();
    const err = errSlot();
    const res = this.#lib.symbols.patala_call(h, cstr(method), body(request), err);
    if (res === null) throw takeError(this.#lib, err, `patala_call(${method}) failed`);
    return JSON.parse(takeString(this.#lib, res) ?? "null") as ResultOf[M];
  }

  /**
   * The same call, on a blocking-task thread instead of this one.
   *
   * Pointless for the mock rail, which answers in microseconds. Not pointless
   * for a real one: `charge` on Solana, Stellar or a fiat processor is a
   * network round trip, and the synchronous form would hold the isolate for its
   * whole duration.
   */
  async callAsync<M extends keyof ResultOf>(method: M, request?: unknown): Promise<ResultOf[M]> {
    const h = this.#live();
    const err = errSlot();
    const res = await this.#lib.symbols.patala_call_async(h, cstr(method), body(request), err);
    if (res === null) throw takeError(this.#lib, err, `patala_call(${method}) failed`);
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
  /** Extra environment for the child; overrides what {@linkcode Sidecar.start} sets. */
  env?: Record<string, string>;
  /** How long to wait for the child to start LISTENING, ms (default 10000). */
  timeoutMs?: number;
  /** Binary to run; default `PATALA_SIDECAR_BINARY`, else `patala-sidecar` on PATH. */
  binary?: string;
  /** Where the child's stdout/stderr go (default "inherit"). */
  stdio?: "inherit" | "null";
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
  // process, and the alternative — a fixed port — races every other run of
  // this example.
  const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  const { port } = listener.addr as Deno.NetAddr;
  listener.close();
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
  #child: Deno.ChildProcess;
  #status: Promise<Deno.CommandStatus>;
  #stopped = false;

  private constructor(
    child: Deno.ChildProcess,
    status: Promise<Deno.CommandStatus>,
    baseURL: string,
    token: string,
  ) {
    this.#child = child;
    this.#status = status;
    this.baseURL = baseURL;
    this.token = token;
  }

  static async start(opts: SidecarOptions = {}): Promise<Sidecar> {
    const port = opts.port ?? freePort();
    const binary = opts.binary ?? Deno.env.get("PATALA_SIDECAR_BINARY") ?? "patala-sidecar";
    const token = opts.token ?? randomToken();
    const stdio = opts.stdio ?? "inherit";

    const child = new Deno.Command(binary, {
      // clearEnv is false, so the child still sees the environment a real rail
      // would read its credentials from.
      env: {
        PATALA_SIDECAR_TOKEN: token,
        PATALA_SIDECAR_PORT: String(port),
        ...opts.env,
      },
      stdout: stdio,
      stderr: stdio,
      stdin: "null",
    }).spawn();

    let exited: number | null = null;
    const status = child.status.then((s) => {
      exited = s.code;
      return s;
    });

    const base = `http://127.0.0.1:${port}`;
    const deadline = Date.now() + (opts.timeoutMs ?? 10_000);
    for (;;) {
      if (exited !== null) {
        throw new Error(
          `patala-sidecar exited with code ${exited} before it listened on ${base} ` +
            "(it refuses to start without PATALA_SIDECAR_TOKEN; check the output above)",
        );
      }
      try {
        // /healthz is the one unauthenticated route and reveals nothing about
        // which rails are configured. Everything else is behind the token.
        const res = await fetch(base + "/healthz");
        await res.text();
        if (res.status === 200) return new Sidecar(child, status, base, token);
      } catch {
        // not listening yet
      }
      if (Date.now() > deadline) {
        child.kill();
        await status;
        throw new Error(
          `patala-sidecar did not start listening on ${base} within ${seconds(opts.timeoutMs ?? 10_000)}`,
        );
      }
      await new Promise((r) => setTimeout(r, 50));
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
    return await this.#request("GET", `/v1/rails/${encodeURIComponent(railId)}`) as RailCapabilities;
  }

  /** `POST /v1/rails/:rail_id/quote`. Moves no money. */
  async quote(req: PayRequest, railId: string = this.railId): Promise<Quote> {
    return await this.#request("POST", `/v1/rails/${encodeURIComponent(railId)}/quote`, req) as Quote;
  }

  /** `POST /v1/rails/:rail_id/charge`. Store the receipt it returns. */
  async charge(req: PayRequest, railId: string = this.railId): Promise<Receipt> {
    return await this.#request("POST", `/v1/rails/${encodeURIComponent(railId)}/charge`, req) as Receipt;
  }

  /**
   * `POST /v1/rails/:rail_id/verify`. A `200` with `{valid: false}` is the
   * honest answer, never an HTTP error — so "verified false" can never be
   * mistaken for "the sidecar broke".
   */
  async verify(receipt: Receipt, railId: string = this.railId): Promise<VerifyResult> {
    return narrowVerify(
      await this.#request("POST", `/v1/rails/${encodeURIComponent(railId)}/verify`, receipt),
    );
  }

  /**
   * `POST /v1/rails/:rail_id/validate-destination`. Every verdict — including
   * the refusals — is a `200`. Branch on `status` and `is_refusal`, not on the
   * status code.
   */
  async validateDestination(destination: string, railId: string = this.railId): Promise<DestinationVerdict> {
    return narrowVerdict(
      await this.#request("POST", `/v1/rails/${encodeURIComponent(railId)}/validate-destination`, {
        destination,
      }),
    );
  }

  /**
   * `POST /v1/rails/:rail_id/webhook`. The body must be the processor's bytes
   * verbatim. A rail with no push delivery answers 501, which arrives here as a
   * {@linkcode SidecarHttpError} with `kind === "unsupported"`.
   */
  async webhook(delivery: WebhookDelivery, railId: string = this.railId): Promise<WebhookEvent> {
    return await this.#request(
      "POST",
      `/v1/rails/${encodeURIComponent(railId)}/webhook`,
      delivery,
    ) as WebhookEvent;
  }

  /** Kill the child and wait for it. Idempotent. */
  async stop(): Promise<void> {
    if (this.#stopped) return;
    this.#stopped = true;
    try {
      this.#child.kill();
    } catch {
      // already gone
    }
    // Awaiting is not politeness: Deno fails a run with a leaked child.
    await this.#status;
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.stop();
  }
}
