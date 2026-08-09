/*
 * mini_http.h — just enough HTTP/1.1 over loopback for the sidecar example.
 *
 * C has no HTTP client in its standard library, and the house rule for these
 * SDKs is to prefer the standard library over a third-party dependency. So the
 * sidecar example talks to the server with BSD sockets directly. That is
 * honest for the case it covers — one request to 127.0.0.1, on a connection
 * this process opened, with `Connection: close` — and it is NOT a general HTTP
 * client: no TLS, no redirects, no chunked transfer-encoding, no keep-alive,
 * no proxies, no IPv6 literals, no retries.
 *
 * If you are writing a real program: link libcurl. This file exists so the
 * examples have no dependencies, not as a component to reuse.
 *
 * There is no SSE reader here, unlike the equivalent file in llmux's SDKs.
 * That is not an omission — patala has no streaming operation, in any mode.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#ifndef MINI_HTTP_H
#define MINI_HTTP_H

#include <stddef.h>

typedef struct {
	int status;  /* HTTP status code, or 0 if the request never completed */
	char *body;  /* NUL-terminated; free with http_response_free */
	size_t len;
} http_response;

/*
 * One request/response against 127.0.0.1:port. body may be NULL for GET, and
 * bearer may be NULL to send no Authorization header — which the sidecar
 * answers with 401 on every /v1 route, as the example shows on purpose.
 *
 * Returns 0 on success (a response arrived, whatever its status) and -1 on a
 * transport failure with errbuf filled in. Note the difference: an HTTP 401 or
 * 404 is a SUCCESS here. The sidecar's `{"valid":false}` arrives with a 200,
 * and conflating "the rail refused" with "the socket broke" is the single
 * easiest way to turn an unpaid order into an entitlement.
 *
 * On success the caller owns out->body and must call http_response_free.
 */
int http_request(int port, const char *method, const char *path, const char *bearer,
                 const char *body, http_response *out, char *errbuf, size_t errcap);

void http_response_free(http_response *r);

/*
 * Ask the kernel for an unused loopback port and give it straight back.
 *
 * Racy by construction: another process can take the port between the close
 * here and the child's bind. There is no portable "reserve a port for my
 * child", and the window is small enough that the health poll reports the loss
 * as a startup failure rather than a mystery. Returns 0 on failure.
 */
int http_free_port(void);

#endif /* MINI_HTTP_H */
