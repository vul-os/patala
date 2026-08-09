// The generated-Swift binding's assertions, as an ordinary program.
//
// Same discipline as ../Sources/patala-checks (the dlopen package's checks)
// and patala-ffi/ctest/smoke.c: a main that counts its checks and asserts HOW
// MANY ran, so a suite that silently stops executing half of them fails
// instead of passing. There is no XCTest here for the reason the package
// README already gives — Xcode is not installed on the machine this runs on,
// and `import XCTest` does not compile with Command Line Tools alone.
//
// Every check below is one the C-ABI package can only make by parsing a JSON
// document. Here they are properties of Swift structs and enums:
// `verdict.isRefusal` is a `Bool` field, `caps.railClass` is a `RailClass` a
// `switch` must cover, and `PatalaError` is an enum with associated values.
//
//   make -C sdks/swift/uniffi checks
//
// Runs against MockRail: deterministic, offline, no credentials, no network,
// and no value moved. This is a payments library.

import Foundation

let expectedChecks = 32
var ran = 0
var failed = 0

func check(_ name: String, _ condition: Bool) {
    ran += 1
    if condition {
        print("  ok   \(name)")
    } else {
        failed += 1
        print("  FAIL \(name)")
    }
}

print("patala Swift checks (generated UniFFI bindings)")

// ---- the type system itself -------------------------------------------------
// A `switch` with no `default` is the whole point of a typed binding: adding a
// rail class upstream must stop a consumer's build rather than fall through to
// a branch that guesses. These closures would not compile if the enums grew.
func describe(_ railClass: RailClass) -> String {
    switch railClass {
    case .custodialReversible: return "card form, refundable pending state"
    case .nonCustodialFinal: return "wallet address, signed final receipt"
    }
}
func isRefusalStatus(_ status: DestinationStatus) -> Bool {
    switch status {
    case .malformed, .wrongNetwork, .notAWallet: return true
    case .structurallyValid, .unknown: return false
    }
}
func paid(_ status: WebhookStatus) -> Bool {
    switch status {
    case .settled: return true
    case .notSettled, .unconfirmed: return false
    }
}
check("an exhaustive switch over RailClass compiles", describe(.nonCustodialFinal).isEmpty == false)
check("an exhaustive switch over DestinationStatus compiles", isRefusalStatus(.malformed))
check("only WebhookStatus.settled means paid", paid(.settled) && !paid(.unconfirmed) && !paid(.notSettled))

let rail = PatalaRail.newMock(
    id: "mock", railClass: .nonCustodialFinal, currencies: ["USDC"], feeMinor: 25, failing: false)

check("id() is the configured rail id", rail.id() == "mock")

let caps = rail.capabilities()
check("capabilities().railClass is a RailClass", caps.railClass == .nonCustodialFinal)
check("a non-custodial rail is not reversible", caps.reversible == false)
check("a non-custodial rail holds no funds", caps.holdsFunds == false)
check("settlement is the Settlement enum", caps.settlement == .instant)
check("currencies is a [String]", caps.currencies == ["USDC"])

// ---- destination pre-flight -------------------------------------------------
let good = rail.validateDestination(destination: "mock:wallet:alice")
check("a well-formed address is structurallyValid", good.status == .structurallyValid)
check("structurallyValid is not a refusal", good.isRefusal == false)
check("structurallyValid still requires a human", good.humanMustConfirm)
check("every verdict carries the exchange caveat", good.exchangeDepositCaveat == exchangeDepositCaveat())

let wrongNetwork = rail.validateDestination(destination: "eth:wallet:alice")
check("another network's address is wrongNetwork", wrongNetwork.status == .wrongNetwork)
check("wrongNetwork is a refusal", wrongNetwork.isRefusal)

let empty = rail.validateDestination(destination: "")
check("an empty destination is malformed", empty.status == .malformed)
check("malformed is a refusal", empty.isRefusal)

