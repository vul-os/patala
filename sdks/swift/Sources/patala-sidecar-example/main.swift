// patala from Swift, sidecar — `patala-sidecar` as a child process on
// loopback, driven over HTTP with URLSession.
//
// The same payout as the direct example, and the same JSON: the sidecar and
// libpatala_ffi serialize the same `patala-core` types, so a body that works
// here works against `patala_call` unchanged.
//
// This program loads NO patala library — it needs a socket.
//
// Why a Swift program would choose this over loading the library:
//
//   - KEY ISOLATION. A non-custodial rail's signing key lives in whichever
//     process calls charge. Five services loading the library means the key is
//     in five address spaces; one sidecar puts it in one narrow process.
//   - There is no shared library for your platform — which, for Swift, is the
//     live one: iOS is not a place you dlopen a .dylib you built yourself.
//   - Your process is sandboxed in a way that forbids loading it.
//
// Not on that list, and this is the difference from the equivalent example in
// llmux and openrate: fork-safety, signal handlers and a runtime in your
// process. patala is Rust.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

import Foundation

#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

// MARK: - helpers

/// Ask the kernel for an unused loopback port and hand it straight back. Racy
/// by construction — there is no portable "reserve a port for my child" —
/// which is why a silent child is reported as a startup failure below.
func freePort() -> UInt16 {
    let fd = socket(AF_INET, SOCK_STREAM, 0)
    guard fd >= 0 else { return 0 }
    defer { close(fd) }
    var addr = sockaddr_in()
    addr.sin_family = sa_family_t(AF_INET)
    addr.sin_port = 0
    addr.sin_addr.s_addr = inet_addr("127.0.0.1")
    let bound = withUnsafePointer(to: &addr) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
        }
    }
    guard bound == 0 else { return 0 }
    var out = sockaddr_in()
    var length = socklen_t(MemoryLayout<sockaddr_in>.size)
    let named = withUnsafeMutablePointer(to: &out) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &length) }
    }
    guard named == 0 else { return 0 }
    return UInt16(bigEndian: out.sin_port)
}

/// The sidecar refuses to start without a token: no unauthenticated mode and
/// no auto-generated fallback. A real deployment generates this once and keeps
/// it out of the environment of everything that is not the sidecar or its
/// client.
func randomToken() throws -> String {
    let handle = try FileHandle(forReadingFrom: URL(fileURLWithPath: "/dev/urandom"))
    defer { try? handle.close() }
    guard let bytes = try handle.read(upToCount: 32), bytes.count == 32 else {
        throw Failure("could not read /dev/urandom")
    }
    return bytes.map { String(format: "%02x", $0) }.joined()
}

struct Failure: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

struct Response {
    let status: Int
    let body: String
}

struct Sidecar {
    let port: UInt16
    let token: String?

    func get(_ path: String) async throws -> Response { try await send("GET", path, nil) }
    func post(_ path: String, _ body: String) async throws -> Response {
        try await send("POST", path, body)
    }

