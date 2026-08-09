/*
 * jsonpeek.h — pull a few fields out of a JSON document, for printing.
 *
 * THIS IS NOT A JSON PARSER. It scans for `"key":` and reads what follows,
 * understanding \" and \\ and nothing else. It does not validate, does not
 * handle nesting, does not know that a key inside a string literal is not a
 * key, and does not decode \uXXXX. It is here so the examples can print an
 * answer without dragging a parser into a directory whose subject is the C
 * ABI. A real program links cJSON, jansson or yyjson; patala speaks ordinary
 * JSON, so any of them works unchanged.
 *
 * One thing here is NOT a shortcut and should survive into your real code:
 * there is no `json_number` returning a double. Amounts in patala are integer
 * minor units, and a double silently loses every integer above 2^53. This is
 * money. Parse it as an integer or do not parse it.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#ifndef JSONPEEK_H
#define JSONPEEK_H

#include <stddef.h>
#include <stdint.h>

/*
 * Copy the string value of the first `"key":"..."` into out (always
 * NUL-terminated). Returns 1 if found, 0 otherwise.
 */
int json_string(const char *doc, const char *key, char *out, size_t outcap);

/*
 * Read the first `"key":<integer>` into *out. Returns 1 if found and the value
 * is a non-negative integer, 0 otherwise — including for `1250.0`, which is
 * REFUSED rather than truncated. A rail that ever sends a fractional amount is
 * a bug to notice, not a value to round.
 */
int json_u64(const char *doc, const char *key, uint64_t *out);

/*
 * Read the first `"key":true|false` into *out (1/0). Returns 1 if found.
 */
int json_bool(const char *doc, const char *key, int *out);

#endif /* JSONPEEK_H */
