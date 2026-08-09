// patala DIRECT mode on Bun — libpatala_ffi inside this process, over the C
// ABI in patala-ffi/include/patala.h.
//
//   cargo build -p patala-ffi --release        # from the repo root
//   bun run examples/direct.ts
//
// EVERY LINE OF THIS RUNS ON MockRail. patala is a payments library, and an
// example that moves real value is not an example: the mock rail is
// deterministic, needs no credentials, opens no socket, and is present in every
// build — so a full charge -> verify round trip is reachable before a single
// secret exists.
//
// Nothing here needs the network, and nothing here is allowed to want it.

import { execFileSync } from "node:child_process";
import { readdirSync, statSync } from "node:fs";

import { abiCheck, abiVersion, Rail, type Receipt, resolveLibrary } from "../index.ts";

/**
 * How many threads this process has right now.
 *
 * Present because patala's headline claim over the Go-based C ABIs in this
 * suite — llmux, openrate — is that loading it starts nothing. A claim like
 * that should be measured, not asserted.
 *
 * READ THE NUMBER WITH CARE ON BUN, though, and the example says so where it
 * prints it: Bun grows its own thread count lazily, and a script with no patala
 * in it at all was measured going 6 -> 8 -> 9 across the same kind of work. So
 * this is a smell test here, not a proof. The proof is
 * `patala-ffi/ctest/smoke.c`, which counts in C with nothing else in the
 * process, and `sdks/node`, whose Node measurement is stable at 7 across runs.
 */
function threads(): number {
  if (process.platform === "linux") return readdirSync("/proc/self/task").length;
  if (process.platform === "darwin") {
    return execFileSync("ps", ["-M", String(process.pid)], { encoding: "utf8" }).trim().split("\n").length - 1;
  }
  return -1;
}

const before = threads();

console.log(`bun         ${Bun.version} on ${process.platform}/${process.arch}`);
console.log(`library     ${resolveLibrary()}`);
console.log(`bytes       ${statSync(resolveLibrary()).size}`);
console.log(`abi         ${abiVersion()}`);
console.log(`threads     ${before} before dlopen -> ${threads()} after\n`);

// patala_abi_check, not a comparison written here: the ABI exports the check so
// that twelve bindings do not each reimplement — and each forget — it.
abiCheck(abiVersion());
try {
  abiCheck("0.0.0-not-this-one");
  console.log("version     UNEXPECTED: a wrong version passed the check");
} catch (e) {
  console.log(`version     ${e instanceof Error ? e.message : "non-Error thrown"}\n`);
}

