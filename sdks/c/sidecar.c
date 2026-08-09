/*
 * sidecar.c — patala from C, as a child process on loopback.
 *
 * The same payout as direct.c, over HTTP against `patala-sidecar` instead of
 * through the C ABI. Same JSON both ways: the sidecar and libpatala_ffi
 * serialize the same `patala-core` types, so a body that works here works
 * against patala_call unchanged.
 *
 * Why a C program would choose this over linking the library — the honest
 * list, because for C the answer is usually "link it":
 *
 *   - KEY ISOLATION. A non-custodial rail's signing key lives in whichever
 *     process calls charge. If five services link the library, the key is in
 *     five address spaces. One sidecar puts it in one narrow process.
 *   - You cannot ship a shared library for your platform (see README.md's
 *     platform table — a Windows DLL does not exist today).
 *   - Your process is not one you want a payment rail inside at all: a plugin
 *     host, a setuid helper, something a customer can load code into.
 *
 * Notably NOT on that list: fork-safety, signal handlers, or a runtime in your
 * process. patala is Rust; libpatala_ffi installs no signal handlers, starts
 * no threads and has no GC, so none of the reasons llmux's and openrate's C
 * examples give for reaching for a sidecar apply here. This example forks, and
 * it would be safe doing so even if it had loaded the library.
 *
 * This program links NO patala library at all — it only needs a socket.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "jsonpeek.h"
#include "mini_http.h"

static const char *PAY_REQUEST =
    "{\"amount_minor\":1250,\"currency\":\"USDC\","
    "\"destination\":\"mock:wallet:alice\",\"reference\":\"order-1\"}";

static pid_t child = 0;

static void reap(void) {
	if (child > 0) {
		kill(child, SIGTERM);
		waitpid(child, NULL, 0);
		child = 0;
	}
}

/* SIGINT/SIGTERM would otherwise leave an orphaned sidecar holding a port —
 * and, in a real deployment, holding a signing key. C's answer to `Drop`. */
static void on_signal(int sig) {
	reap();
	_exit(128 + sig);
}

/* The sidecar refuses to start without a token: there is no unauthenticated
 * mode and no auto-generated fallback. A real deployment generates this once
 * and keeps it out of the environment of everything that is not the sidecar
 * or its client. */
static int random_token(char *out, size_t outcap) {
	unsigned char raw[32];
	FILE *f = fopen("/dev/urandom", "rb");
	if (!f) return -1;
	size_t got = fread(raw, 1, sizeof(raw), f);
	fclose(f);
	if (got != sizeof(raw) || outcap < sizeof(raw) * 2 + 1) return -1;
	for (size_t i = 0; i < sizeof(raw); i++) {
		snprintf(out + i * 2, 3, "%02x", raw[i]);
	}
	return 0;
}

/* Poll /healthz — the one unauthenticated route — until the child is
 * listening. Mandatory: fork+exec returns long before the socket is bound. */
static int wait_healthy(int port) {
	char errbuf[256];
	for (int i = 0; i < 500; i++) {
		http_response r;
		if (http_request(port, "GET", "/healthz", NULL, NULL, &r, errbuf, sizeof(errbuf)) == 0) {
			int ok = r.status == 200;
			http_response_free(&r);
			if (ok) return 0;
		}
		struct timespec ts = {0, 20 * 1000 * 1000}; /* 20 ms */
		nanosleep(&ts, NULL);
	}
	return -1;
}