// ---- quote ------------------------------------------------------------------
let request = PayRequest(
    amountMinor: 1250, currency: "USDC", destination: "mock:wallet:alice", reference: "checks-1")
do {
    let quote = try rail.quote(req: request)
    check("quote totals in integer minor units", quote.totalMinor == quote.amountMinor + quote.feeMinor)
    check("the fee is the configured one", quote.feeMinor == 25)
} catch {
    check("quote totals in integer minor units", false)
    check("the fee is the configured one", false)
}

// ---- charge -> verify -------------------------------------------------------
do {
    let receipt = try rail.charge(req: request)
    check("a fresh receipt verifies", try rail.verify(receipt: receipt))
    check("the receipt carries the amount that was charged", receipt.amountMinor == 1250)
    check("the receipt is signed", receipt.proof.count == 32)

    // Fail-closed, field by field. `Receipt` is a struct, so each of these is
    // the mutation a real bug would make rather than a mangled string.
    var amount = receipt; amount.amountMinor = 125_000
    var currency = receipt; currency.currency = "USD"
    var reference = receipt; reference.reference = "someone-elses-order"
    var proof = receipt; proof.proof = Data(count: 32)
    for (field, tampered) in [
        ("amount", amount), ("currency", currency), ("reference", reference), ("proof", proof),
    ] {
        check("verify() is false for a tampered \(field)", try rail.verify(receipt: tampered) == false)
    }
} catch {
    for name in [
        "a fresh receipt verifies", "the receipt carries the amount that was charged",
        "the receipt is signed", "verify() is false for a tampered amount",
        "verify() is false for a tampered currency", "verify() is false for a tampered reference",
        "verify() is false for a tampered proof",
    ] {
        check(name, false)
    }
}

// ---- the refusals, matched as enum cases ------------------------------------
do {
    _ = try rail.verifyWebhook(
        delivery: WebhookDelivery(rawBody: Data(), headers: [:], query: nil, nowUnix: 1_700_000_000))
    check("the mock rail reports webhook verification Unsupported", false)
} catch PatalaError.Unsupported(let operation) {
    check("the mock rail reports webhook verification Unsupported", operation == "verify_webhook")
} catch {
    check("the mock rail reports webhook verification Unsupported", false)
}

do {
    _ = try rail.charge(
        req: PayRequest(
            amountMinor: 1, currency: "EUR", destination: "mock:wallet:alice", reference: "checks-2"))
    check("an unsupported currency is InvalidRequest, with a detail", false)
} catch PatalaError.InvalidRequest(let detail) {
    check("an unsupported currency is InvalidRequest, with a detail", detail.isEmpty == false)
} catch {
    check("an unsupported currency is InvalidRequest, with a detail", false)
}

let broken = PatalaRail.newMock(
    id: "mock", railClass: .nonCustodialFinal, currencies: ["USDC"], feeMinor: 0, failing: true)
do {
    _ = try broken.charge(req: request)
    check("a failing rail raises PatalaError.Rail", false)
} catch PatalaError.Rail(let detail) {
    check("a failing rail raises PatalaError.Rail", detail.isEmpty == false)
} catch {
    check("a failing rail raises PatalaError.Rail", false)
}

// ---- the rail that cannot check a destination -------------------------------
let opaque = PatalaRail.newMockWithoutDestinationChecks(
    id: "mock", railClass: .nonCustodialFinal, currencies: ["USDC"], feeMinor: 0, failing: false)
let unknown = opaque.validateDestination(destination: "mock:wallet:alice")
check("a rail that cannot check answers unknown", unknown.status == .unknown)
check("unknown is not a refusal — it needs a human", unknown.isRefusal == false)
check("unknown still requires a human", unknown.humanMustConfirm)

print("")
print("\(ran) checks ran, \(failed) failed (expected \(expectedChecks))")
if ran != expectedChecks {
    print("FAIL: \(ran) checks ran, expected \(expectedChecks)")
    exit(1)
}
if failed != 0 {
    print("FAIL: \(failed) check(s) failed")
    exit(1)
}
print("PASS")