// ===========================================================================
// 1. A rail, and what it says about itself
// ===========================================================================
{
  // `using` closes the handle on every exit path out of this block, throw
  // included. Without it, an exception between open and close leaks a rail.
  using rail = Rail.open({ rail: "mock", currencies: ["USDC"], fee_minor: 25 });
  console.log(`rail        handle ${rail.handle}, id ${rail.id().rail_id}`);

  const caps = rail.capabilities();
  // Decide your whole UX from `class`, without knowing which provider answered.
  // NonCustodialFinal means a wallet address and a signed, irreversible
  // receipt; CustodialReversible would mean a card form and a refundable
  // pending state. It is not a bool because those are not two shades of one
  // thing.
  console.log(`caps        ${caps.class}, reversible ${caps.reversible}, holds_funds ${caps.holds_funds}`);
  console.log(`            currencies ${caps.currencies.join(", ")}, settlement ${JSON.stringify(caps.settlement)}\n`);

  // =========================================================================
  // 2. quote, charge, verify — and the receipt IS the entitlement
  // =========================================================================
  const req = {
    amount_minor: 1250,
    currency: "USDC",
    destination: "mock:wallet:alice",
    reference: "order-1",
  };

  // Integer minor units on both sides. 1250 USDC base units, plus a 25 fee.
  // Never a float, anywhere.
  const q = rail.quote(req);
  console.log(`quote       ${q.amount_minor} + ${q.fee_minor} fee = ${q.total_minor} ${q.currency}`);
  console.log(`            expires_at_unix ${q.expires_at_unix}`);

  const receipt = rail.charge(req);
  console.log(`charge      ${receipt.amount_minor} ${receipt.currency} for ${receipt.reference}`);
  console.log(`            proof ${receipt.proof.length} bytes, settled_at_unix ${receipt.settled_at_unix}`);

  // THIS is the entitlement check — not `charge` having returned. Store the
  // receipt, hand it back later, and gate on this.
  console.log(`verify      ${JSON.stringify(rail.verify(receipt))}`);

  // A tampered receipt verifies FALSE, and false is a RESULT, not an error.
  // The distinction is the whole point: if this arrived as a thrown error, a
  // caller with a retry loop would treat an unpaid order as a transient failure
  // and eventually as an entitlement.
  const tampered: Receipt = { ...receipt, amount_minor: receipt.amount_minor * 100 };
  const verdict = rail.verify(tampered);
  console.log(`tampered    ${JSON.stringify(verdict)} — a result, not a thrown error`);
  console.log(`            typeof ${typeof verdict.valid}, so \`if (v.valid)\` is the only correct gate\n`);

  // =========================================================================
  // 3. validate-destination — the offline pre-flight, which never fails
  // =========================================================================
  // Five verdicts, all reachable on the mock rail, all returned as answers.
  // "I cannot check this" is `Unknown`, not an exception, because an error is
  // too easy to swallow on the one question that decides where money goes.
  for (const dest of ["mock:wallet:alice", "mock:program:vault", "stellar:wallet:alice", "nonsense"]) {
    const v = rail.validateDestination(dest);
    const status = v.status.padEnd(18);
    console.log(`destination ${dest.padEnd(21)} ${status} refusal=${v.is_refusal} confirm=${v.human_must_confirm}`);
  }
  // human_must_confirm is true even on StructurallyValid, and this is the
  // sentence to show that human. `caveat` returns the same text with no verdict
  // attached, for the form where the address is first asked for.
  console.log(`caveat      ${rail.caveat().exchange_deposit_caveat.slice(0, 96)}…\n`);

  // =========================================================================
  // 4. What this build cannot do, said by name
  // =========================================================================
  // The mock rail has no push delivery, so it refuses rather than inventing an
  // event that a caller might gate entitlement on.
  try {
    rail.webhook({ body: "{}", headers: {}, now_unix: 1_700_000_000 });
    console.log("webhook     UNEXPECTED: the mock rail produced an event");
  } catch (e) {
    console.log(`webhook     ${e instanceof Error ? e.message : "non-Error thrown"}`);
  }

  // A default cdylib links no fiat adapters, and says which feature is missing
  // rather than answering with an empty list.
  try {
    console.log(`providers   ${JSON.stringify(rail.providers().providers)}`);
  } catch (e) {
    console.log(`providers   ${e instanceof Error ? e.message : "non-Error thrown"}`);
  }

  // An unknown method is a clean error naming the closed set.
  try {
    rail.call("refund" as never);
  } catch (e) {
    console.log(`unknown     ${e instanceof Error ? e.message : "non-Error thrown"}\n`);
  }
}

// ===========================================================================
// 5. Your error path, on demand
// ===========================================================================
{
  // `failing: true` is a mock rail where every operation fails — so the branch
  // your code takes when a processor is down is reachable offline, in a test,
  // without waiting for a real outage.
  using broken = Rail.open({ rail: "mock", failing: true });
  try {
    broken.charge({ amount_minor: 1, currency: "USDC", destination: "mock:wallet:alice", reference: "r" });
    console.log("failing     UNEXPECTED: a failing rail charged");
  } catch (e) {
    console.log(`failing     ${e instanceof Error ? e.message : "non-Error thrown"}`);
  }

  // A rail with destination checks off answers Unknown to everything — the
  // offline stand-in for a fiat rail, whose destination is an opaque
  // processor-side token. Unknown is NOT "fine": it is "a human must decide",
  // which is why human_must_confirm is set and is_refusal is not.
  using opaque = Rail.open({ rail: "mock", destination_checks: false });
  const v = opaque.validateDestination("acct_1234567890");
  console.log(`opaque      ${v.status}, refusal=${v.is_refusal} confirm=${v.human_must_confirm} — a fiat rail's honest answer`);
}

// A closed handle is a clean error, never a crash. This particular message is
// this binding's own guard, which fires before the ABI is reached; the ABI is
// equally safe on its own, because handles are registry integers, never
// pointers, and are never reused.
const stale = Rail.open();
stale.close();
stale.close(); // idempotent, as patala_close is
try {
  stale.id();
} catch (e) {
  console.log(`use-after   ${e instanceof Error ? e.message : "non-Error thrown"}`);
}

// bun:ffi has no asynchronous call mode — no `nonblocking` like Deno's, no
// threadpool variant like koffi's — so every call above ran on this thread. On
// the mock rail that is microseconds. On a real rail see README.md: a Bun
// `Worker` is the answer, and it was measured terminating cleanly with this
// library loaded.
console.log(`\nthreads     ${before} at startup -> ${threads()} after all of the above`);
console.log("            ±1 run to run, and that ±1 is BUN's: the same measurement with no patala");
console.log("            loaded was seen going 6 -> 8 -> 9. patala starts no thread — no GC, no");
console.log("            scheduler, no signal handler — and `make smoke-ffi` is where that is proved");
console.log("            in C rather than smelled here.");
