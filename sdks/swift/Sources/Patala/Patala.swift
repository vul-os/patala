// Patala.swift — patala's C ABI, in-process, from Swift.
//
// Six symbols, resolved by `dlsym` from `libpatala_ffi`. No module map and no
// `unsafeFlags`, so this package builds with nothing but a Swift toolchain and
// can be a dependency of another package — see Package.swift.
//
// WHAT LOADING THE LIBRARY COSTS YOU
//
//   Almost nothing, and that is the unusual part. patala is Rust: there is no
//   language runtime in your process, no GC, no scheduler, no signal handlers
//   installed, no threads started at load time or ever, and nothing happens at
//   load — no socket, no file, no background task. Each rail owns a
//   current-thread async runtime that drives work on whichever thread called
//   in and is dropped with the rail.
//
//   The equivalent Swift packages for llmux and openrate carry a list of
//   Go-runtime caveats. Those are true there and false here, and have
//   deliberately not been copied.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

import Foundation

#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

// MARK: - Errors

public enum PatalaError: Error, CustomStringConvertible {
    /// A message from the library itself. Plain UTF-8 text, never JSON — print
    /// it, do not parse it.
    case patala(String)
    /// `libpatala_ffi` could not be found or opened.
    case libraryNotFound(String)
    /// The library opened but does not export one of the six symbols — which
    /// usually means it is not `libpatala_ffi` at all.
    case symbolMissing(String)

    public var description: String {
        switch self {
        case .patala(let message): return message
        case .libraryNotFound(let detail): return "libpatala_ffi not found: \(detail)"
        case .symbolMissing(let name): return "libpatala_ffi exports no \(name)"
        }
    }
}

// MARK: - The C ABI

