// sidecar.cpp — patala from C++, as a child process on loopback.
//
// The same payout as direct.cpp, over HTTP against `patala-sidecar` instead of
// through the C ABI. Same JSON both ways: the sidecar and libpatala_ffi
// serialize the same `patala-core` types, so a body that works here works
// against patala_call unchanged.
//
// This program links NO patala library — it needs a socket and nothing else.
//
// Why a C++ program would choose this over linking the library, honestly:
//
//   - KEY ISOLATION. A non-custodial rail's signing key lives in whichever
//     process calls charge. Five services linking the library means the key is
//     in five address spaces; one sidecar puts it in one narrow process.
//   - No shared library exists for your platform (README.md has the table).
//   - Your process loads third-party code and is not a trust boundary you
//     control.
//
// Not on that list, and this is the difference from llmux's and openrate's
// equivalents: fork-safety, signal handlers and a runtime in your process.
// patala is Rust. This example forks, and it would be safe doing so even if it
// had loaded libpatala_ffi.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <signal.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#include <chrono>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>

#include "jsonpeek.hpp"

namespace {

const char *kPayRequest =
    R"({"amount_minor":1250,"currency":"USDC","destination":"mock:wallet:alice","reference":"order-1"})";

struct Response {
	int status = 0;
	std::string body;
};

// A loopback HTTP client, in about sixty lines.
//
// THIS IS NOT AN HTTP CLIENT. One request to 127.0.0.1 with
// `Connection: close`; no TLS, no redirects, no chunked encoding, no
// keep-alive, no retries, no SSE (patala has nothing to stream). It is here so
// this example has no dependency you have to trust in order to read it. A real
// program links libcurl or cpr and writes the same six lines.
class Loopback {
public:
	explicit Loopback(int port, std::string token = std::string())
	    : port_(port), token_(std::move(token)) {}

	Response get(const std::string &path) const { return request("GET", path, ""); }
	Response post(const std::string &path, const std::string &body) const {
		return request("POST", path, body);
	}

	Response request(const char *method, const std::string &path, const std::string &body) const {
		const int fd = ::socket(AF_INET, SOCK_STREAM, 0);
		if (fd < 0) throw std::runtime_error("socket");
		sockaddr_in addr{};
		addr.sin_family = AF_INET;
		addr.sin_port = htons(static_cast<uint16_t>(port_));
		::inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);
		if (::connect(fd, reinterpret_cast<sockaddr *>(&addr), sizeof(addr)) != 0) {
			::close(fd);
			throw std::runtime_error("connect");
		}

		std::ostringstream req;
		req << method << " " << path << " HTTP/1.1\r\n"
		    << "Host: 127.0.0.1:" << port_ << "\r\n"
		    << "User-Agent: patala-cpp-example/1\r\n";
		if (!token_.empty()) req << "Authorization: Bearer " << token_ << "\r\n";
		req << "Content-Type: application/json\r\n"
		    << "Content-Length: " << body.size() << "\r\n"
		    << "Connection: close\r\n\r\n"
		    << body;
		const std::string raw_req = req.str();
		if (::send(fd, raw_req.data(), raw_req.size(), 0) < 0) {
			::close(fd);
			throw std::runtime_error("send");
		}

		std::string raw;
		char chunk[4096];
		for (;;) {
			const ssize_t n = ::recv(fd, chunk, sizeof(chunk), 0);
			if (n < 0) {
				if (errno == EINTR) continue;
				::close(fd);
				throw std::runtime_error("recv");
			}
			if (n == 0) break;
			raw.append(chunk, static_cast<std::size_t>(n));
		}
		::close(fd);

		const std::size_t sep = raw.find("\r\n\r\n");
		if (sep == std::string::npos) throw std::runtime_error("no header terminator");
		Response out;
		out.status = std::atoi(raw.c_str() + raw.find(' ') + 1);
		out.body = raw.substr(sep + 4);
		return out;
	}

private:
	int port_;
	std::string token_;
};

