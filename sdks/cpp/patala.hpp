// patala.hpp — a header-only C++ wrapper over patala's C ABI.
//
// Zero dependencies beyond the standard library and
// patala-ffi/include/patala.h. C++17. Nothing here allocates a rail, opens a
// socket or starts a thread until you ask it to.
//
// WHAT THIS FILE IS FOR
//
//   The C ABI hands back `char*` that must be released with patala_free() and
//   with nothing else, through a trailing `char** err` on every fallible call.
//   Written by hand at each call site, that is four lines of bookkeeping per
//   operation and exactly one of them — the error string on the failure path —
//   is the one everybody forgets. A destructor does it instead.
//
//   The interesting case is not the happy path. It is the THROW path: the
//   library has already handed you a malloc'd message, you are about to
//   convert it into a std::string (which can itself throw), and then you
//   unwind. `Owned` below holds the raw pointer for the whole of that, so the
//   message is freed during unwinding whether the throw succeeds or the string
//   construction fails first. `direct.cpp` exercises it and `leaks --atExit`
//   reports zero.
//
// WHAT IT DELIBERATELY DOES NOT DO
//
//   - No JSON type. Requests and responses are the same JSON the sidecar
//     serves; you already have a JSON library, and patala should not pick one
//     for you. `std::string` in, `std::string` out.
//   - No streaming. patala has no streaming operation — see patala.h.
//   - No global state, no static rail, no hidden thread pool.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

#ifndef PATALA_HPP
#define PATALA_HPP

#include <cstdint>
#include <stdexcept>
#include <string>
#include <utility>

#include "patala.h"

namespace patala {

// An error reported BY the library. The message is plain UTF-8 text, never
// JSON — print it, do not parse it.
class Error : public std::runtime_error {
public:
	explicit Error(const std::string &message) : std::runtime_error(message) {}
};

namespace detail {

// Sole owner of a `char*` the library returned. Frees it with patala_free and
// nothing else — not `delete`, not `free()`: this is Rust's allocator.
// patala_free(nullptr) is defined and safe, so there is no null check.
class Owned {
public:
	explicit Owned(char *p = nullptr) noexcept : p_(p) {}
	~Owned() { patala_free(p_); }

	Owned(const Owned &) = delete;
	Owned &operator=(const Owned &) = delete;

	Owned(Owned &&other) noexcept : p_(other.p_) { other.p_ = nullptr; }
	Owned &operator=(Owned &&other) noexcept {
		if (this != &other) {
			patala_free(p_);
			p_ = other.p_;
			other.p_ = nullptr;
		}
		return *this;
	}

	const char *get() const noexcept { return p_; }
	explicit operator bool() const noexcept { return p_ != nullptr; }

	// May throw std::bad_alloc. That is fine and is the whole point: this
	// object still owns the pointer while it does, and frees it on unwind.
	std::string copy() const { return p_ ? std::string(p_) : std::string(); }

private:
	char *p_;
};

} // namespace detail

// The version of patala the loaded library was built from, e.g. "0.1.0". A
// static string; it is not owned and must not be freed.
inline std::string abi_version() { return std::string(patala_abi_version()); }

// Throws unless the loaded library reports exactly `expected`.
//
// Call this at startup. A shared library is resolved off a load path you may
// not control, and a stale libpatala_ffi earlier on that path is called
// silently and then misbehaves in ways that look like patala bugs.
inline void require_abi(const std::string &expected) {
	char *err = nullptr;
	if (patala_abi_check(expected.c_str(), &err) != 0) {
		detail::Owned owned(err);
		throw Error(owned ? owned.copy() : "patala: ABI version mismatch");
	}
}

// One rail. Move-only: a handle has exactly one owner, and the destructor
// closes it — on the happy path, on an early return, and on a throw.
class Rail {
public:
	// `config_json` is a JSON object tagged by "rail"; empty means the offline
	// default, a deterministic MockRail that needs no credentials and no
	// network. Unknown fields are REFUSED rather than ignored, so a misspelled
	// key throws instead of quietly building a rail you did not ask for.
	//
	// Building a rail talks to nothing: no socket, no thread, no file.
	explicit Rail(const std::string &config_json = std::string()) {
		char *err = nullptr;
		handle_ = patala_new(config_json.empty() ? nullptr : config_json.c_str(), &err);
		if (handle_ == 0) {
			detail::Owned owned(err);
			throw Error(owned ? owned.copy() : "patala: could not open a rail");
		}
	}

	~Rail() { patala_close(handle_); } // closing 0 or twice is a no-op

	Rail(const Rail &) = delete;
	Rail &operator=(const Rail &) = delete;

	Rail(Rail &&other) noexcept : handle_(other.handle_) { other.handle_ = 0; }
	Rail &operator=(Rail &&other) noexcept {
		if (this != &other) {
			patala_close(handle_);
			handle_ = other.handle_;
			other.handle_ = 0;
		}
		return *this;
	}

	std::uint64_t handle() const noexcept { return handle_; }

	// One unary call: JSON in, JSON out, an Error thrown on failure.
	//
	// `request_json` may be empty for the methods that ignore it ("id",
	// "capabilities", "caveat", "providers").
	std::string call(const char *method, const std::string &request_json = std::string()) const {
		char *err = nullptr;
		detail::Owned out(patala_call(handle_, method,
		                              request_json.empty() ? nullptr : request_json.c_str(), &err));
		detail::Owned message(err);
		if (!out) {
			throw Error(message ? message.copy() : "patala: the call failed with no message");
		}
		return out.copy();
	}

	// Thin, named wrappers. They add nothing but a spelling you cannot typo
	// past the compiler, which matters for a string-dispatched ABI.
	std::string id() const { return call("id"); }
	std::string capabilities() const { return call("capabilities"); }
	std::string caveat() const { return call("caveat"); }
	std::string providers() const { return call("providers"); }
	std::string quote(const std::string &pay_request) const { return call("quote", pay_request); }
	std::string charge(const std::string &pay_request) const { return call("charge", pay_request); }
	std::string webhook(const std::string &delivery) const { return call("webhook", delivery); }

	std::string validate_destination(const std::string &destination_json) const {
		return call("validate-destination", destination_json);
	}

	// `verify` returns a DOCUMENT, `{"valid":true}` or `{"valid":false}`, and
	// this returns it verbatim rather than a bool — see verify_holds() for why
	// the bool is the more dangerous shape and why it is spelled out.
	std::string verify(const std::string &receipt) const { return call("verify", receipt); }

	// True only for exactly `{"valid":true}`.
	//
	// Read this carefully, because it is the one place a C++ wrapper can do
	// real damage. There are THREE outcomes here, not two:
	//
	//   throws Error   -> the check could not be performed. Retry, alert.
	//   returns false  -> the rail checked and the receipt does not hold.
	//                     Never retry, never grant.
	//   returns true   -> the entitlement.
	//
	// A wrapper that turned `{"valid":false}` into an exception would merge
	// the first two, and every `catch` that logs-and-retries would then be
	// granting entitlements on unpaid orders. patala's C ABI keeps them apart
	// by returning a result rather than NULL; this keeps them apart too.
	bool verify_holds(const std::string &receipt) const {
		const std::string answer = verify(receipt);
		return answer.find("\"valid\":true") != std::string::npos;
	}

private:
	std::uint64_t handle_ = 0;
};

} // namespace patala

#endif // PATALA_HPP
