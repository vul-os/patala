/*
 * smoke.c — the C smoke test for libpatala_ffi.
 *
 * It dlopens the built shared library, resolves the six exported symbols BY
 * NAME, and drives a real MockRail charge -> verify round trip end to end.
 * Without this, the ABI would be unverified: every Rust test in patala-ffi
 * passes just as well with a missing #[no_mangle], a renamed symbol, or a
 * header that has drifted from the library, because a Rust test calls the Rust
 * function directly and never goes through the C surface at all.
 *
 * It also checks the claim patala's C ABI is actually sold on — that loading
 * this library puts no runtime in the host process — by counting the process's
 * threads before dlopen, after dlopen, and after a full round trip.
 *
 * Usage:
 *   smoke <library-path> <expected-version>
 *
 * No fake server and no fixtures: MockRail is deterministic and offline, which
 * is the whole reason it exists (PATALA.md §8).
 *
 * The test ends by asserting the NUMBER of checks that actually ran. A C
 * program that returns 0 having executed nothing is the classic false green,
 * and an early `return` after a dlsym failure is exactly the kind of thing
 * that hides.
 */

#define _POSIX_C_SOURCE 200809L

#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "patala.h"

/* ------------------------------------------------------------------ */
/* Counting the process's threads, so "no runtime in your process" is a
 * test rather than a sentence in a README. Deliberately no #else that
 * returns "unknown": an unsupported platform must FAIL and be taught about
 * this, not quietly skip the one check that backs the headline claim. */

#if defined(__APPLE__)
#include <mach/mach.h>
static int thread_count(void) {
	thread_act_array_t threads;
	mach_msg_type_number_t n;
	if (task_threads(mach_task_self(), &threads, &n) != KERN_SUCCESS) {
		return -1;
	}
	for (mach_msg_type_number_t i = 0; i < n; i++) {
		mach_port_deallocate(mach_task_self(), threads[i]);
	}
	vm_deallocate(mach_task_self(), (vm_address_t)threads, n * sizeof(thread_t));
	return (int)n;
}
#elif defined(__linux__)
#include <dirent.h>
static int thread_count(void) {
	DIR *d = opendir("/proc/self/task");
	if (!d) return -1;
	int n = 0;
	struct dirent *e;
	while ((e = readdir(d)) != NULL) {
		if (e->d_name[0] != '.') n++;
	}
	closedir(d);
	return n;
}
#else
static int thread_count(void) { return -1; }
#endif

/* ------------------------------------------------------------------ */

static int checks_run = 0;
static int failures = 0;

static void ok(const char *what) {
	checks_run++;
	printf("  ok   %s\n", what);
}

static void bad(const char *what, const char *detail) {
	checks_run++;
	failures++;
	printf("  FAIL %s: %s\n", what, detail ? detail : "(no detail)");
}

static void check(int cond, const char *what, const char *detail) {
	if (cond) {
		ok(what);
	} else {
		bad(what, detail);
	}
}

static patala_abi_version_fn p_abi_version;
static patala_abi_check_fn   p_abi_check;
static patala_new_fn         p_new;
static patala_close_fn       p_close;
static patala_call_fn        p_call;
static patala_free_fn        p_free;

/* A charge request for `amount` minor units. Returned in a static buffer;
 * one at a time is all this test needs. */
static const char *pay_request(unsigned long amount, const char *currency, const char *reference) {
	static char buf[512];
	snprintf(buf, sizeof(buf),
	         "{\"amount_minor\":%lu,\"currency\":\"%s\",\"destination\":\"mock:wallet:alice\",\"reference\":\"%s\"}",
	         amount, currency, reference);
	return buf;
}