// Owns the child. Kills and reaps it in the destructor — happy path, early
// return, or unwinding out of main.
class Child {
public:
	Child(const std::string &bin, int port, const std::string &token) {
		pid_ = ::fork();
		if (pid_ < 0) throw std::runtime_error("fork");
		if (pid_ == 0) {
			::setenv("PATALA_SIDECAR_PORT", std::to_string(port).c_str(), 1);
			::setenv("PATALA_SIDECAR_TOKEN", token.c_str(), 1);
			// The sidecar logs to stdout; send it to /dev/null so this
			// example's output is its own. stderr is left alone, so a refusal
			// to start is still visible.
			const int devnull = ::open("/dev/null", O_WRONLY);
			if (devnull >= 0) {
				::dup2(devnull, STDOUT_FILENO);
				::close(devnull);
			}
			::execl(bin.c_str(), bin.c_str(), static_cast<char *>(nullptr));
			std::cerr << "exec " << bin << ": " << std::strerror(errno) << "\n";
			::_exit(127);
		}
	}
	~Child() {
		if (pid_ > 0) {
			::kill(pid_, SIGTERM);
			::waitpid(pid_, nullptr, 0);
		}
	}
	Child(const Child &) = delete;
	Child &operator=(const Child &) = delete;

private:
	pid_t pid_ = 0;
};

// Ask the kernel for an unused loopback port and hand it straight back. Racy
// by construction — there is no portable "reserve a port for my child" — which
// is why a silent child is reported as a startup failure below.
int free_port() {
	const int fd = ::socket(AF_INET, SOCK_STREAM, 0);
	if (fd < 0) return 0;
	sockaddr_in addr{};
	addr.sin_family = AF_INET;
	addr.sin_port = 0;
	::inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);
	if (::bind(fd, reinterpret_cast<sockaddr *>(&addr), sizeof(addr)) != 0) {
		::close(fd);
		return 0;
	}
	socklen_t len = sizeof(addr);
	::getsockname(fd, reinterpret_cast<sockaddr *>(&addr), &len);
	const int port = ntohs(addr.sin_port);
	::close(fd);
	return port;
}

// The sidecar refuses to start without a token: no unauthenticated mode, no
// auto-generated fallback.
std::string random_token() {
	std::ifstream urandom("/dev/urandom", std::ios::binary);
	unsigned char raw[32];
	urandom.read(reinterpret_cast<char *>(raw), sizeof(raw));
	if (!urandom) throw std::runtime_error("/dev/urandom");
	std::ostringstream hex;
	hex << std::hex << std::setfill('0');
	for (unsigned char b : raw) hex << std::setw(2) << static_cast<int>(b);
	return hex.str();
}

// Poll /healthz — the one unauthenticated route — until the child is
// listening. Mandatory: fork+exec returns long before a socket is bound.
void wait_healthy(const Loopback &anon) {
	for (int i = 0; i < 500; i++) {
		try {
			if (anon.get("/healthz").status == 200) return;
		} catch (const std::exception &) {
			// not up yet
		}
		std::this_thread::sleep_for(std::chrono::milliseconds(20));
	}
	throw std::runtime_error("the sidecar never became healthy");
}

} // namespace