    func send(_ method: String, _ path: String, _ body: String?) async throws -> Response {
        var request = URLRequest(url: URL(string: "http://127.0.0.1:\(port)\(path)")!)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let token {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        if let body { request.httpBody = Data(body.utf8) }
        let (data, response) = try await URLSession.shared.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        return Response(status: status, body: String(decoding: data, as: UTF8.self))
    }

    /// Poll `/healthz` — the one unauthenticated route — until the child is
    /// listening. Mandatory: `Process.run()` returns long before a socket is
    /// bound.
    func waitHealthy() async throws {
        let anonymous = Sidecar(port: port, token: nil)
        for _ in 0..<500 {
            if let response = try? await anonymous.get("/healthz"), response.status == 200 {
                return
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        throw Failure("the sidecar never became healthy on port \(port)")
    }
}

/// Pull one field out for printing.
///
/// Two Foundation traps are handled here rather than papered over, because
/// both bite on this exact wire format:
///
///   - `JSONSerialization` hands back `NSNumber` for booleans as well as
///     numbers, and interpolating an `NSNumber` bool prints `0`/`1`. Ask
///     CoreFoundation which it is.
///   - The same `NSNumber` will hand you a `.doubleValue` for `amount_minor`
///     without complaint, and a `Double` loses every integer above 2^53. This
///     is money: decode amounts into `UInt64`, as the direct example does with
///     `Codable`, or read them as `Int64`/`UInt64` here. Never `Double`.
func field(_ document: String, _ key: String) -> String {
    guard let data = document.data(using: .utf8),
        let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
        let value = object[key]
    else { return "?" }
    if let number = value as? NSNumber, CFGetTypeID(number as CFTypeRef) == CFBooleanGetTypeID() {
        return number.boolValue ? "true" : "false"
    }
    return "\(value)"
}

// MARK: - the flow

let payRequest = #"""
{"amount_minor":1250,"currency":"USDC","destination":"mock:wallet:alice","reference":"order-1"}
"""#

let arguments = CommandLine.arguments
let environment = ProcessInfo.processInfo.environment
let binary = arguments.count > 1 ? arguments[1] : (environment["PATALA_SIDECAR_BIN"] ?? "")

guard !binary.isEmpty else {
    FileHandle.standardError.write(
        Data(
            """
            usage: patala-sidecar-example <path-to-patala-sidecar>
               or: PATALA_SIDECAR_BIN=... patala-sidecar-example
            build one with:  cargo build -p patala-sidecar --release

            """.utf8))
    exit(2)
}

let port = freePort()
guard port != 0 else {
    FileHandle.standardError.write(Data("no free loopback port\n".utf8))
    exit(1)
}

do {
    let token = try randomToken()
    print("patala sidecar (Swift, child process) — 127.0.0.1:\(port)")
    print("binary:    \(binary)")

    let child = Process()
    child.executableURL = URL(fileURLWithPath: binary)
    var childEnvironment = environment
    childEnvironment["PATALA_SIDECAR_PORT"] = String(port)
    childEnvironment["PATALA_SIDECAR_TOKEN"] = token
    child.environment = childEnvironment
    // The sidecar logs to stdout; send it to /dev/null so this example's
    // output is its own. stderr is left alone, so a refusal to start is still
    // visible.
    child.standardOutput = FileHandle.nullDevice
    try child.run()
    // Kill it on every path out of here, including a thrown error.
    defer {
        child.terminate()
        child.waitUntilExit()
    }

    let sidecar = Sidecar(port: port, token: token)
    try await sidecar.waitHealthy()
    print("health:    ok")

    // --- the token gate ------------------------------------------------------
    // Every /v1 route is behind it, including the read-only capabilities
    // lookup. Missing, malformed and wrong are all the same 401.
    let anonymous = Sidecar(port: port, token: nil)
    print("no token:  HTTP \(try await anonymous.get("/v1/rails/mock").status)")

    // --- capabilities ----------------------------------------------------------
    let caps = try await sidecar.get("/v1/rails/mock")
    print(
        "caps:      HTTP \(caps.status) \(field(caps.body, "class")) "
            + "holds_funds=\(field(caps.body, "holds_funds"))")

    // --- pre-flight ----------------------------------------------------------
    // All five verdicts come back as 200. Read the BODY: a rail's honest
    // refusal is data, and flattening five states into "worked / did not work"
    // loses the only one that matters.
    let dest = try await sidecar.post(
        "/v1/rails/mock/validate-destination", #"{"destination":"stellar:wallet:alice"}"#)
    print(
        "dest:      HTTP \(dest.status) \(field(dest.body, "status")) "
            + "is_refusal=\(field(dest.body, "is_refusal")) "
            + "human_must_confirm=\(field(dest.body, "human_must_confirm"))")

    // A malformed REQUEST is a 400 with no verdict fields at all, so a rejected
    // request can never be mistaken for a checked address.
    let typo = try await sidecar.post(
        "/v1/rails/mock/validate-destination", #"{"destinaton":"typo"}"#)
    print("typo:      HTTP \(typo.status) — a bad request is not a verdict")

    // --- quote ------------------------------------------------------------------
    let quote = try await sidecar.post("/v1/rails/mock/quote", payRequest)
    print(
        "quote:     HTTP \(quote.status) total_minor=\(field(quote.body, "total_minor")) "
            + "(an integer on the wire, never a float)")

    // --- charge -> verify -----------------------------------------------------
    let charged = try await sidecar.post("/v1/rails/mock/charge", payRequest)
    guard charged.status == 200 else {
        throw Failure("charge: HTTP \(charged.status) \(charged.body)")
    }
    print(
        "charge:    HTTP 200 \(field(charged.body, "amount_minor")) minor units "
            + "ref=\(field(charged.body, "reference"))")

    let verified = try await sidecar.post("/v1/rails/mock/verify", charged.body)
    print("verify:    HTTP \(verified.status) \(verified.body)  <- the entitlement check")

    // A tampered receipt is HTTP 200 with {"valid":false}, NOT a 4xx. Branch on
    // the body. The day someone adds "retry on 4xx" to a shared HTTP helper, a
    // status-code-only integration grants an unpaid order.
    let tampered = charged.body.replacingOccurrences(
        of: #""amount_minor":1250"#, with: #""amount_minor":999999"#)
    let bad = try await sidecar.post("/v1/rails/mock/verify", tampered)
    print("tampered:  HTTP \(bad.status) \(bad.body)  <- 200, and false")

    // --- the edges ---------------------------------------------------------------
    print(
        "no rail:   HTTP \(try await sidecar.get("/v1/rails/solana").status) "
            + "— the sidecar's registry is mock-only")
    print(
        "webhook:   HTTP "
            + "\(try await sidecar.post("/v1/rails/mock/webhook", #"{"hello":"there"}"#).status) "
            + "— the mock has no processor, so it invents no event")

    print("\nOK — offline, MockRail only, no value moved. Child reaped on exit.")
} catch {
    FileHandle.standardError.write(Data("error: \(error)\n".utf8))
    exit(1)
}
