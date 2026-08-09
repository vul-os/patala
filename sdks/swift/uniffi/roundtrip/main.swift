// A charge -> verify round trip through the GENERATED UniFFI Swift bindings.
//
// The sibling of ../Sources/patala-direct-example, which does the same thing
// over the plain C ABI with JSON on both sides. Read them next to each other:
// this file never mentions JSON, never decodes anything, and gets a compiler
// error rather than a nil if it misspells a field.
//
//   make -C sdks/swift/uniffi run
//
// MockRail only: deterministic, offline, no credentials, no value moved.

import Foundation

let alice = "mock:wallet:alice"

let rail = PatalaRail.newMock(
    id: "mock", railClass: .nonCustodialFinal, currencies: ["USDC"], feeMinor: 25, failing: false)

print("patala direct (Swift, generated UniFFI) — namespace `patala`")
print("id:        \(rail.id())")

let caps = rail.capabilities()
// No `default:`. A third rail class added upstream stops this build, which is
// the entire argument for a generated binding over a string one.
switch caps.railClass {
case .nonCustodialFinal:
    print("caps:      nonCustodialFinal — wallet address, signed final receipt")
case .custodialReversible:
    print("caps:      custodialReversible — card form, refundable pending state")
}
print(
    "           holds_funds=\(caps.holdsFunds) reversible=\(caps.reversible) "
        + "currencies=\(caps.currencies) settlement=\(caps.settlement)")

print("caveat:    \(exchangeDepositCaveat().prefix(72))...")

// ---- destination pre-flight -------------------------------------------------
for candidate in [alice, "eth:wallet:alice", ""] {
    let verdict = rail.validateDestination(destination: candidate)
    let shown = candidate.isEmpty ? "\"\" (empty)" : "\"\(candidate)\""
    print(
        "dest:      \(shown) -> \(verdict.status) "
            + "(is_refusal=\(verdict.isRefusal), human_must_confirm=\(verdict.humanMustConfirm))")
}
// human_must_confirm is true on EVERY verdict, structurallyValid included:
// patala does not detect exchange-owned addresses and will not guess.

let opaque = PatalaRail.newMockWithoutDestinationChecks(
    id: "mock", railClass: .nonCustodialFinal, currencies: ["USDC"], feeMinor: 0, failing: false)
let cannotCheck = opaque.validateDestination(destination: alice)
print("dest:      the same address on a rail that cannot check -> \(cannotCheck.status)")
print("           unknown is NOT a refusal and NOT an approval. It needs a human.")

// ---- the money --------------------------------------------------------------
// UInt64 minor units. Never a Double, on either side of the boundary.
let request = PayRequest(
    amountMinor: 1250, currency: "USDC", destination: alice, reference: "order-4711")

do {
    let quote = try rail.quote(req: request)
    print(
        "quote:     \(quote.amountMinor) + \(quote.feeMinor) fee = \(quote.totalMinor) "
            + "minor units of \(quote.currency), \(quote.settlement)")

    let receipt = try rail.charge(req: request)
    print(
        "charge:    \(receipt.amountMinor) \(receipt.currency) ref=\(receipt.reference) "
            + "proof=\(receipt.proof.count)B issued by \(receipt.railId)")

    // THIS is the entitlement check — not "charge returned without throwing",
    // which only says the rail accepted the instruction.
    print("verify:    \(try rail.verify(receipt: receipt))  <- the entitlement check")

    var tampered = receipt
    tampered.amountMinor = 125_000
    print("tampered:  \(try rail.verify(receipt: tampered))  <- returned, not thrown")

    _ = try rail.charge(
        req: PayRequest(amountMinor: 1, currency: "EUR", destination: alice, reference: "x"))
    print("refused:   UNEXPECTED SUCCESS")
} catch let error as PatalaError {
    // Matched as an enum, not as text.
    switch error {
    case .InvalidRequest(let detail): print("refused:   InvalidRequest — \(detail)")
    case .Rail(let detail): print("refused:   Rail — \(detail)")
    case .Unsupported(let operation): print("refused:   Unsupported — \(operation)")
    case .CrossClassFailover(let from, let to): print("refused:   CrossClassFailover \(from) -> \(to)")
    case .AllRailsFailed: print("refused:   AllRailsFailed")
    }
} catch {
    print("refused:   unexpected error \(error)")
    exit(1)
}

print("")
print("OK — offline, MockRail only, no value moved.")
