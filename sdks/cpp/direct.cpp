// direct.cpp — patala from C++, in-process, through the C ABI.
//
// The same payout `sdks/c/direct.c` walks, written the way C++ writes it: the
// handle and every returned string are owned by objects, so there is no
// cleanup label, no `goto`, and no path — including a throw — that leaks.
//
// This is an EXAMPLE, not a test. The ABI's test is
// patala-ffi/ctest/smoke.c, which dlopens the artifact and resolves every
// symbol by name.
//
// Everything runs on MockRail: deterministic, offline, no credentials, no
// network. This is a payments library, and an example that moves real value is
// not an example.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

#include <cstdio>
#include <iostream>
#include <string>

#include "jsonpeek.hpp"
#include "patala.hpp"

#ifndef PATALA_EXPECTED_VERSION
#define PATALA_EXPECTED_VERSION "0.0.0-unset"
#endif

namespace {

// Spelled out rather than defaulted so the shape of a real configuration is
// visible. Unknown fields are refused, so a typo throws.
const char *kRailConfig = R"({"rail":"mock","currencies":["USDC","USD"]})";

// What the customer typed into the payout form.
const char *kCustomerAddress = "mock:wallet:alice";

const char *kPayRequest =
    R"({"amount_minor":1250,"currency":"USDC","destination":"mock:wallet:alice","reference":"order-1"})";

} // namespace

int main() {
	try {
		// --- 0. the version probe ---------------------------------------
		std::cout << "patala direct (C++, in-process) — libpatala_ffi " << patala::abi_version()
		          << "\n";
		patala::require_abi(PATALA_EXPECTED_VERSION);
		std::cout << "abi:       matches " PATALA_EXPECTED_VERSION "\n";

		// --- 1. open a rail ----------------------------------------------
		// The destructor closes it. There is no close() to forget, and an
		// exception thrown anywhere below still runs it.
		const patala::Rail rail{kRailConfig};
		std::cout << "rail:      handle " << rail.handle() << "\n";

		// --- 2. capabilities decide the UX -------------------------------
		// Not a bool: "CustodialReversible" means a card form and a refundable
		// pending state, "NonCustodialFinal" means a wallet address and a
		// signed final receipt. Those are not two shades of one thing.
		const std::string caps = rail.capabilities();
		const std::string rail_class = peek::str(caps, "class").value_or("?");
		std::cout << "caps:      " << rail_class << " / "
		          << (rail_class == "CustodialReversible" ? "card form, refundable pending state"
		                                                  : "wallet address, signed final receipt")
		          << "\n";
		std::cout << "           holds_funds="
		          << (peek::flag(caps, "holds_funds").value_or(true) ? "true" : "false")
		          << " — patala itself never holds funds\n";

		// --- 3. the caveat, before there is a verdict ---------------------
		// The wording to show on the form where the address is first asked
		// for. It is its own method because at that moment there is nothing
		// to validate yet.
		const std::string caveat =
		    peek::str(rail.caveat(), "exchange_deposit_caveat").value_or("");
		std::cout << "caveat:    " << caveat.substr(0, 72) << "...\n";

		// --- 4. the pre-flight check ---------------------------------------
		// Offline, and it NEVER fails: "I cannot check this" comes back as the
		// verdict Unknown, not as an exception, because a caller must handle
		// it as carefully as a refusal.
		const std::string verdict =
		    rail.validate_destination(std::string(R"({"destination":")") + kCustomerAddress + "\"}");
		const bool refused = peek::flag(verdict, "is_refusal").value_or(true);
		std::cout << "dest:      " << kCustomerAddress << " -> "
		          << peek::str(verdict, "status").value_or("?")
		          << " (is_refusal=" << (refused ? "true" : "false") << ", human_must_confirm="
		          << (peek::flag(verdict, "human_must_confirm").value_or(false) ? "true" : "false")
		          << ")\n";
		if (refused) {
			std::cout << "           refused — do not send. Stop here.\n";
			return 1;
		}
		// human_must_confirm is true on EVERY verdict, including
		// StructurallyValid. patala does not detect exchange-owned addresses
		// and will not guess, so a person still has to say yes.

		// --- 5. quote ------------------------------------------------------
		const std::string quote = rail.quote(kPayRequest);
		std::cout << "quote:     " << peek::u64(quote, "amount_minor").value_or(0) << " + "
		          << peek::u64(quote, "fee_minor").value_or(0) << " fee = "
		          << peek::u64(quote, "total_minor").value_or(0) << " minor units of "
		          << peek::str(quote, "currency").value_or("?") << "\n";

		// --- 6. charge -------------------------------------------------------
		// The Receipt is the entitlement. Store the whole document; it is what
		// you hand back to verify() later.
		const std::string receipt = rail.charge(kPayRequest);
		std::cout << "charge:    " << peek::u64(receipt, "amount_minor").value_or(0)
		          << " minor units, ref=" << peek::str(receipt, "reference").value_or("?")
		          << ", issued by rail " << peek::str(receipt, "rail_id").value_or("?") << "\n";

		// --- 7. verify --------------------------------------------------------
		// Gate entitlement on this, never on charge() having returned.
		std::cout << "verify:    " << rail.verify(receipt) << "  <- the entitlement check\n";

		// --- 8. the outcome that is NOT an exception ---------------------------
		// verify_holds() returns false for a tampered receipt and THROWS only
		// when the check could not be performed. Merging the two is how an
		// unpaid order becomes an entitlement: every `catch` that logs and
		// retries would be granting one.
		std::string tampered = receipt;
		const std::string needle = "\"amount_minor\":1250";
		const std::size_t at = tampered.find(needle);
		if (at != std::string::npos) {
			tampered.replace(at, needle.size(), "\"amount_minor\":999999");
			std::cout << "tampered:  " << rail.verify(tampered) << " -> verify_holds()="
			          << (rail.verify_holds(tampered) ? "true (WRONG)" : "false")
			          << "  <- returned, not thrown\n";
		}

		// --- 9. the throw path, and what it does not leak ----------------------
		// An operational refusal IS an exception, and the library's message is
		// malloc'd on the other side of the ABI. `detail::Owned` holds it
		// across the std::string copy and frees it during unwinding — the one
		// line a hand-written binding forgets. `leaks --atExit -- ./direct`
		// reports zero; see README.md.
		try {
			rail.charge(
			    R"({"amount_minor":100,"currency":"EUR","destination":"mock:wallet:alice","reference":"order-2"})");
			std::cout << "refused:   (a receipt — WRONG)\n";
		} catch (const patala::Error &e) {
			std::cout << "refused:   patala::Error: " << e.what() << "\n";
		}

		// A closed or invented handle is a clean error, never a segfault in
		// your address space: handles are registry keys, and they are never
		// reused.
		try {
			patala::Rail moved{kRailConfig};
			const patala::Rail took = std::move(moved); // moved-from handle is 0
			std::cout << "moved:     handle " << moved.handle() << " -> " << took.handle()
			          << ", one owner, one close\n";
		} catch (const patala::Error &e) {
			std::cout << "moved:     unexpected: " << e.what() << "\n";
		}

		std::cout << "\nOK — offline, MockRail only, no value moved.\n";
		return 0;
	} catch (const patala::Error &e) {
		std::cerr << "patala::Error: " << e.what() << "\n";
		return 1;
	} catch (const std::exception &e) {
		std::cerr << "error: " << e.what() << "\n";
		return 1;
	}
}
