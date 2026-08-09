/* jsonpeek.c — see jsonpeek.h. SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "jsonpeek.h"

#include <stdio.h>
#include <string.h>

/* Find `"key":` and return a pointer just past the colon, skipping spaces. */
static const char *seek(const char *doc, const char *key) {
	char needle[128];
	int n = snprintf(needle, sizeof(needle), "\"%s\":", key);
	if (n < 0 || (size_t)n >= sizeof(needle)) return NULL;
	const char *at = strstr(doc, needle);
	if (!at) return NULL;
	at += strlen(needle);
	while (*at == ' ') at++;
	return at;
}

int json_string(const char *doc, const char *key, char *out, size_t outcap) {
	if (outcap) out[0] = '\0';
	const char *p = seek(doc, key);
	if (!p || *p != '"') return 0;
	p++;
	size_t len = 0;
	while (*p && *p != '"') {
		char c = *p;
		if (c == '\\' && p[1]) {
			p++;
			switch (*p) {
			case 'n': c = '\n'; break;
			case 't': c = '\t'; break;
			case 'r': c = '\r'; break;
			default:  c = *p;   break; /* \" \\ \/ and, unhandled, \uXXXX */
			}
		}
		if (len + 1 < outcap) {
			out[len++] = c;
			out[len] = '\0';
		}
		p++;
	}
	return 1;
}

int json_u64(const char *doc, const char *key, uint64_t *out) {
	const char *p = seek(doc, key);
	if (!p || *p < '0' || *p > '9') return 0;
	uint64_t v = 0;
	while (*p >= '0' && *p <= '9') {
		v = v * 10 + (uint64_t)(*p - '0');
		p++;
	}
	/* A fractional amount is a defect, not a rounding problem. Refuse it. */
	if (*p == '.' || *p == 'e' || *p == 'E') return 0;
	*out = v;
	return 1;
}

int json_bool(const char *doc, const char *key, int *out) {
	const char *p = seek(doc, key);
	if (!p) return 0;
	if (strncmp(p, "true", 4) == 0) {
		*out = 1;
		return 1;
	}
	if (strncmp(p, "false", 5) == 0) {
		*out = 0;
		return 1;
	}
	return 0;
}
