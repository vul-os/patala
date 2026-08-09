/*
 * patala.h — the stable C ABI of libpatala_ffi.
 *
 * patala is a sovereign, centerless payment-rail substrate: one PaymentRail
 * trait, several rails behind it (a deterministic offline mock, SPL-USDC on
 * Solana, native USDC on Stellar, a self-hosted Hyperswitch, and twenty direct
 * fiat processors), and one rule — patala itself never holds funds.
 *
 * Rust hosts use `patala-core` directly. Python, Go, Swift, Kotlin and Ruby
 * use the UniFFI bindings generated from `patala-uniffi`. C, C++,
 * Node/Deno/Bun, PHP and Elixir have no UniFFI backend, so they load THIS
 * library and run patala IN THEIR OWN PROCESS — or, if in-process is the wrong
 * shape for them, talk to the `patala-sidecar` HTTP server instead. Both are
 * supported answers.
 *
 * This header is hand-written and is the supported surface.
 *
 * WHAT LOADING THIS LIBRARY COSTS YOU
 *
 *   Almost nothing, and this is the unusual part, so it is stated up front
 *   rather than buried:
 *
 *     - There is NO language runtime in your process. patala is Rust. No
 *       garbage collector, no scheduler, no green threads.
 *     - NO signal handlers are installed. Your SIGSEGV/SIGPROF handling is
 *       untouched, so JVM hosts and profiling Python builds have nothing to
 *       conflict with.
 *     - It is fork-safe in the way that matters: nothing of ours is running at
 *       fork() time, so Python multiprocessing's default "fork" start method,
 *       uWSGI and Unicorn need no special handling. (Handles are not inherited
 *       usefully across a fork; open them in the child.)
 *     - NO threads are started, at load time or at any other time. Each handle
 *       owns a current-thread async runtime, which drives work on whichever
 *       thread called in.
 *     - Nothing happens at load: no socket, no file, no background task.
 *
 *   If you have read the equivalent header for llmux or openrate, those carry
 *   a list of Go-runtime caveats. Those products are Go and use
 *   `-buildmode=c-shared`; the caveats are real there. They do not apply here
 *   and have deliberately not been copied.
 *
 * CONVENTIONS
 *
 *   Strings   UTF-8, NUL-terminated.
 *   Documents Requests and responses are JSON — the SAME JSON the
 *             `patala-sidecar` HTTP API uses, built from the same Rust types.
 *             A body that works against POST /v1/rails/:id/charge works here
 *             unchanged, and the response is what that endpoint returns.
 *   Errors    Functions that can fail take a trailing `char** err`. On failure
 *             they write a malloc'd, human-readable UTF-8 message there. Pass
 *             NULL for err if you do not want the message. The message is NOT
 *             JSON; do not parse it.
 *   Ownership Every non-const char* this library returns — results AND error
 *             messages — is freed with patala_free, and with nothing else. In
 *             particular NOT with your own free(): this is Rust's allocator.
 *             patala_abi_version is the one exception: it returns a static
 *             string you must NOT free.
 *   Handles   Integers in a registry inside the library, never pointers.
 *             Calling with a closed or invented handle is a clean error, not a
 *             crash. Handles are never reused, so a use-after-close says so
 *             instead of landing on someone else's rail.
 *   Returns   Functions returning int use 0 for success and -1 for failure
 *             with *err set. patala_new returns 0 for FAILURE because its
 *             success value is a handle, and handles start at 1.
 *   Threads   A handle is safe to use from several threads at once. Calls on
 *             ONE handle serialise (that handle owns one runtime); calls on
 *             different handles run concurrently. Open one handle per rail,
 *             and more than one if you want parallelism on the same rail.
 *   Panics    A bug in patala becomes an error string, not a crash in your
 *             process: every entry point catches unwinding at the boundary.
 *
 * MONEY
 *
 *   Amounts are integer minor units (`amount_minor`) plus a currency string.
 *   Never a float, on either side of this boundary. JSON numbers here are
 *   integers; parse them as such.
 *
 *   `verify` returning {"valid": false} is NOT an error — it is the rail's
 *   honest, fail-closed answer that a receipt does not hold. Gate entitlement
 *   on {"valid": true} and nothing else. Do not treat a false as a transient
 *   failure to retry.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#ifndef PATALA_H
#define PATALA_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Returns the patala version this library was built from, e.g. "0.1.0", as a
 * static string. Do NOT free it.
 *
 * Compare it against the version your bindings were generated for. A shared
 * library is resolved off a load path you may not control; without this probe
 * a stale libpatala_ffi earlier on that path is called silently and misbehaves
 * in ways that look like patala bugs.
 */
const char* patala_abi_version(void);

/*
 * The same check as one call: returns 0 when this library's version equals
 * `expected`, or -1 with *err set to a message naming both. It exists so the
 * comparison is not reimplemented — and forgotten — in each binding.
 */
int patala_abi_check(const char* expected, char** err);

