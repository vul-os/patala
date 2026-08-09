/* mini_http.c — see mini_http.h. SPDX-License-Identifier: MIT OR Apache-2.0 */

#define _POSIX_C_SOURCE 200809L

#include "mini_http.h"

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static void fail(char *errbuf, size_t errcap, const char *what) {
	if (errbuf && errcap) {
		snprintf(errbuf, errcap, "%s: %s", what, strerror(errno));
	}
}

static int dial(int port, char *errbuf, size_t errcap) {
	int fd = socket(AF_INET, SOCK_STREAM, 0);
	if (fd < 0) {
		fail(errbuf, errcap, "socket");
		return -1;
	}
	struct sockaddr_in addr;
	memset(&addr, 0, sizeof(addr));
	addr.sin_family = AF_INET;
	addr.sin_port = htons((unsigned short)port);
	/* inet_pton rather than INADDR_LOOPBACK: the macro is hidden by a strict
	 * _POSIX_C_SOURCE on darwin, and inet_pton is POSIX everywhere. */
	inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);
	if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
		fail(errbuf, errcap, "connect");
		close(fd);
		return -1;
	}
	return fd;
}

static int send_all(int fd, const char *data, size_t len) {
	while (len > 0) {
		ssize_t n = send(fd, data, len, 0);
		if (n <= 0) {
			if (n < 0 && errno == EINTR) continue;
			return -1;
		}
		data += n;
		len -= (size_t)n;
	}
	return 0;
}

int http_request(int port, const char *method, const char *path, const char *bearer,
                 const char *body, http_response *out, char *errbuf, size_t errcap) {
	memset(out, 0, sizeof(*out));
	int fd = dial(port, errbuf, errcap);
	if (fd < 0) return -1;

	char head[1024];
	size_t body_len = body ? strlen(body) : 0;
	int n = snprintf(head, sizeof(head),
	                 "%s %s HTTP/1.1\r\n"
	                 "Host: 127.0.0.1:%d\r\n"
	                 "User-Agent: patala-c-example/1\r\n"
	                 "Accept: application/json\r\n"
	                 "%s%s%s"
	                 "Content-Type: application/json\r\n"
	                 "Content-Length: %zu\r\n"
	                 "Connection: close\r\n"
	                 "\r\n",
	                 method, path, port,
	                 bearer ? "Authorization: Bearer " : "", bearer ? bearer : "",
	                 bearer ? "\r\n" : "",
	                 body_len);
	if (n < 0 || (size_t)n >= sizeof(head) || send_all(fd, head, (size_t)n) != 0 ||
	    (body_len && send_all(fd, body, body_len) != 0)) {
		fail(errbuf, errcap, "send");
		close(fd);
		return -1;
	}

	/* Connection: close, so the response ends at EOF and no framing is needed. */
	size_t cap = 8192, len = 0;
	char *buf = malloc(cap);
	if (!buf) {
		close(fd);
		if (errbuf && errcap) snprintf(errbuf, errcap, "out of memory");
		return -1;
	}
	for (;;) {
		if (len + 4096 + 1 > cap) {
			cap *= 2;
			char *bigger = realloc(buf, cap);
			if (!bigger) {
				free(buf);
				close(fd);
				if (errbuf && errcap) snprintf(errbuf, errcap, "out of memory");
				return -1;
			}
			buf = bigger;
		}
		ssize_t got = recv(fd, buf + len, 4096, 0);
		if (got < 0) {
			if (errno == EINTR) continue;
			fail(errbuf, errcap, "recv");
			free(buf);
			close(fd);
			return -1;
		}
		if (got == 0) break;
		len += (size_t)got;
	}
	buf[len] = '\0';
	close(fd);

	char *sep = strstr(buf, "\r\n\r\n");
	if (!sep) {
		free(buf);
		if (errbuf && errcap) snprintf(errbuf, errcap, "no header terminator in the response");
		return -1;
	}
	int major = 0, minor = 0, status = 0;
	if (sscanf(buf, "HTTP/%d.%d %d", &major, &minor, &status) != 3) status = 0;
	out->status = status;

	char *payload = sep + 4;
	out->len = len - (size_t)(payload - buf);
	out->body = malloc(out->len + 1);
	if (!out->body) {
		free(buf);
		if (errbuf && errcap) snprintf(errbuf, errcap, "out of memory");
		return -1;
	}
	memcpy(out->body, payload, out->len);
	out->body[out->len] = '\0';
	free(buf);
	return 0;
}

void http_response_free(http_response *r) {
	if (!r) return;
	free(r->body);
	r->body = NULL;
	r->len = 0;
}

int http_free_port(void) {
	int fd = socket(AF_INET, SOCK_STREAM, 0);
	if (fd < 0) return 0;
	struct sockaddr_in addr;
	memset(&addr, 0, sizeof(addr));
	addr.sin_family = AF_INET;
	addr.sin_port = 0;
	inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);
	if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
		close(fd);
		return 0;
	}
	socklen_t len = sizeof(addr);
	if (getsockname(fd, (struct sockaddr *)&addr, &len) != 0) {
		close(fd);
		return 0;
	}
	int port = ntohs(addr.sin_port);
	close(fd);
	return port;
}
