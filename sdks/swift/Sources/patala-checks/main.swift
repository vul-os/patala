// patala-checks — the assertions that back this package's claims.
//
// WHY THIS IS NOT AN XCTest TARGET
//
//   `swift test` needs XCTest, and XCTest ships with Xcode. On a machine with
//   only the Command Line Tools — which is the machine these examples were
//   written and run on — `import XCTest` does not even compile. A test target
//   that cannot be executed here would be a file nobody had ever seen pass,
//   sitting next to a README claiming it does.
//
//   So the checks are an ordinary executable instead. It runs anywhere Swift
//   runs, and it ends by asserting the NUMBER of checks that actually ran —
//   the same discipline as patala-ffi/ctest/smoke.c, and for the same reason:
//   a program that exits 0 having executed three of its checks looks exactly
//   like one that ran them all.
//
//   Run it:  swift run patala-checks
//
// SPDX-License-Identifier: MIT OR Apache-2.0

import Foundation
import Patala

var checksRun = 0
var failures = 0

func ok(_ what: String) {
    checksRun += 1
    print("  ok   \(what)")
}

func bad(_ what: String, _ detail: String) {
    checksRun += 1
    failures += 1
    print("  FAIL \(what): \(detail)")
}

func check(_ condition: Bool, _ what: String, _ detail: @autoclosure () -> String = "") {
    condition ? ok(what) : bad(what, detail())
}

let mock = #"{"rail":"mock","currencies":["USDC"]}"#

func pay(_ amountMinor: UInt64, _ currency: String, _ reference: String) -> String {
    #"{"amount_minor":\#(amountMinor),"currency":"\#(currency)","#
        + #""destination":"mock:wallet:alice","reference":"\#(reference)"}"#
}

do {
    let library = try Patala.Library.shared()
    print("patala Swift checks")
    print("  library: \(library.path)")

    // --- the library itself -------------------------------------------------
    check(!library.abiVersion.isEmpty, "patala_abi_version returns a version", "it returned \"\"")
    do {
        try library.requireABI(library.abiVersion)
        ok("requireABI accepts the version the library reports")
    } catch {
        bad("requireABI accepts the version the library reports", "\(error)")
    }
    do {
        try library.requireABI("0.0.1-stale")
        bad("requireABI refuses a wrong version", "it accepted one")
    } catch {
        check(
            "\(error)".contains("0.0.1-stale"),
            "the mismatch names the version the caller expected", "\(error)")
    }

    // --- the round trip -------------------------------------------------------
    let rail = try Rail(configJSON: mock, library: library)
    check(try rail.id().contains("\"rail_id\":\"mock\""), "id names the rail")

    let caps = try rail.capabilities()
    check(caps.contains("\"class\":\"NonCustodialFinal\""), "the settlement class crosses as a name, not a bool", caps)
    check(caps.contains("\"holds_funds\":false"), "a non-custodial rail reports holding no funds", caps)

    let quote = try rail.quote(pay(1250, "USDC", "check-1"))
    check(quote.contains("\"total_minor\":1250"), "the amount is an integer in minor units", quote)

    let receipt = try rail.charge(pay(1250, "USDC", "check-1"))
    check(receipt.contains("\"amount_minor\":1250"), "the receipt carries the amount charged", receipt)
    check(try rail.verifyHolds(receipt), "a genuine receipt verifies true")

    // The load-bearing one: a tampered receipt is RETURNED false, never thrown.
    let tampered = receipt.replacingOccurrences(
        of: #""amount_minor":1250"#, with: #""amount_minor":999999"#)
    do {
        let holds = try rail.verifyHolds(tampered)
        check(!holds, "a tampered receipt verifies false (fail-closed)")
        check(
            try rail.verify(tampered) == #"{"valid":false}"#,
            "and it comes back as a DOCUMENT, not as a thrown error")
    } catch {
        bad("verifying a tampered receipt must not throw", "\(error)")
        bad("and it comes back as a DOCUMENT, not as a thrown error", "it threw")
    }

    // --- refusals -------------------------------------------------------------
    do {
        _ = try rail.charge(pay(100, "EUR", "check-2"))
        bad("an unsupported currency is refused", "it returned a receipt")
        bad("the refusal names the currency", "there was no refusal")
    } catch {
        ok("an unsupported currency is refused")
        check("\(error)".contains("EUR"), "the refusal names the currency", "\(error)")
    }

    do {
        _ = try Rail(configJSON: #"{"rail":"mock","currencys":["USDC"]}"#, library: library)
        bad("a misspelled config field is refused, not defaulted", "it built a rail")
    } catch {
        ok("a misspelled config field is refused, not defaulted")
    }

    do {
        _ = try rail.call("no-such-method", "{}")
        bad("an unknown method is refused", "it returned a result")
    } catch {
        check("\(error)".contains("unknown method"), "an unknown method is refused", "\(error)")
    }

    // --- the destination pre-flight ---------------------------------------------
    for address in ["mock:wallet:alice", "stellar:wallet:alice", "not-an-address", ""] {
        let verdict = try rail.validateDestination(address)
        check(
            verdict.contains("\"human_must_confirm\":true"),
            "validate-destination(\(address.isEmpty ? "<empty>" : address)) still requires a human",
            verdict)
    }
    check(
        try rail.validateDestination("stellar:wallet:alice").contains("\"is_refusal\":true"),
        "an address for another chain is a refusal")
    check(
        try rail.caveat().contains("exchange"),
        "caveat returns the wording for the form where an address is first asked for")

    // --- the mock has no processor ------------------------------------------------
    do {
        _ = try rail.webhook(#"{"body":"{}","headers":{},"now_unix":1700000000}"#)
        bad("the mock invents no webhook event", "it returned one")
    } catch {
        ok("the mock invents no webhook event")
    }
} catch {
    bad("the checks reached the end", "\(error)")
}

// Update this when checks are added. It is the gate on the gate.
let expected = 22
print("\n\(checksRun) checks ran, \(failures) failed (expected \(expected))")
if checksRun != expected {
    print("FAIL: ran \(checksRun) checks, not \(expected) — an early exit, or a check was removed")
    exit(1)
}
if failures > 0 {
    print("FAIL: \(failures) check(s) failed")
    exit(1)
}
print("PASS")
