// patala from Swift, direct — in-process, through the C ABI.
//
// The same payout `sdks/c/direct.c` walks, written the way Swift writes it:
// the handle is closed by `deinit`, every string the library returns is copied
// and freed in one place, and the JSON is decoded into `Decodable` structs
// with `UInt64` amounts rather than peeked at with substring searches.
//
// Everything runs on MockRail: deterministic, offline, no credentials, no
// network. This is a payments library, and an example that moves real value is
// not an example.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

import Foundation
import Patala

// MARK: - Just enough of patala's wire types
//
// These mirror `patala-core`'s own structs. Note `UInt64` for every amount:
// money is integer minor units plus a currency string, and a `Double` silently
// loses every integer above 2^53. Decoding `1250.0` into `UInt64` FAILS here,
// which is the correct outcome — a fractional amount is a defect to notice,
// not a value to round.

struct Capabilities: Decodable {
    let railClass: String
    let reversible: Bool
    let requiresKyc: Bool
    let holdsFunds: Bool
    let currencies: [String]

    enum CodingKeys: String, CodingKey {
        case railClass = "class"
        case reversible
        case requiresKyc = "requires_kyc"
        case holdsFunds = "holds_funds"
        case currencies
    }
}

struct Quote: Decodable {
    let amountMinor: UInt64
    let feeMinor: UInt64
    let totalMinor: UInt64
    let currency: String

    enum CodingKeys: String, CodingKey {
        case amountMinor = "amount_minor"
        case feeMinor = "fee_minor"
        case totalMinor = "total_minor"
        case currency
    }
}

struct Receipt: Decodable {
    let railId: String
    let amountMinor: UInt64
    let currency: String
    let reference: String
    let proof: [UInt8]

    enum CodingKeys: String, CodingKey {
        case railId = "rail_id"
        case amountMinor = "amount_minor"
        case currency
        case reference
        case proof
    }
}

struct Verdict: Decodable {
    let status: String
    let isRefusal: Bool
    let humanMustConfirm: Bool
    let exchangeDepositCaveat: String

    enum CodingKeys: String, CodingKey {
        case status
        case isRefusal = "is_refusal"
        case humanMustConfirm = "human_must_confirm"
        case exchangeDepositCaveat = "exchange_deposit_caveat"
    }
}

struct Caveat: Decodable {
    let text: String
    enum CodingKeys: String, CodingKey { case text = "exchange_deposit_caveat" }
}

func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try JSONDecoder().decode(type, from: Data(json.utf8))
}

// MARK: - The flow

// Spelled out rather than defaulted so the shape of a real configuration is
// visible. Unknown fields are refused, so a typo throws.
let railConfig = #"{"rail":"mock","currencies":["USDC","USD"]}"#

// What the customer typed into the payout form.
let customerAddress = "mock:wallet:alice"