int main(int argc, char **argv) {
	if (argc != 3) {
		fprintf(stderr, "usage: %s <library-path> <expected-version>\n", argv[0]);
		return 2;
	}
	const char *libpath  = argv[1];
	const char *want_ver = argv[2];

	printf("patala C smoke test\n  library: %s\n", libpath);

	const int threads_before = thread_count();
	if (threads_before < 1) {
		fprintf(stderr,
		        "smoke: FAIL - this platform has no thread_count() implementation, so the "
		        "\"no runtime in your process\" claim cannot be checked here. Teach "
		        "ctest/smoke.c about it; do not skip it.\n");
		return 1;
	}

	void *lib = dlopen(libpath, RTLD_NOW | RTLD_LOCAL);
	if (!lib) {
		fprintf(stderr, "dlopen(%s) failed: %s\n", libpath, dlerror());
		return 1;
	}

	/* Resolve every symbol the header promises. A missing one is fatal: the
	 * remaining checks would crash rather than report. */
	struct { const char *name; void **slot; } syms[] = {
		{"patala_abi_version", (void **)&p_abi_version},
		{"patala_abi_check",   (void **)&p_abi_check},
		{"patala_new",         (void **)&p_new},
		{"patala_close",       (void **)&p_close},
		{"patala_call",        (void **)&p_call},
		{"patala_free",        (void **)&p_free},
	};
	for (size_t i = 0; i < sizeof(syms) / sizeof(syms[0]); i++) {
		dlerror();
		*syms[i].slot = dlsym(lib, syms[i].name);
		const char *e = dlerror();
		if (e || !*syms[i].slot) {
			fprintf(stderr, "dlsym(%s) failed: %s\n", syms[i].name, e ? e : "NULL");
			return 1;
		}
		ok(syms[i].name);
	}

	/* --- 1. loading it started nothing ----------------------------------- */
	{
		const int after_load = thread_count();
		char detail[160];
		snprintf(detail, sizeof(detail), "threads went %d -> %d across dlopen", threads_before, after_load);
		check(after_load > 0 && after_load <= threads_before,
		      "dlopen started no threads (no runtime in the host process)", detail);
	}

	/* --- 2. version probe ------------------------------------------------ */
	{
		const char *got = p_abi_version();
		if (got && strcmp(got, want_ver) == 0) {
			ok("patala_abi_version matches the package version");
		} else {
			char detail[256];
			snprintf(detail, sizeof(detail),
			         "library says %s, this tree is %s - a stale library is on the load path",
			         got ? got : "(null)", want_ver);
			bad("patala_abi_version matches the package version", detail);
		}

		char *err = NULL;
		check(p_abi_check(want_ver, &err) == 0, "patala_abi_check accepts the matching version",
		      err ? err : "it reported a mismatch");
		check(err == NULL, "a matching patala_abi_check leaves *err untouched", err ? err : "");
		if (err) p_free(err);

		err = NULL;
		check(p_abi_check("0.0.1-stale", &err) == -1, "patala_abi_check refuses a wrong version",
		      "it reported a match");
		check(err != NULL && strstr(err, "0.0.1-stale") != NULL,
		      "the mismatch message names the version the caller expected", err ? err : "(NULL)");
		if (err) p_free(err);
	}

	/* --- 3. a bad configuration is refused, with a message --------------- */
	{
		char *err = NULL;
		uint64_t h = p_new("{\"rail\": ", &err);
		check(h == 0, "patala_new refuses a malformed configuration", "it returned a handle");
		check(err != NULL, "patala_new sets *err on failure", "err was left NULL");
		if (err) p_free(err);

		/* Fail-closed: an unknown field is refused, not defaulted. */
		err = NULL;
		h = p_new("{\"rail\":\"mock\",\"currencys\":[\"USDC\"]}", &err);
		check(h == 0, "a misspelled config field is refused, not silently defaulted",
		      "it built a rail from a config it did not understand");
		if (err) p_free(err);

		/* A rail this build has no feature for names the feature. */
		err = NULL;
		h = p_new("{\"rail\":\"dogecoin\"}", &err);
		check(h == 0, "an unknown rail kind is refused", "it returned a handle");
		if (err) p_free(err);
	}

	/* --- 4. open the offline default -------------------------------------- */
	char *err = NULL;
	uint64_t h = p_new("{\"rail\":\"mock\",\"currencies\":[\"USDC\",\"USD\"]}", &err);
	if (h == 0) {
		fprintf(stderr, "patala_new failed: %s\n", err ? err : "(no error message)");
		if (err) p_free(err);
		return 1;
	}
	ok("patala_new returned a handle");

	{
		char *err2 = NULL;
		uint64_t d = p_new(NULL, &err2);
		check(d != 0, "a NULL configuration means the offline default, not a failure",
		      err2 ? err2 : "it returned 0");
		if (err2) p_free(err2);
		if (d) p_close(d);
	}

	/* --- 5. id + capabilities --------------------------------------------- */
	{
		err = NULL;
		char *out = p_call(h, "id", NULL, &err);
		check(out != NULL && strstr(out, "\"rail_id\":\"mock\"") != NULL,
		      "id names the rail", out ? out : (err ? err : "NULL"));
		if (out) p_free(out);
		if (err) p_free(err);

		err = NULL;
		out = p_call(h, "capabilities", NULL, &err);
		check(out != NULL, "capabilities returned a result", err ? err : "NULL with no error");
		if (out) {
			check(strstr(out, "\"class\":\"NonCustodialFinal\"") != NULL,
			      "the settlement class crosses as a name, not a bool", out);
			check(strstr(out, "\"holds_funds\":false") != NULL,
			      "a non-custodial rail reports holding no funds", out);
			p_free(out);
		}
		if (err) p_free(err);
	}

	/* --- 6. quote -> charge -> verify, the real round trip ---------------- */
	char *receipt = NULL;
	{
		err = NULL;
		char *quote = p_call(h, "quote", pay_request(1250, "USDC", "c-smoke-1"), &err);
		check(quote != NULL, "quote returned a result", err ? err : "NULL with no error");
		if (quote) {
			check(strstr(quote, "\"total_minor\":1250") != NULL,
			      "the amount is an integer in minor units, never a float", quote);
			p_free(quote);
		}
		if (err) p_free(err);

		err = NULL;
		receipt = p_call(h, "charge", pay_request(1250, "USDC", "c-smoke-1"), &err);
		check(receipt != NULL, "charge returned a receipt", err ? err : "NULL with no error");
		if (receipt) {
			check(strstr(receipt, "\"amount_minor\":1250") != NULL,
			      "the receipt carries the amount charged", receipt);
			check(strstr(receipt, "\"rail_id\":\"mock\"") != NULL,
			      "the receipt names the rail that issued it", receipt);
		}
		if (err) p_free(err);
	}

	if (!receipt) {
		printf("\nFAIL: no receipt, so the rest of the round trip cannot run\n");
		return 1;
	}

	{
		err = NULL;
		char *out = p_call(h, "verify", receipt, &err);
		check(out != NULL && strcmp(out, "{\"valid\":true}") == 0,
		      "a genuine receipt verifies true", out ? out : (err ? err : "NULL"));
		if (out) p_free(out);
		if (err) p_free(err);
	}

	/* *err must be NULL after a call that SUCCEEDED, and not merely "nobody
	 * has failed since you last cleared it". patala_call only ever wrote *err
	 * on the failing path, so a host that branches on `err != NULL` rather
	 * than on the return value keeps the first error it ever hit forever: the
	 * charge below succeeds and still reports "unsupported currency".
	 *
	 * Deliberately does NOT reset err between the two calls, because that is
	 * exactly what the buggy host does not do. */
	{
		err = NULL;
		char *bad = p_call(h, "charge",
		                   "{\"amount_minor\":1250,\"currency\":\"EUR\","
		                   "\"destination\":\"mock:wallet:alice\",\"reference\":\"stale-err\"}",
		                   &err);
		check(bad == NULL && err != NULL, "the setup call really did fail",
		      bad ? bad : "(NULL, but *err was not set)");
		if (bad) p_free(bad);
		char *stale = err; /* left in place on purpose; freed below */

		char *out = p_call(h, "verify", receipt, &err);
		check(err == NULL,
		      "a SUCCEEDING patala_call clears *err, so err != NULL means THIS call failed",
		      err ? err : "");
		check(out != NULL && strcmp(out, "{\"valid\":true}") == 0,
		      "...and it still returned the right answer", out ? out : "NULL");
		if (out) p_free(out);
		if (err != stale && err) p_free(err);
		p_free(stale);
		err = NULL;
	}

	/* Fail-closed, and NOT as an error: a rail's honest "this did not settle"
	 * is data. Tamper by rewriting the amount in the receipt JSON. */
	{
		char tampered[1024];
		const char *needle = "\"amount_minor\":1250";
		const char *at = strstr(receipt, needle);
		check(at != NULL, "the receipt contains the amount to tamper with", receipt);
		if (at) {
			size_t head = (size_t)(at - receipt);
			snprintf(tampered, sizeof(tampered), "%.*s\"amount_minor\":999999%s",
			         (int)head, receipt, at + strlen(needle));
			err = NULL;
			char *out = p_call(h, "verify", tampered, &err);
			check(out != NULL, "verifying a tampered receipt is not an ERROR",
			      err ? err : "it returned NULL - a caller could mistake that for a transport failure");
			if (out) {
				check(strcmp(out, "{\"valid\":false}") == 0,
				      "a tampered receipt verifies false (fail-closed)", out);
				p_free(out);
			}
			if (err) p_free(err);
		}
	}
	p_free(receipt);

	/* --- 7. a currency the rail does not support is a readable error ------ */
	{
		err = NULL;
		char *out = p_call(h, "charge", pay_request(100, "EUR", "c-smoke-2"), &err);
		check(out == NULL, "an unsupported currency is refused", "it returned a receipt");
		check(err != NULL && strstr(err, "EUR") != NULL,
		      "the refusal names the currency", err ? err : "(NULL)");
		if (out) p_free(out);
		if (err) p_free(err);
	}

	/* --- 8. the destination pre-flight ------------------------------------ */
	{
		err = NULL;
		char *out = p_call(h, "validate-destination", "{\"destination\":\"stellar:wallet:alice\"}", &err);
		check(out != NULL, "validate-destination returned a verdict", err ? err : "NULL");
		if (out) {
			check(strstr(out, "\"status\":\"WrongNetwork\"") != NULL,
			      "an address for another chain is WrongNetwork", out);
			check(strstr(out, "\"is_refusal\":true") != NULL,
			      "is_refusal crosses as DATA, not something the caller re-derives", out);
			p_free(out);
		}
		if (err) p_free(err);

		err = NULL;
		out = p_call(h, "validate-destination", "{\"destination\":\"mock:wallet:alice\"}", &err);
		check(out != NULL && strstr(out, "\"status\":\"StructurallyValid\"") != NULL,
		      "a well-formed address on this rail's own network is StructurallyValid",
		      out ? out : (err ? err : "NULL"));
		if (out) {
			check(strstr(out, "\"human_must_confirm\":true") != NULL,
			      "even StructurallyValid still requires a human to confirm", out);
			check(strstr(out, "\"exchange_deposit_caveat\":\"") != NULL,
			      "every verdict carries the caveat to show that human", out);
			p_free(out);
		}
		if (err) p_free(err);

		/* "I cannot check" is a VERDICT, not an error - the fiat shape. */
		err = NULL;
		uint64_t opaque = p_new("{\"rail\":\"mock\",\"destination_checks\":false}", &err);
		check(opaque != 0, "a rail that cannot check destinations is constructible",
		      err ? err : "it returned 0");
		if (err) p_free(err);
		if (opaque) {
			err = NULL;
			out = p_call(opaque, "validate-destination", "{\"destination\":\"cus_opaque_token\"}", &err);
			check(out != NULL && strstr(out, "\"status\":\"Unknown\"") != NULL,
			      "a rail that cannot check says Unknown rather than erroring",
			      out ? out : (err ? err : "NULL"));
			if (out) {
				check(strstr(out, "\"is_refusal\":false") != NULL,
				      "Unknown is not a refusal - and is not a green light either", out);
				p_free(out);
			}
			if (err) p_free(err);
			p_close(opaque);
		}

		/* Fail-closed on the request itself. */
		err = NULL;
		out = p_call(h, "validate-destination", "{\"destinaton\":\"typo\"}", &err);
		check(out == NULL && err != NULL,
		      "a misspelled request field is refused, not answered about the wrong field",
		      out ? out : "it returned a verdict");
		if (out) p_free(out);
		if (err) p_free(err);
	}

	/* --- 9. the caveat is reachable before there is a verdict ------------- */
	{
		err = NULL;
		char *out = p_call(h, "caveat", NULL, &err);
		check(out != NULL && strstr(out, "exchange") != NULL,
		      "caveat returns the wording for the form where an address is first asked for",
		      out ? out : (err ? err : "NULL"));
		if (out) p_free(out);
		if (err) p_free(err);
	}

	/* --- 10. a rail with no push delivery refuses rather than inventing --- */
	{
		err = NULL;
		char *out = p_call(h, "webhook", "{\"body\":\"{}\",\"headers\":{},\"now_unix\":1700000000}", &err);
		check(out == NULL, "the mock has no processor, so no webhook event is invented",
		      "it returned an event");
		check(err != NULL && strstr(err, "verify_webhook") != NULL,
		      "the refusal says the operation is unsupported", err ? err : "(NULL)");
		if (out) p_free(out);
		if (err) p_free(err);
	}

	/* --- 11. errors are errors, not crashes ------------------------------- */
	{
		err = NULL;
		char *out = p_call(h, "no-such-method", "{}", &err);
		check(out == NULL, "patala_call rejects an unknown method", "it returned a result");
		check(err != NULL && strstr(err, "unknown method") != NULL,
		      "an unknown method sets a readable *err", err ? err : "(NULL)");
		if (err) p_free(err);

		err = NULL;
		out = p_call(999999, "capabilities", NULL, &err);
		check(out == NULL, "patala_call rejects an invented handle", "it returned a result");
		check(err != NULL && strstr(err, "unknown handle") != NULL,
		      "an invented handle is a clean error, not a segfault", err ? err : "(NULL)");
		if (err) p_free(err);

		/* A NULL err is a legitimate "I do not want the message". */
		out = p_call(999999, "capabilities", NULL, NULL);
		check(out == NULL, "a NULL err pointer is accepted rather than dereferenced",
		      "it returned a result");
	}

	/* --- 12. no runtime, still ------------------------------------------- */
	{
		const int after_work = thread_count();
		char detail[160];
		snprintf(detail, sizeof(detail), "threads went %d -> %d across a full round trip",
		         threads_before, after_work);
		check(after_work > 0 && after_work <= threads_before,
		      "a full charge/verify round trip started no threads either", detail);
	}

	/* --- 13. close, twice -------------------------------------------------- */
	p_close(h);
	p_close(h); /* must be a no-op, not a double free */
	ok("patala_close is idempotent");
	{
		err = NULL;
		char *out = p_call(h, "capabilities", NULL, &err);
		check(out == NULL && err != NULL, "use-after-close is a clean error",
		      "the closed handle answered");
		if (out) p_free(out);
		if (err) p_free(err);
	}

	/* patala_free(NULL) must be safe: bindings call it on every path. */
	p_free(NULL);
	ok("patala_free(NULL) is safe");

	/* --- the gate on this test itself -------------------------------------- */
	/* Update this when checks are added. It exists because a C test that exits
	 * 0 having run three of its checks looks exactly like one that ran them
	 * all. */
	const int EXPECTED_CHECKS = 58;
	printf("\n%d checks ran, %d failed (expected %d checks)\n", checks_run, failures, EXPECTED_CHECKS);
	if (checks_run != EXPECTED_CHECKS) {
		printf("FAIL: the smoke test ran %d checks, not %d - it took an early exit "
		       "or a check was removed without updating EXPECTED_CHECKS\n", checks_run, EXPECTED_CHECKS);
		return 1;
	}
	if (failures) {
		printf("FAIL: %d check(s) failed\n", failures);
		return 1;
	}
	printf("PASS\n");
	return 0;
}
