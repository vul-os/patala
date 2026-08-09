// patala SIDECAR mode on Deno — the `patala-sidecar` binary in a child
// process, HTTP over loopback.
//
//   cargo build -p patala-sidecar              # from the repo root
//   PATALA_SIDECAR_BINARY=../../target/debug/patala-sidecar deno task example:sidecar
//   # or, spelled out:
//   deno run --allow-run --allow-net=127.0.0.1 --allow-env examples/sidecar.ts
//
// No --allow-ffi: there is no native code in this path and nothing is loaded
// into this process. Note that --allow-net IS here and IS meaningful — and it
// is scoped to 127.0.0.1, because that is the only host this example may
// legitimately reach. Direct mode's `--allow-ffi` could not make that promise
// even if it wanted to.
//
// The reason to choose the sidecar is KEY ISOLATION: a signing key lives in the
// sidecar's process and nowhere else, instead of in every process of a polyglot
// stack that linked the library.
//
// Everything below runs against MockRail — which is not a choice this example
// gets to make, because the sidecar's registry is mock-only today. That is
// stated rather than hidden: see the 404 near the end.
//
// If you are behind a proxy: Deno's fetch honours HTTP(S)_PROXY for loopback
// URLs too, so export NO_PROXY=127.0.0.1 or this process cannot reach its own
// sidecar and start() times out against a server that is running fine.

import { Sidecar, type SidecarHttpError } from "../mod.ts";

console.log(`deno        ${Deno.version.deno} on ${Deno.build.os}/${Deno.build.arch}`);

// start() mints a 32-byte token and passes it in the child's ENVIRONMENT, never
// in argv — argv is world-readable via `ps`, and this token authorises
// `charge`. The sidecar refuses to start at all without it: there is no
// unauthenticated mode and no generated fallback.
//
// `await using` kills the child and waits for it on the way out, throw
// included; Deno fails a run that leaks one.
await using side = await Sidecar.start({ stdio: "null" });
console.log(`sidecar     ${side.baseURL}  (loopback only, hardcoded — not a knob)`);
console.log(`token       ${side.token.slice(0, 8)}… (32 random bytes, in the environment, not argv)`);
console.log(`healthz     ${await side.healthz()} — the one unauthenticated route\n`);

const caps = await side.capabilities();
const settlement = JSON.stringify(caps.settlement);
console.log(
  `caps        ${caps.class}, currencies ${caps.currencies.join(", ")}, settlement ${settlement}`,
);

const req = {
  amount_minor: 1250,
  currency: "USDC",
  destination: "mock:wallet:alice",
  reference: "order-1",
};

const q = await side.quote(req);
console.log(`quote       ${q.amount_minor} + ${q.fee_minor} fee = ${q.total_minor} ${q.currency}`);

const receipt = await side.charge(req);
console.log(
  `charge      ${receipt.amount_minor} ${receipt.currency} for ${receipt.reference}, proof ${receipt.proof.length} bytes`,
);

// The same JSON direct mode returns — identical wire contract, different
// transport. A body that works here works against patala_call unchanged.
console.log(`verify      ${JSON.stringify(await side.verify(receipt))}`);

// 200 with {"valid": false}, never an HTTP error, so "verified false" can never
// be mistaken for "the sidecar broke".
const tampered = { ...receipt, reference: "order-99" };
console.log(
  `tampered    HTTP 200 ${JSON.stringify(await side.verify(tampered))} — an answer, not a failure\n`,
);

// Every verdict, including the refusals, is a 200. Branch on `status` and
// `is_refusal`, never on the status code.
for (const dest of ["mock:wallet:alice", "mock:program:vault", "nonsense"]) {
  const v = await side.validateDestination(dest);
  const status = v.status.padEnd(18);
  console.log(
    `destination ${dest.padEnd(19)} ${status} refusal=${v.is_refusal} confirm=${v.human_must_confirm}`,
  );
}
console.log();

// The mock rail has no push delivery and refuses rather than inventing an
// event. 501 arrives as kind "unsupported" — a rail declining, not an outage.
try {
  await side.webhook({ body: "{}", headers: {}, now_unix: 1_700_000_000 });
  console.log("webhook     UNEXPECTED: the mock rail produced an event");
} catch (e) {
  const err = e as SidecarHttpError;
  console.log(`webhook     HTTP ${err.status} kind=${err.kind}: ${err.message}`);
}

// The registry is mock-only. Any other rail id is a 404 because this process
// has never heard of it — per-rail registration is unwritten.
try {
  await side.capabilities("solana");
  console.log("registry    UNEXPECTED: a non-mock rail answered");
} catch (e) {
  const err = e as SidecarHttpError;
  console.log(`registry    HTTP ${err.status} kind=${err.kind}: ${err.message}`);
}

// The token gate sits in front of EVERY /v1 route, including the read-only
// capabilities lookup — not just the money-moving ones. A missing header, a
// malformed one and a wrong token are the same detail-free 401.
const bare = await fetch(`${side.baseURL}/v1/rails/mock`);
await bare.body?.cancel();
console.log(`no token    HTTP ${bare.status} on a read-only route — the gate is in front of everything`);
const wrong = await fetch(`${side.baseURL}/v1/rails/mock`, { headers: { Authorization: "Bearer wrong" } });
await wrong.body?.cancel();
console.log(`wrong token HTTP ${wrong.status}, and indistinguishable from the line above`);