private typealias AbiVersionFn = @convention(c) () -> UnsafePointer<CChar>?
private typealias AbiCheckFn = @convention(c) (
    UnsafePointer<CChar>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32
private typealias NewFn = @convention(c) (
    UnsafePointer<CChar>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> UInt64
private typealias CloseFn = @convention(c) (UInt64) -> Void
private typealias CallFn = @convention(c) (
    UInt64, UnsafePointer<CChar>?, UnsafePointer<CChar>?,
    UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> UnsafeMutablePointer<CChar>?
private typealias FreeFn = @convention(c) (UnsafeMutablePointer<CChar>?) -> Void

/// The loaded shared library. One per path, for the life of the process.
///
/// It is never `dlclose`d. Not because that would hang — it would not; there
/// is no runtime with threads still executing the mapping, which is the
/// hazard the Go-based products in this suite have — but because handles are
/// registry keys inside the library, and unmapping it while a `Rail` still
/// holds one would turn a clean "unknown handle" error into a crash. Keeping
/// one mapping per process is also simply what every host does.
public final class Library {
    private static var cache: [String: Library] = [:]
    private static let cacheLock = NSLock()

    private let handle: UnsafeMutableRawPointer
    fileprivate let abiVersionFn: AbiVersionFn
    fileprivate let abiCheckFn: AbiCheckFn
    fileprivate let newFn: NewFn
    fileprivate let closeFn: CloseFn
    fileprivate let callFn: CallFn
    fileprivate let freeFn: FreeFn

    /// The path this library was opened from. Print it at startup: a shared
    /// library resolves off a load path you may not control.
    public let path: String

    /// Open (or reuse) the library. `path` defaults to ``Library/resolve()``.
    public static func shared(path: String? = nil) throws -> Library {
        let resolved = path ?? resolve()
        cacheLock.lock()
        defer { cacheLock.unlock() }
        if let existing = cache[resolved] { return existing }
        let library = try Library(path: resolved)
        cache[resolved] = library
        return library
    }

    private init(path: String) throws {
        guard let handle = dlopen(path, RTLD_NOW | RTLD_LOCAL) else {
            let reason = dlerror().map { String(cString: $0) } ?? "dlopen failed"
            throw PatalaError.libraryNotFound("\(path): \(reason)")
        }
        self.handle = handle
        self.path = path

        func symbol<T>(_ name: String, as type: T.Type) throws -> T {
            guard let raw = dlsym(handle, name) else {
                throw PatalaError.symbolMissing(name)
            }
            return unsafeBitCast(raw, to: type)
        }
        // Resolve all six up front. A library missing one is not a library to
        // find out about three calls into a payment.
        self.abiVersionFn = try symbol("patala_abi_version", as: AbiVersionFn.self)
        self.abiCheckFn = try symbol("patala_abi_check", as: AbiCheckFn.self)
        self.newFn = try symbol("patala_new", as: NewFn.self)
        self.closeFn = try symbol("patala_close", as: CloseFn.self)
        self.callFn = try symbol("patala_call", as: CallFn.self)
        self.freeFn = try symbol("patala_free", as: FreeFn.self)
    }

    /// Where to look, in order:
    ///
    /// 1. `$PATALA_LIBRARY` — an explicit path always wins.
    /// 2. `target/{release,debug}/libpatala_ffi.{dylib,so}`, walking up from
    ///    the working directory, which is what makes this work from a checkout.
    /// 3. The bare file name, handed to the platform loader.
    public static func resolve() -> String {
        if let explicit = ProcessInfo.processInfo.environment["PATALA_LIBRARY"], !explicit.isEmpty {
            return explicit
        }
        #if canImport(Darwin)
        let name = "libpatala_ffi.dylib"
        #else
        let name = "libpatala_ffi.so"
        #endif
        var directory = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
        for _ in 0..<8 {
            for profile in ["release", "debug"] {
                let candidate = directory
                    .appendingPathComponent("target")
                    .appendingPathComponent(profile)
                    .appendingPathComponent(name)
                if FileManager.default.fileExists(atPath: candidate.path) {
                    return candidate.path
                }
            }
            directory = directory.deletingLastPathComponent()
        }
        return name
    }

    /// The patala version this library was built from. A static string owned
    /// by the library; it is not freed.
    public var abiVersion: String {
        guard let raw = abiVersionFn() else { return "" }
        return String(cString: raw)
    }

    /// Throws unless the library reports exactly `expected`.
    ///
    /// Call this at startup. Without it, a stale `libpatala_ffi` earlier on the
    /// load path is called silently and then misbehaves in ways that look like
    /// patala bugs.
    public func requireABI(_ expected: String) throws {
        var err: UnsafeMutablePointer<CChar>?
        if abiCheckFn(expected, &err) != 0 {
            throw PatalaError.patala(takeString(err) ?? "patala: ABI version mismatch")
        }
    }

    /// Copy a string the library returned and free the original with
    /// `patala_free` — the only function that may free it. Doing the copy and
    /// the free here, in one place, is what stops an error message leaking on
    /// the throw path, which is the allocation a hand-written binding forgets.
    fileprivate func takeString(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { freeFn(pointer) }
        return String(cString: pointer)
    }
}

// MARK: - Rail

/// One rail. `deinit` closes the handle — on the happy path, on a `throw`, on
/// an early `return`. ARC is the RAII here; there is no `close()` to forget.
public final class Rail {
    private let library: Library
    private var handle: UInt64

    /// `configJSON` is a JSON object tagged by `"rail"`; `nil` means the
    /// offline default, a deterministic `MockRail` that needs no credentials
    /// and no network. Unknown fields are **refused** rather than ignored, so
    /// a misspelled key throws instead of quietly building a rail you did not
    /// ask for.
    ///
    /// Building a rail talks to nothing: no socket, no thread, no file.
    public init(configJSON: String? = nil, library: Library? = nil) throws {
        let library = try library ?? Library.shared()
        self.library = library
        var err: UnsafeMutablePointer<CChar>?
        let handle = library.newFn(configJSON, &err)
        guard handle != 0 else {
            throw PatalaError.patala(library.takeString(err) ?? "patala: could not open a rail")
        }
        self.handle = handle
    }

    deinit { library.closeFn(handle) }  // closing twice, or 0, is a no-op

    /// The library this rail was opened from — handy for printing where the
    /// code actually came from.
    public var libraryPath: String { library.path }

    /// One unary call: JSON in, JSON out, `PatalaError.patala` thrown on
    /// failure. `requestJSON` may be `nil` for the methods that ignore it.
    public func call(_ method: String, _ requestJSON: String? = nil) throws -> String {
        var err: UnsafeMutablePointer<CChar>?
        let out = library.callFn(handle, method, requestJSON, &err)
        // Take BOTH before deciding, so neither can leak on either path.
        let result = library.takeString(out)
        let message = library.takeString(err)
        guard let result else {
            throw PatalaError.patala(message ?? "patala: the call failed with no message")
        }
        return result
    }

    public func id() throws -> String { try call("id") }
    public func capabilities() throws -> String { try call("capabilities") }
    public func caveat() throws -> String { try call("caveat") }
    public func providers() throws -> String { try call("providers") }
    public func quote(_ payRequest: String) throws -> String { try call("quote", payRequest) }
    public func charge(_ payRequest: String) throws -> String { try call("charge", payRequest) }
    public func webhook(_ delivery: String) throws -> String { try call("webhook", delivery) }

    public func validateDestination(_ destination: String) throws -> String {
        let request = ["destination": destination]
        let body = try JSONSerialization.data(withJSONObject: request)
        return try call("validate-destination", String(decoding: body, as: UTF8.self))
    }

    /// The raw `{"valid":true}` / `{"valid":false}` document.
    public func verify(_ receipt: String) throws -> String { try call("verify", receipt) }

    /// True only for `{"valid":true}`.
    ///
    /// There are **three** outcomes here, not two, and the difference is the
    /// whole fail-closed contract:
    ///
    /// - `throws` — the check could not be performed. Retry, alert.
    /// - `false` — the rail checked and the receipt does not hold. Never
    ///   retry, never grant.
    /// - `true` — the entitlement.
    ///
    /// A binding that threw on `{"valid":false}` would merge the first two, and
    /// every `catch` that logs-and-retries would then grant entitlements on
    /// unpaid orders. The C ABI keeps them apart by returning a result rather
    /// than `NULL`; this keeps them apart too.
    public func verifyHolds(_ receipt: String) throws -> Bool {
        try verify(receipt).contains("\"valid\":true")
    }
}