int main(int argc, char **argv) {
	const char *bin = argc > 1 ? argv[1] : getenv("PATALA_SIDECAR_BIN");
	if (!bin || !*bin) {
		fprintf(stderr,
		        "usage: %s <path-to-patala-sidecar>\n"
		        "   or: PATALA_SIDECAR_BIN=... %s\n"
		        "build one with:  cargo build -p patala-sidecar --release\n",
		        argv[0], argv[0]);
		return 2;
	}

	int port = http_free_port();
	if (!port) {
		fprintf(stderr, "no free loopback port\n");
		return 1;
	}

	char token[65];
	if (random_token(token, sizeof(token)) != 0) {
		fprintf(stderr, "could not read /dev/urandom\n");
		return 1;
	}

	printf("patala sidecar (C, child process) — 127.0.0.1:%d\n", port);
	printf("binary:    %s\n", bin);

	char port_env[64];
	snprintf(port_env, sizeof(port_env), "%d", port);

	signal(SIGINT, on_signal);
	signal(SIGTERM, on_signal);

	child = fork();
	if (child < 0) {
		fprintf(stderr, "fork: %s\n", strerror(errno));
		return 1;
	}
	if (child == 0) {
		setenv("PATALA_SIDECAR_PORT", port_env, 1);
		setenv("PATALA_SIDECAR_TOKEN", token, 1);
		/* The sidecar logs to stdout; send it to /dev/null so this example's
		 * output is its own. Its stderr is left alone — a refusal to start
		 * (no token, port taken) must still be visible. */
		{
			int devnull = open("/dev/null", O_WRONLY);
			if (devnull >= 0) {
				dup2(devnull, STDOUT_FILENO);
				close(devnull);
			}
		}
		execl(bin, bin, (char *)NULL);
		fprintf(stderr, "exec %s: %s\n", bin, strerror(errno));
		_exit(127);
	}
	atexit(reap);

	if (wait_healthy(port) != 0) {
		fprintf(stderr, "the sidecar never became healthy on port %d\n", port);
		return 1;
	}
	printf("health:    ok\n");

	int rc = 1;
	char errbuf[256];
	http_response r;

	/* --- the token gate --------------------------------------------------
	 * Every /v1 route is behind it, including the read-only capabilities
	 * lookup. A missing header, a malformed header and a wrong token are all
	 * the same 401 with nothing to distinguish them. */
	if (http_request(port, "GET", "/v1/rails/mock", NULL, NULL, &r, errbuf, sizeof(errbuf)) != 0) {
		fprintf(stderr, "transport: %s\n", errbuf);
		return 1;
	}
	printf("no token:  HTTP %d\n", r.status);
	http_response_free(&r);

	/* --- capabilities ---------------------------------------------------- */
	if (http_request(port, "GET", "/v1/rails/mock", token, NULL, &r, errbuf, sizeof(errbuf)) != 0) {
		fprintf(stderr, "transport: %s\n", errbuf);
		return 1;
	}
	{
		char class_name[64] = "";
		int holds_funds = 1;
		json_string(r.body, "class", class_name, sizeof(class_name));
		json_bool(r.body, "holds_funds", &holds_funds);
		printf("caps:      HTTP %d %s holds_funds=%s\n", r.status, class_name,
		       holds_funds ? "true" : "false");
	}
	http_response_free(&r);

	/* --- pre-flight ------------------------------------------------------
	 * All five verdicts come back as 200. Read the BODY: a rail's honest
	 * refusal is data, and flattening five states into "worked / did not
	 * work" loses the only one that matters. A 400 here means the REQUEST
	 * was malformed and carries no verdict at all, so a rejected request can
	 * never be mistaken for a checked address. */
	if (http_request(port, "POST", "/v1/rails/mock/validate-destination", token,
	                 "{\"destination\":\"stellar:wallet:alice\"}", &r, errbuf, sizeof(errbuf)) != 0) {
		fprintf(stderr, "transport: %s\n", errbuf);
		return 1;
	}
	{
		char status[64] = "";
		int is_refusal = 0, must_confirm = 0;
		json_string(r.body, "status", status, sizeof(status));
		json_bool(r.body, "is_refusal", &is_refusal);
		json_bool(r.body, "human_must_confirm", &must_confirm);
		printf("dest:      HTTP %d %s is_refusal=%s human_must_confirm=%s\n", r.status, status,
		       is_refusal ? "true" : "false", must_confirm ? "true" : "false");
	}
	http_response_free(&r);

	/* --- quote ------------------------------------------------------------ */
	if (http_request(port, "POST", "/v1/rails/mock/quote", token, PAY_REQUEST, &r, errbuf,
	                 sizeof(errbuf)) != 0) {
		fprintf(stderr, "transport: %s\n", errbuf);
		return 1;
	}
	{
		uint64_t total = 0;
		int parsed = json_u64(r.body, "total_minor", &total);
		printf("quote:     HTTP %d total_minor=%llu (parsed as an integer: %s)\n", r.status,
		       (unsigned long long)total, parsed ? "yes" : "NO — refused a non-integer amount");
	}
	http_response_free(&r);

	/* --- charge ------------------------------------------------------------ */
	char receipt[1024] = "";
	if (http_request(port, "POST", "/v1/rails/mock/charge", token, PAY_REQUEST, &r, errbuf,
	                 sizeof(errbuf)) != 0) {
		fprintf(stderr, "transport: %s\n", errbuf);
		return 1;
	}
	if (r.status != 200 || r.len >= sizeof(receipt)) {
		fprintf(stderr, "charge:    HTTP %d %s\n", r.status, r.body);
		http_response_free(&r);
		return 1;
	}
	memcpy(receipt, r.body, r.len + 1);
	{
		char reference[64] = "";
		uint64_t amount = 0;
		json_string(receipt, "reference", reference, sizeof(reference));
		json_u64(receipt, "amount_minor", &amount);
		printf("charge:    HTTP %d %llu minor units ref=%s\n", r.status,
		       (unsigned long long)amount, reference);
	}
	http_response_free(&r);

	/* --- verify ------------------------------------------------------------ */
	if (http_request(port, "POST", "/v1/rails/mock/verify", token, receipt, &r, errbuf,
	                 sizeof(errbuf)) != 0) {
		fprintf(stderr, "transport: %s\n", errbuf);
		return 1;
	}
	printf("verify:    HTTP %d %s  <- the entitlement check\n", r.status, r.body);
	http_response_free(&r);

	/* A tampered receipt is HTTP 200 with {"valid":false}. NOT a 4xx. If you
	 * take one thing from this file: branch on the body. The day someone adds
	 * "retry on 4xx" to your HTTP helper, a status-code-only integration turns
	 * an unpaid order into an entitlement. */
	{
		char tampered[1024];
		const char *needle = "\"amount_minor\":1250";
		const char *at = strstr(receipt, needle);
		if (at) {
			size_t head = (size_t)(at - receipt);
			snprintf(tampered, sizeof(tampered), "%.*s\"amount_minor\":999999%s", (int)head,
			         receipt, at + strlen(needle));
			if (http_request(port, "POST", "/v1/rails/mock/verify", token, tampered, &r, errbuf,
			                 sizeof(errbuf)) != 0) {
				fprintf(stderr, "transport: %s\n", errbuf);
				return 1;
			}
			printf("tampered:  HTTP %d %s  <- 200, and false\n", r.status, r.body);
			http_response_free(&r);
		}
	}

	/* --- the edges --------------------------------------------------------- */
	if (http_request(port, "GET", "/v1/rails/solana", token, NULL, &r, errbuf, sizeof(errbuf)) ==
	    0) {
		printf("no rail:   HTTP %d — the sidecar's registry is mock-only\n", r.status);
		http_response_free(&r);
	}
	if (http_request(port, "POST", "/v1/rails/mock/webhook", token, "{\"hello\":\"there\"}", &r,
	                 errbuf, sizeof(errbuf)) == 0) {
		printf("webhook:   HTTP %d — the mock has no processor, so it invents no event\n",
		       r.status);
		http_response_free(&r);
	}

	printf("\nOK — offline, MockRail only, no value moved. Child reaped on exit.\n");
	rc = 0;
	return rc;
}
