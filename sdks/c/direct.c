/*
 * direct.c — patala from C, in-process, through the C ABI.
 *
 * This is an EXAMPLE, not a test. The test is patala-ffi/ctest/smoke.c: it
 * dlopens the artifact, resolves all six symbols by name, and asserts the
 * number of checks it ran, which is what catches a renamed export or a header
 * that has drifted. This file instead does the thing a program actually does —
 * it LINKS the library and walks one payout from an address the customer typed
 * to a receipt you can re-check a year later.
 *
 * Everything runs on MockRail: deterministic, offline, no credentials, no
 * network. This is a payments library, and an example that moves real value is
 * not an example.
 *
 * The flow, in the order a correct integration performs it:
 *
 *   0. check the ABI version                  (a stale library on the load path)
 *   1. open a rail
 *   2. capabilities  -> decide the whole UX   (card form vs wallet address)
 *   3. caveat        -> the sentence to show beside the address field
 *   4. validate-destination -> pre-flight, before any money moves
 *   5. quote         -> what it will cost
 *   6. charge        -> the Receipt IS the entitlement; store it
 *   7. verify        -> gate on this, not on charge having returned
 *   8. verify again later, and on a tampered copy
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "jsonpeek.h"
#include "patala.h"

#ifndef PATALA_EXPECTED_VERSION
#define PATALA_EXPECTED_VERSION "0.0.0-unset"
#endif

/* The rail. NULL or "" would mean the same offline default; it is spelled out
 * here so the shape of a real configuration is visible. Unknown fields are
 * REFUSED, so a typo is an error rather than a rail built from defaults you
 * did not choose. */
static const char *RAIL_CONFIG =
    "{\"rail\":\"mock\",\"currencies\":[\"USDC\",\"USD\"]}";

/* What the customer typed into the payout form. */
static const char *CUSTOMER_ADDRESS = "mock:wallet:alice";

static const char *PAY_REQUEST =
    "{\"amount_minor\":1250,\"currency\":\"USDC\","
    "\"destination\":\"mock:wallet:alice\",\"reference\":\"order-1\"}";

