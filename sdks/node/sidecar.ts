// patala, sidecar — the `patala-sidecar` binary in a child process, HTTP over
// loopback, started and supervised for you.
//
//   import { Sidecar } from "patala";
//
//   await using side = await Sidecar.start();
//   const receipt = await side.charge({ amount_minor: 1250, currency: "USDC",
//                                       destination: "mock:wallet:alice", reference: "order-1" });
//   (await side.verify(receipt)).valid;              // true
//
// No native code, no FFI, and it works wherever the binary builds. What it buys
// over direct mode is KEY ISOLATION: a non-custodial rail's signing key lives
// in the sidecar's process and nowhere else, instead of being smeared across
// every process in a polyglot stack that linked the library. That is the
// reason to prefer it, and it is a real one.
//
// TWO THINGS THE SIDECAR DOES THAT DIRECT MODE DOES NOT:
//
//   1. It is fail-closed on auth. `PATALA_SIDECAR_TOKEN` must be in its
//      environment or the process refuses to start — there is no generated
//      fallback and no unauthenticated mode. `start()` mints a 32-byte token
//      and passes it in the ENVIRONMENT, never in argv, because argv is
//      readable by every process on the machine via `ps`.
//   2. It binds 127.0.0.1, hardcoded, not configurable. Only the port is.
//
// AND ONE THING IT DOES NOT DO YET: the rail registry is MOCK-ONLY. The only
// `rail_id` that exists is "mock"; anything else is a 404. Per-rail
// registration is unwritten — see patala-sidecar/README.md. Direct mode is
// where a real rail is reachable today.
//
// There is deliberately no streaming: patala has no streaming operation.

import { spawn, type ChildProcess } from "child_process";
import * as crypto from "crypto";
import * as net from "net";

import type {
  DestinationVerdict,
  PayRequest,
  Quote,
  RailCapabilities,
  Receipt,
  VerifyResult,
  WebhookDelivery,
  WebhookEvent,
} from "./types.js";
import { narrowVerdict, narrowVerify } from "./types.js";

export interface SidecarOptions {
  /** Fixed port; default is an ephemeral free port on 127.0.0.1. */
  port?: number;
  /**
   * The bearer token. Default: 32 fresh random bytes, hex. Supply your own
   * only to share a sidecar with something else that already knows it.
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
 * A non-2xx answer from the sidecar, with the status and the `kind` discriminant
 * from its error body.
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

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.unref();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      // A loopback host:port, so address() is always an AddressInfo here.
      const { port } = srv.address() as net.AddressInfo;
      srv.close(() => {
        resolve(port);
      });
    });
  });
}

const seconds = (ms: number): string => `${String(Math.round(ms / 100) / 10)}s`;

/**
 * A running `patala-sidecar`, owned by this process.
 *
 * `start()` mints a token, picks a free loopback port, launches the binary and
 * polls `/healthz` until it answers. There is no second readiness question
 * here: `/healthz` answering means the router is up, and the mock rail needs no
 * warm-up. (openrate's SDK, which this one is shaped after, has to distinguish
 * liveness from readiness because its server fetches rates at startup. patala's
 * does not fetch anything.)
 */
export class Sidecar implements AsyncDisposable {
  readonly baseURL: string;
  /** The bearer token this sidecar was started with. */
  readonly token: string;
  /** The only rail id the sidecar's registry currently has. */
  readonly railId: string;
  private child: ChildProcess;
  private stopped = false;

  private constructor(child: ChildProcess, baseURL: string, token: string, railId: string) {
    this.child = child;
    this.baseURL = baseURL;
    this.token = token;
    this.railId = railId;
  }

