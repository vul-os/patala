// patala SIDECAR mode on Node — the `patala-sidecar` binary in a child
// process, HTTP over loopback.
//
//   cargo build -p patala-sidecar                       # from the repo root
//   PATALA_SIDECAR_BINARY=../../target/debug/patala-sidecar npm run example:sidecar
//
// No FFI, no native dependency, nothing loaded into this process. The reason to
// choose it is KEY ISOLATION: a signing key lives in the sidecar and nowhere
// else, instead of in every process of a polyglot stack that linked the library.
//
// Everything below runs against MockRail — which is not a choice this example
// gets to make, because the sidecar's registry is mock-only today. That is
// stated rather than hidden: see the 404 near the end.
//
// The only network here is 127.0.0.1.

import { Sidecar, SidecarHttpError } from "../index.js";

console.log(`node        ${process.version} on ${process.platform}/${process.arch}`);

// start() mints a 32-byte token and passes it in the child's ENVIRONMENT, never
// in argv — argv is world-readable via `ps`, and this token authorises `charge`.
// The sidecar refuses to start at all without it: there is no unauthenticated
// mode and no generated fallback.
//
// `await using` kills the child on the way out of the block, throw included.
await using side = await Sidecar.start({ stdio: "ignore" });
console.log(`sidecar     ${side.baseURL}  (loopback only, hardcoded — not a knob)`);
console.log(`token       ${side.token.slice(0, 8)}… (32 random bytes, in the environment, not argv)`);
console.log(`healthz     ${await side.healthz()} — the one unauthenticated route\n`);

const caps = await side.capabilities();
console.log(`caps        ${caps.class}, currencies ${caps.currencies.join(", ")}, settlement ${JSON.stringify(caps.settlement)}`);

const req = {
  amount_minor: 1250,
  currency: "USDC",
  destination: "mock:wallet:alice",
  reference: "order-1",
};

const q = await side.quote(req);
console.log(`quote       ${String(q.amount_minor)} + ${String(q.fee_minor)} fee = ${String(q.total_minor)} ${q.currency}`);

const receipt = await side.charge(req);
console.log(`charge      ${String(receipt.amount_minor)} ${receipt.currency} for ${receipt.reference}, proof ${String(receipt.proof.length)} bytes`);

// The same JSON direct mode returns — identical wire contract, different
// transport. A body that works here works against patala_call unchanged.
console.log(`verify      ${JSON.stringify(await side.verify(receipt))}`);

// 200 with {"valid": false}, never an HTTP error, so "verified false" can never
// be mistaken for "the sidecar broke".
const tampered = { ...receipt, reference: "order-99" };
console.log(`tampered    HTTP 200 ${JSON.stringify(await side.verify(tampered))} — an answer, not a failure\n`);

// Every verdict, including the refusals, is a 200. Branch on `status` and
// `is_refusal`, never on the status code.
for (const dest of ["mock:wallet:alice", "mock:program:vault", "nonsense"]) {
  const v = await side.validateDestination(dest);
  console.log(`destination ${dest.padEnd(19)} ${v.status.padEnd(18)} refusal=${String(v.is_refusal)} confirm=${String(v.human_must_confirm)}`);
}
console.log();

// The mock rail has no push delivery and refuses rather than inventing an event.
// 501 arrives as kind "unsupported" — a rail declining, not an outage.
try {
  await side.webhook({ body: "{}", headers: {}, now_unix: 1_700_000_000 });
  console.log("webhook     UNEXPECTED: the mock rail produced an event");
} catch (e) {
  const err = e as SidecarHttpError;
  console.log(`webhook     HTTP ${String(err.status)} kind=${err.kind}: ${err.message}`);
}

// The registry is mock-only. Any other rail id is a 404 because this process
// has never heard of it — per-rail registration is unwritten.
try {
  await side.capabilities("solana");
  console.log("registry    UNEXPECTED: a non-mock rail answered");
} catch (e) {
  const err = e as SidecarHttpError;
  console.log(`registry    HTTP ${String(err.status)} kind=${err.kind}: ${err.message}`);
}

// The token gate sits in front of EVERY /v1 route, including the read-only
// capabilities lookup — not just the money-moving ones. A missing header, a
// malformed one and a wrong token are the same detail-free 401.
const bare = await fetch(`${side.baseURL}/v1/rails/mock`);
console.log(`no token    HTTP ${String(bare.status)} on a read-only route — the gate is in front of everything`);
const wrong = await fetch(`${side.baseURL}/v1/rails/mock`, { headers: { Authorization: "Bearer wrong" } });
console.log(`wrong token HTTP ${String(wrong.status)}, and indistinguishable from the line above`);