int main(void) {
	int rc = 1;
	uint64_t h = 0;
	char *err = NULL;
	char *caps = NULL, *caveat = NULL, *verdict = NULL;
	char *quote = NULL, *receipt = NULL, *valid = NULL;
	char buf[512];

	/* --- 0. the version probe ------------------------------------------
	 * A shared library is resolved off a load path you may not control. A
	 * stale libpatala_ffi earlier on that path is called silently and then
	 * misbehaves in ways that look like patala bugs. One call rules it out. */
	printf("patala direct (C, in-process) — libpatala_ffi %s\n", patala_abi_version());
	if (patala_abi_check(PATALA_EXPECTED_VERSION, &err) != 0) {
		fprintf(stderr, "abi:       %s\n", err);
		goto done; /* err is freed at `done`, like everything else */
	}
	printf("abi:       matches %s\n", PATALA_EXPECTED_VERSION);

	/* --- 1. open a rail -------------------------------------------------
	 * Nothing happens on a network here. No socket, no thread, no file. */
	h = patala_new(RAIL_CONFIG, &err);
	if (h == 0) {
		fprintf(stderr, "open:      %s\n", err);
		goto done;
	}
	printf("rail:      handle %llu\n", (unsigned long long)h);

	/* --- 2. capabilities decide the UX ----------------------------------
	 * Not a bool. "CustodialReversible" means a card form and a refundable
	 * pending state; "NonCustodialFinal" means a wallet address and a signed
	 * final receipt. Those are not two shades of one thing, and this is how
	 * you branch without knowing which provider answered. */
	caps = patala_call(h, "capabilities", NULL, &err);
	if (!caps) {
		fprintf(stderr, "caps:      %s\n", err);
		goto done;
	}
	{
		char class_name[64];
		int holds_funds = 1;
		json_string(caps, "class", class_name, sizeof(class_name));
		json_bool(caps, "holds_funds", &holds_funds);
		printf("caps:      %s / %s\n", class_name,
		       strcmp(class_name, "CustodialReversible") == 0
		           ? "card form, refundable pending state"
		           : "wallet address, signed final receipt");
		printf("           holds_funds=%s — patala itself never holds funds\n",
		       holds_funds ? "true" : "false");
	}

	/* --- 3. the caveat, before there is a verdict ------------------------
	 * The wording to show on the form where the address is first asked for.
	 * It exists as its own method precisely because at that moment there is
	 * nothing to validate yet. */
	caveat = patala_call(h, "caveat", NULL, &err);
	if (!caveat) {
		fprintf(stderr, "caveat:    %s\n", err);
		goto done;
	}
	{
		char text[512];
		json_string(caveat, "exchange_deposit_caveat", text, sizeof(text));
		printf("caveat:    %.72s...\n", text);
	}

	/* --- 4. the pre-flight check ----------------------------------------
	 * Offline, and it NEVER fails: "I cannot check this" comes back as the
	 * verdict Unknown, not as an error, because a caller must handle it as
	 * carefully as a refusal and an error is too easy to swallow. */
	snprintf(buf, sizeof(buf), "{\"destination\":\"%s\"}", CUSTOMER_ADDRESS);
	verdict = patala_call(h, "validate-destination", buf, &err);
	if (!verdict) {
		fprintf(stderr, "dest:      %s\n", err);
		goto done;
	}
	{
		char status[64];
		int is_refusal = 1, must_confirm = 0;
		json_string(verdict, "status", status, sizeof(status));
		json_bool(verdict, "is_refusal", &is_refusal);
		json_bool(verdict, "human_must_confirm", &must_confirm);
		printf("dest:      %s -> %s (is_refusal=%s, human_must_confirm=%s)\n",
		       CUSTOMER_ADDRESS, status, is_refusal ? "true" : "false",
		       must_confirm ? "true" : "false");
		if (is_refusal) {
			printf("           refused — do not send. Stop here.\n");
			goto done;
		}
		/* human_must_confirm is true on EVERY verdict, including
		 * StructurallyValid. patala does not detect exchange-owned addresses
		 * and will not guess, so a person still has to say yes. */
	}

	/* --- 5. quote --------------------------------------------------------
	 * Integer minor units on the wire. json_u64, never a double — see
	 * jsonpeek.h. */
	quote = patala_call(h, "quote", PAY_REQUEST, &err);
	if (!quote) {
		fprintf(stderr, "quote:     %s\n", err);
		goto done;
	}
	{
		uint64_t amount = 0, fee = 0, total = 0;
		char currency[16] = "";
		json_u64(quote, "amount_minor", &amount);
		json_u64(quote, "fee_minor", &fee);
		json_u64(quote, "total_minor", &total);
		json_string(quote, "currency", currency, sizeof(currency));
		printf("quote:     %llu + %llu fee = %llu minor units of %s\n",
		       (unsigned long long)amount, (unsigned long long)fee,
		       (unsigned long long)total, currency);
	}

	/* --- 6. charge -------------------------------------------------------
	 * The Receipt is the entitlement. Store the whole JSON document; it is
	 * what you hand back to "verify" later. */
	receipt = patala_call(h, "charge", PAY_REQUEST, &err);
	if (!receipt) {
		fprintf(stderr, "charge:    %s\n", err);
		goto done;
	}
	{
		char reference[64] = "", rail_id[32] = "";
		uint64_t amount = 0;
		json_string(receipt, "reference", reference, sizeof(reference));
		json_string(receipt, "rail_id", rail_id, sizeof(rail_id));
		json_u64(receipt, "amount_minor", &amount);
		printf("charge:    %llu minor units, ref=%s, issued by rail %s\n",
		       (unsigned long long)amount, reference, rail_id);
	}

	/* --- 7. verify -------------------------------------------------------
	 * Gate entitlement on THIS, never on charge having returned non-NULL. */
	valid = patala_call(h, "verify", receipt, &err);
	if (!valid) {
		fprintf(stderr, "verify:    %s\n", err);
		goto done;
	}
	printf("verify:    %s  <- the entitlement check\n", valid);
	patala_free(valid);
	valid = NULL;

	/* --- 8. the two outcomes that are NOT errors -------------------------
	 * A tampered receipt comes back as a RESULT — {"valid":false} — not as
	 * NULL. That is deliberate: NULL means "I could not perform the check"
	 * (retry it), while {"valid":false} means "I checked and it does not
	 * hold" (never retry, never grant). Collapsing the two is how an unpaid
	 * order becomes an entitlement. */
	{
		const char *needle = "\"amount_minor\":1250";
		const char *at = strstr(receipt, needle);
		if (at) {
			char tampered[1024];
			size_t head = (size_t)(at - receipt);
			snprintf(tampered, sizeof(tampered), "%.*s\"amount_minor\":999999%s",
			         (int)head, receipt, at + strlen(needle));
			valid = patala_call(h, "verify", tampered, &err);
			printf("tampered:  %s  <- a result, not NULL; a refusal is DATA\n",
			       valid ? valid : "(NULL — WRONG)");
			patala_free(valid);
			valid = NULL;
		}
	}

	/* An operational refusal, by contrast, IS NULL with a readable message —
	 * and the message names the currency rather than making you guess. */
	{
		char *nope = patala_call(h, "charge",
		                         "{\"amount_minor\":100,\"currency\":\"EUR\","
		                         "\"destination\":\"mock:wallet:alice\","
		                         "\"reference\":\"order-2\"}",
		                         &err);
		printf("refused:   %s\n", nope ? "(a receipt — WRONG)" : err);
		patala_free(nope);
		patala_free(err);
		err = NULL;
	}

	printf("\nOK — offline, MockRail only, no value moved.\n");
	rc = 0;

	/* --- one cleanup label ----------------------------------------------
	 * C has no destructor, so the guarantee every other patala SDK makes with
	 * `Drop`, `deinit` or a `with` block is made here by having exactly one
	 * exit path. Every non-const char* the library returned — results AND
	 * error messages — goes back through patala_free and nothing else. Not
	 * free(): this is Rust's allocator. patala_free(NULL) is safe, which is
	 * why none of these are guarded. */
done:
	patala_free(err);
	patala_free(caps);
	patala_free(caveat);
	patala_free(verdict);
	patala_free(quote);
	patala_free(receipt);
	patala_free(valid);
	patala_close(h); /* closing 0, or closing twice, is a no-op */
	return rc;
}