let payRequest = #"""
{"amount_minor":1250,"currency":"USDC","destination":"mock:wallet:alice","reference":"order-1"}
"""#

do {
    // --- 0. find the library, and check it is the right one ----------------
    let library = try Patala.Library.shared()
    print("patala direct (Swift, in-process) — libpatala_ffi \(library.abiVersion)")
    print("library:   \(library.path)")
    let expected = ProcessInfo.processInfo.environment["PATALA_VERSION"] ?? library.abiVersion
    try library.requireABI(expected)
    print("abi:       matches \(expected)")

    // --- 1. open a rail ------------------------------------------------------
    // `deinit` closes it. There is no close() to forget and no `defer` to write
    // at the call site.
    let rail = try Rail(configJSON: railConfig, library: library)

    // --- 2. capabilities decide the UX ---------------------------------------
    // Not a bool: "CustodialReversible" means a card form and a refundable
    // pending state, "NonCustodialFinal" means a wallet address and a signed
    // final receipt. Those are not two shades of one thing.
    let caps = try decode(Capabilities.self, rail.capabilities())
    print("caps:      \(caps.railClass) / "
        + (caps.railClass == "CustodialReversible"
            ? "card form, refundable pending state"
            : "wallet address, signed final receipt"))
    print("           holds_funds=\(caps.holdsFunds) reversible=\(caps.reversible) "
        + "currencies=\(caps.currencies)")

    // --- 3. the caveat, before there is a verdict -----------------------------
    // The wording to show on the form where the address is first asked for. It
    // is its own method because at that moment there is nothing to validate.
    let caveat = try decode(Caveat.self, rail.caveat()).text
    print("caveat:    \(caveat.prefix(72))...")

    // --- 4. the pre-flight check -----------------------------------------------
    // Offline, and it NEVER fails: "I cannot check this" comes back as the
    // verdict Unknown, not as a thrown error, because a caller must handle it
    // as carefully as a refusal.
    let verdict = try decode(Verdict.self, rail.validateDestination(customerAddress))
    print("dest:      \(customerAddress) -> \(verdict.status) "
        + "(is_refusal=\(verdict.isRefusal), human_must_confirm=\(verdict.humanMustConfirm))")
    guard !verdict.isRefusal else {
        print("           refused — do not send. Stop here.")
        exit(1)
    }
    // human_must_confirm is true on EVERY verdict, including StructurallyValid.
    // patala does not detect exchange-owned addresses and will not guess.

    // --- 5. quote ----------------------------------------------------------------
    let quote = try decode(Quote.self, rail.quote(payRequest))
    print("quote:     \(quote.amountMinor) + \(quote.feeMinor) fee = \(quote.totalMinor) "
        + "minor units of \(quote.currency)")

    // --- 6. charge ------------------------------------------------------------------
    // The Receipt is the entitlement. Keep the RAW document — that is what you
    // hand back to verify() later; the decoded struct is only for printing.
    let receiptJSON = try rail.charge(payRequest)
    let receipt = try decode(Receipt.self, receiptJSON)
    print("charge:    \(receipt.amountMinor) minor units, ref=\(receipt.reference), "
        + "issued by rail \(receipt.railId), proof=\(receipt.proof.count)B")

    // --- 7. verify --------------------------------------------------------------------
    print("verify:    \(try rail.verify(receiptJSON))  <- the entitlement check")

    // --- 8. the outcome that is NOT a thrown error ---------------------------------------
    // verifyHolds() returns false for a tampered receipt and THROWS only when
    // the check could not be performed. Merging the two is how an unpaid order
    // becomes an entitlement: every `catch` that logs and retries grants one.
    let tampered = receiptJSON.replacingOccurrences(
        of: #""amount_minor":1250"#, with: #""amount_minor":999999"#)
    let holds = try rail.verifyHolds(tampered)
    print("tampered:  \(try rail.verify(tampered)) -> verifyHolds()=\(holds)  <- returned, not thrown")

    // --- 9. the throw path ------------------------------------------------------------------
    // An operational refusal IS a thrown error, and the library's message is
    // malloc'd on the other side of the ABI. `Library.takeString` copies it and
    // frees the original BEFORE the error is constructed — the step a
    // hand-written binding forgets.
    do {
        _ = try rail.charge(#"""
        {"amount_minor":100,"currency":"EUR","destination":"mock:wallet:alice","reference":"order-2"}
        """#)
        print("refused:   (a receipt — WRONG)")
    } catch let error as PatalaError {
        print("refused:   PatalaError: \(error)")
    }

    // A misspelled configuration field is refused, not defaulted.
    do {
        _ = try Rail(configJSON: #"{"rail":"mock","currencys":["USDC"]}"#, library: library)
        print("typo:      (a rail — WRONG)")
    } catch let error as PatalaError {
        print("typo:      refused — \(String(describing: error).prefix(64))...")
    }

    print("\nOK — offline, MockRail only, no value moved.")
} catch {
    FileHandle.standardError.write(Data("error: \(error)\n".utf8))
    exit(1)
}