/*
 * Builds a rail and returns its handle, or 0 on failure with *err set.
 *
 * config_json is a JSON object tagged by "rail". NULL or "" means the offline
 * default: a deterministic MockRail on USDC, which needs no credentials and no
 * network, so a consumer in any language can drive a full charge -> verify
 * round trip before a single secret exists.
 *
 * Bytes that are not valid UTF-8 are REFUSED (0 with *err set) rather than
 * treated as absent. The distinction is load-bearing: absent means the offline
 * MockRail, so collapsing the two turned a corrupt configuration asking for a
 * real processor into a mock that reported every charge as settled. If your
 * host builds this string from a byte-string or a latin-1 source, that is the
 * path — encode it as UTF-8 first.
 *
 *   {"rail":"mock"}
 *   {"rail":"mock", "id":"mock", "class":"non-custodial-final",
 *    "currencies":["USDC","USD"], "fee_minor":0, "failing":false,
 *    "destination_checks":true}
 *
 *     class              "non-custodial-final" (default) | "custodial-reversible"
 *     failing            make every operation fail, for exercising your error path
 *     destination_checks false gives a rail that answers "Unknown" to every
 *                        destination — the offline stand-in for a fiat rail,
 *                        whose destination is an opaque processor-side token
 *
 *   {"rail":"fiat", "provider":"stripe", "config":{"secret_key":"…","webhook_secret":"…"}}
 *   {"rail":"solana", "rpc_url":"…", "cluster":"devnet"|"mainnet",
 *    "keypair_seed_hex":"<64 hex chars>"}
 *   {"rail":"stellar", "horizon_url":"…", "network":"testnet"|"public",
 *    "usdc_issuer":"…", "keypair_seed_hex":"<64 hex chars>"}
 *   {"rail":"hyperswitch", "base_url":"…", "api_key":"…", "connector":"…"}
 *
 * Everything but "mock" needs the matching Cargo feature compiled into the
 * library you loaded. A build without it refuses by name and tells you which
 * feature is missing — it never falls back to a different rail.
 *
 * Unknown fields are REFUSED rather than ignored. A misspelled "currencys" is
 * an error, not a rail quietly built with a default currency list you did not
 * choose.
 *
 * Creating a rail talks to nothing: no socket is opened, no thread is started.
 * Only patala_call reaches a network, and only for a rail that has one.
 */
uint64_t patala_new(const char* config_json, char** err);

/*
 * Releases the rail behind h. Closing an unknown or already-closed handle is a
 * no-op, so cleanup paths can be idempotent.
 */
void patala_close(uint64_t h);

/*
 * One unary call. Returns malloc'd UTF-8 JSON (free with patala_free), or NULL
 * with *err set.
 *
 *   method                  request_json                 result
 *   ------------------------------------------------------------------------
 *   "id"                    ignored (NULL)               {"rail_id":"mock"}
 *   "capabilities"          ignored (NULL)               RailCapabilities
 *   "quote"                 PayRequest                   Quote
 *   "charge"                PayRequest                   Receipt
 *   "verify"                Receipt                      {"valid":true|false}
 *   "validate-destination"  {"destination":"…"}          DestinationVerdict + is_refusal
 *   "webhook"               see below                    WebhookEvent
 *   "caveat"                ignored (NULL)               {"exchange_deposit_caveat":"…"}
 *   "providers"             ignored (NULL)               {"providers":[…]}  (fiat builds)
 *
 * `method` is a string rather than one C function per operation so this header
 * stays stable as patala grows methods.
 *
 * PayRequest is {"amount_minor":1250,"currency":"USDC","destination":"…",
 * "reference":"your-order-id"}. Receipt is what "charge" returned; store it
 * and hand it back to "verify" later — that, not "charge" having returned OK,
 * is the entitlement check.
 *
 * "capabilities" is how you decide your whole UX without knowing which
 * provider answered: a "CustodialReversible" rail means a card form and a
 * refundable pending state; a "NonCustodialFinal" rail means a wallet address
 * and a signed final receipt. It is not a bool, because those are not two
 * shades of one thing.
 *
 * "validate-destination" is the offline pre-flight check to run before any
 * money moves, and it NEVER fails: "I cannot check this" comes back as the
 * verdict {"status":"Unknown"}, not as an error, because a caller must handle
 * it as carefully as a refusal and an error is too easy to swallow. Read
 * `is_refusal` (do not send) and `human_must_confirm` (true on EVERY verdict,
 * including "StructurallyValid" — patala does not detect exchange-owned
 * addresses and will not guess). `exchange_deposit_caveat` is the sentence to
 * show that human, and "caveat" returns the same text for the form where the
 * address is first asked for, before there is a verdict to render.
 *
 * "webhook" authenticates an inbound delivery from a rail's processor. Forward
 * the request VERBATIM:
 *
 *   {"body":"<the exact request body>",   // or "body_hex":"<hex>" if binary
 *    "headers":{"Stripe-Signature":"…"},  // matched case-insensitively
 *    "query":{},                          // only LNbits reads this
 *    "now_unix":1700000000}               // explicit clock for replay windows
 *
 * Every scheme signs the bytes the processor actually sent, so a body that has
 * been through a JSON round-trip on your side is no longer what was signed.
 * Give exactly one of "body" / "body_hex". A returned WebhookEvent means the
 * rail authenticated the delivery; gate entitlement on its "status" being
 * "Settled". "Unconfirmed" means genuine-but-says-nothing-about-money: take
 * the object_id, find your own record, and call "verify". A rail with no push
 * delivery (the mock, fiat's `manual`) refuses with Unsupported rather than
 * inventing an event.
 */
char* patala_call(uint64_t h, const char* method, const char* request_json, char** err);

/*
 * Frees anything this library returned: results and error messages alike.
 * NULL is accepted and ignored.
 */
void patala_free(char* p);

/*
 * Function-pointer types for hosts that dlopen/LoadLibrary this library rather
 * than linking against it — which is the usual way, since it lets you probe
 * patala_abi_version before committing to the rest.
 */
typedef const char* (*patala_abi_version_fn)(void);
typedef int (*patala_abi_check_fn)(const char*, char**);
typedef uint64_t (*patala_new_fn)(const char*, char**);
typedef void (*patala_close_fn)(uint64_t);
typedef char* (*patala_call_fn)(uint64_t, const char*, const char*, char**);
typedef void (*patala_free_fn)(char*);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* PATALA_H */