  static async start(opts: SidecarOptions = {}): Promise<Sidecar> {
    const port = opts.port ?? (await freePort());
    const binary = opts.binary ?? process.env.PATALA_SIDECAR_BINARY ?? "patala-sidecar";
    // 32 bytes from the CSPRNG. In the ENVIRONMENT, never in argv: argv is
    // world-readable through `ps`, and a bearer token that authorises `charge`
    // is not something to publish to every process on the machine.
    const token = opts.token ?? crypto.randomBytes(32).toString("hex");

    const child = spawn(binary, [], {
      env: {
        ...process.env,
        PATALA_SIDECAR_TOKEN: token,
        PATALA_SIDECAR_PORT: String(port),
        ...opts.env,
      },
      stdio: ["ignore", opts.stdio ?? "inherit", opts.stdio ?? "inherit"],
    });
    // Capture spawn failures (ENOENT for a missing binary) so they surface as a
    // rejected start() rather than an uncaught 'error' event.
    let spawnError: Error | null = null;
    child.on("error", (e) => {
      spawnError = e;
    });
    // The sidecar EXITS 1 when PATALA_SIDECAR_TOKEN is missing or empty, so a
    // child that is gone is a real outcome to report, not just a slow start.
    let exited: number | null = null;
    child.on("exit", (code) => {
      exited = code;
    });
    const currentSpawnError = (): Error | null => spawnError;
    const currentExit = (): number | null => exited;

    const base = `http://127.0.0.1:${String(port)}`;
    const deadline = Date.now() + (opts.timeoutMs ?? 10_000);
    for (;;) {
      const failed = currentSpawnError();
      if (failed) throw failed;
      const code = currentExit();
      if (code !== null) {
        throw new Error(
          `patala-sidecar exited with code ${String(code)} before it listened on ${base} ` +
            "(it refuses to start without PATALA_SIDECAR_TOKEN; check the output above)"
        );
      }
      try {
        // /healthz is the one unauthenticated route and reveals nothing about
        // which rails are configured. Everything else is behind the token.
        const res = await fetch(base + "/healthz");
        await res.text();
        if (res.status === 200) return new Sidecar(child, base, token, "mock");
      } catch {
        // not listening yet
      }
      if (Date.now() > deadline) {
        child.kill();
        throw new Error(
          `patala-sidecar did not start listening on ${base} within ${seconds(opts.timeoutMs ?? 10_000)}`
        );
      }
      await new Promise((r) => setTimeout(r, 50));
    }
  }

  private async request(method: "GET" | "POST", pathname: string, body?: unknown): Promise<unknown> {
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
      throw new SidecarHttpError(res.status, kind, `patala-sidecar: HTTP ${String(res.status)}: ${message}`);
    }
    return JSON.parse(text) as unknown;
  }

  /** Liveness. The one route that needs no token. */
  async healthz(): Promise<string> {
    const res = await fetch(this.baseURL + "/healthz");
    return (await res.text()).trim();
  }

  /** `GET /v1/rails/:rail_id`. */
  async capabilities(railId = this.railId): Promise<RailCapabilities> {
    return (await this.request("GET", `/v1/rails/${encodeURIComponent(railId)}`)) as RailCapabilities;
  }

  /** `POST /v1/rails/:rail_id/quote`. Moves no money. */
  async quote(req: PayRequest, railId = this.railId): Promise<Quote> {
    return (await this.request("POST", `/v1/rails/${encodeURIComponent(railId)}/quote`, req)) as Quote;
  }

  /** `POST /v1/rails/:rail_id/charge`. Store the receipt it returns. */
  async charge(req: PayRequest, railId = this.railId): Promise<Receipt> {
    return (await this.request("POST", `/v1/rails/${encodeURIComponent(railId)}/charge`, req)) as Receipt;
  }

  /**
   * `POST /v1/rails/:rail_id/verify`. A `200` with `{valid: false}` is the
   * honest answer, never an HTTP error — so "verified false" can never be
   * mistaken for "the sidecar broke".
   */
  async verify(receipt: Receipt, railId = this.railId): Promise<VerifyResult> {
    const body = await this.request("POST", `/v1/rails/${encodeURIComponent(railId)}/verify`, receipt);
    return narrowVerify(body);
  }

  /**
   * `POST /v1/rails/:rail_id/validate-destination`. Every verdict — including
   * the refusals — is a `200`. Branch on `status` and `is_refusal`, not on the
   * status code.
   */
  async validateDestination(destination: string, railId = this.railId): Promise<DestinationVerdict> {
    const body = await this.request("POST", `/v1/rails/${encodeURIComponent(railId)}/validate-destination`, {
      destination,
    });
    return narrowVerdict(body);
  }

  /**
   * `POST /v1/rails/:rail_id/webhook`. The body must be the processor's bytes
   * verbatim. A rail with no push delivery answers 501, which arrives here as a
   * {@link SidecarHttpError} with `kind === "unsupported"`.
   */
  async webhook(delivery: WebhookDelivery, railId = this.railId): Promise<WebhookEvent> {
    return (await this.request(
      "POST",
      `/v1/rails/${encodeURIComponent(railId)}/webhook`,
      delivery
    )) as WebhookEvent;
  }

  /** Kill the child. Idempotent. */
  stop(): void {
    if (this.stopped) return;
    this.stopped = true;
    this.child.kill();
  }

  async [Symbol.asyncDispose](): Promise<void> {
    this.stop();
    // Wait for the child to actually go, so a test that starts several
    // sidecars in a row is not racing a still-bound port.
    if (this.child.exitCode === null && this.child.signalCode === null) {
      await new Promise<void>((resolve) => this.child.once("exit", () => resolve()));
    }
  }
}
