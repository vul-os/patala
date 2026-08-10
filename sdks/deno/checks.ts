// checks.ts — counted assertions over the part of this SDK that decides
// something, run with `deno task checks`.
//
// Almost everything money-shaped in patala is decided in Rust and this SDK
// hands it to you unchanged. The exception is the boundary itself: a response
// arrives as `unknown` from a socket or across a C ABI, and every method here
// used to `as`-cast it into its interface. A cast is a compile-time assertion
// about a runtime value — `JSON.parse` returns whatever shape it was given.
//
// For a Quote or a Receipt that is fine: the caller reads fields and
// reconciles them against its own record. For the two documents that ARE the
// decision it is not. An absent `valid` is `undefined`; a `valid` some proxy
// stringified is `"false"`, which is TRUTHY, so `if (result.valid)` grants
// entitlement against a receipt no rail ever confirmed. `narrowVerify` and
// `narrowVerdict` close both directions, and this is what holds them shut.
//
// No network, no child process, no shared library.

import { narrowVerdict, narrowVerify } from "./mod.ts";

const EXPECTED = 18;
let ran = 0;
let failed = 0;

function check(what: string, ok: boolean): void {
  ran += 1;
  if (ok) {
    console.log(`  ok   ${what}`);
  } else {
    failed += 1;
    console.log(`  FAIL ${what}`);
  }
}

console.log("-- narrowVerify: `valid` is true only for the JSON boolean true --");

check("a genuine {valid:true} verifies", narrowVerify({ valid: true }).valid === true);
check("a genuine {valid:false} does not", narrowVerify({ valid: false }).valid === false);
check('the STRING "true" is not true', narrowVerify({ valid: "true" }).valid === false);
check(
  'the STRING "false" is not true — and it is TRUTHY, which is the whole point',
  narrowVerify({ valid: "false" }).valid === false,
);
check("the number 1 is not true", narrowVerify({ valid: 1 }).valid === false);
check("an absent valid is not true", narrowVerify({}).valid === false);
check("null is not true", narrowVerify(null).valid === false);
check("a non-object body is not true", narrowVerify("ok").valid === false);
check(
  "the result is always a boolean, never undefined",
  typeof narrowVerify({}).valid === "boolean",
);

console.log();
console.log("-- narrowVerdict: `is_refusal` is false only for the JSON boolean false --");

const good = {
  rail_id: "mock",
  status: "StructurallyValid",
  reason: "ok",
  human_must_confirm: true,
  exchange_deposit_caveat: "…",
  is_refusal: false,
};

check("a genuine non-refusal stays a non-refusal", narrowVerdict(good).is_refusal === false);
check(
  "a genuine refusal stays a refusal",
  narrowVerdict({ ...good, status: "Malformed", is_refusal: true }).is_refusal === true,
);
check(
  'the STRING "false" is not false, so it is a refusal',
  narrowVerdict({ ...good, is_refusal: "false" }).is_refusal === true,
);
check("an absent is_refusal is a refusal", narrowVerdict({ status: "Malformed" }).is_refusal === true);
check("null is a refusal", narrowVerdict(null).is_refusal === true);
check("a non-object body is a refusal", narrowVerdict("<html>502</html>").is_refusal === true);
check(
  "human_must_confirm is likewise true unless the wire said false",
  narrowVerdict({}).human_must_confirm === true,
);
check(
  "...and a verdict that says false is respected",
  narrowVerdict({ ...good, human_must_confirm: false }).human_must_confirm === false,
);
check(
  "every other field is passed through untouched",
  narrowVerdict(good).status === "StructurallyValid" && narrowVerdict(good).rail_id === "mock",
);

console.log();
if (ran !== EXPECTED) {
  console.error(
    `checks: ran ${ran} assertions, expected ${EXPECTED}. A suite that quietly stops ` +
      "running half of itself is worse than no suite — update EXPECTED deliberately.",
  );
  Deno.exit(1);
}
if (failed !== 0) {
  console.error(`checks: ${failed} of ${ran} FAILED`);
  Deno.exit(1);
}
console.log(`checks: ${ran}/${EXPECTED} OK`);