int main(int argc, char **argv) {
	const char *env_bin = std::getenv("PATALA_SIDECAR_BIN");
	const std::string bin = argc > 1 ? argv[1] : (env_bin ? env_bin : "");
	if (bin.empty()) {
		std::cerr << "usage: " << argv[0] << " <path-to-patala-sidecar>\n"
		          << "   or: PATALA_SIDECAR_BIN=... " << argv[0] << "\n"
		          << "build one with:  cargo build -p patala-sidecar --release\n";
		return 2;
	}

	try {
		const int port = free_port();
		if (port == 0) throw std::runtime_error("no free loopback port");
		const std::string token = random_token();

		std::cout << "patala sidecar (C++, child process) — 127.0.0.1:" << port << "\n";
		std::cout << "binary:    " << bin << "\n";

		const Child child{bin, port, token};
		const Loopback anon{port};
		const Loopback sc{port, token};

		wait_healthy(anon);
		std::cout << "health:    ok\n";

		// --- the token gate ------------------------------------------------
		// Every /v1 route is behind it, including the read-only capabilities
		// lookup. Missing, malformed and wrong are all the same 401.
		std::cout << "no token:  HTTP " << anon.get("/v1/rails/mock").status << "\n";

		// --- capabilities ----------------------------------------------------
		const Response caps = sc.get("/v1/rails/mock");
		std::cout << "caps:      HTTP " << caps.status << " "
		          << peek::str(caps.body, "class").value_or("?") << " holds_funds="
		          << (peek::flag(caps.body, "holds_funds").value_or(true) ? "true" : "false")
		          << "\n";

		// --- pre-flight --------------------------------------------------------
		// All five verdicts come back as 200. Read the BODY: a rail's honest
		// refusal is data, and flattening five states into "worked / did not"
		// loses the only one that matters.
		const Response dest =
		    sc.post("/v1/rails/mock/validate-destination", R"({"destination":"stellar:wallet:alice"})");
		std::cout << "dest:      HTTP " << dest.status << " "
		          << peek::str(dest.body, "status").value_or("?") << " is_refusal="
		          << (peek::flag(dest.body, "is_refusal").value_or(false) ? "true" : "false")
		          << " human_must_confirm="
		          << (peek::flag(dest.body, "human_must_confirm").value_or(false) ? "true" : "false")
		          << "\n";

		// A malformed REQUEST is a 400 with no verdict fields at all, so a
		// rejected request can never be mistaken for a checked address.
		std::cout << "typo:      HTTP "
		          << sc.post("/v1/rails/mock/validate-destination", R"({"destinaton":"typo"})").status
		          << " — a bad request is not a verdict\n";

		// --- quote --------------------------------------------------------------
		const Response quote = sc.post("/v1/rails/mock/quote", kPayRequest);
		std::cout << "quote:     HTTP " << quote.status << " total_minor="
		          << peek::u64(quote.body, "total_minor").value_or(0)
		          << " (parsed as an integer, never a double)\n";

		// --- charge -> verify -----------------------------------------------------
		const Response charged = sc.post("/v1/rails/mock/charge", kPayRequest);
		if (charged.status != 200) {
			std::cerr << "charge:    HTTP " << charged.status << " " << charged.body << "\n";
			return 1;
		}
		std::cout << "charge:    HTTP 200 " << peek::u64(charged.body, "amount_minor").value_or(0)
		          << " minor units ref=" << peek::str(charged.body, "reference").value_or("?")
		          << "\n";

		const Response verified = sc.post("/v1/rails/mock/verify", charged.body);
		std::cout << "verify:    HTTP " << verified.status << " " << verified.body
		          << "  <- the entitlement check\n";

		// A tampered receipt is HTTP 200 with {"valid":false}, NOT a 4xx.
		// Branch on the body. The day someone adds "retry on 4xx" to a shared
		// HTTP helper, a status-code-only integration grants an unpaid order.
		std::string tampered = charged.body;
		const std::string needle = "\"amount_minor\":1250";
		const std::size_t at = tampered.find(needle);
		if (at != std::string::npos) {
			tampered.replace(at, needle.size(), "\"amount_minor\":999999");
			const Response bad = sc.post("/v1/rails/mock/verify", tampered);
			std::cout << "tampered:  HTTP " << bad.status << " " << bad.body
			          << "  <- 200, and false\n";
		}

		// --- the edges -------------------------------------------------------------
		std::cout << "no rail:   HTTP " << sc.get("/v1/rails/solana").status
		          << " — the sidecar's registry is mock-only\n";
		std::cout << "webhook:   HTTP "
		          << sc.post("/v1/rails/mock/webhook", R"({"hello":"there"})").status
		          << " — the mock has no processor, so it invents no event\n";

		std::cout << "\nOK — offline, MockRail only, no value moved. Child reaped on exit.\n";
		return 0;
	} catch (const std::exception &e) {
		std::cerr << "error: " << e.what() << "\n";
		return 1;
	}
}
