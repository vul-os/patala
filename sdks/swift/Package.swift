// swift-tools-version:5.9
//
// No `unsafeFlags`, no module map, no system-library target, no bridging
// header. patala's C ABI is reached with `dlopen`/`dlsym` plus
// `@convention(c)` function types, which buys three things:
//
//   - `swift build` works with nothing on the machine but a Swift toolchain —
//     no header, no `-I`, no `-L`.
//   - The library is located at RUN time, so one build works whether
//     `libpatala_ffi` sits in `target/release/`, on `DYLD_LIBRARY_PATH`, or
//     wherever `$PATALA_LIBRARY` points.
//   - This package can be a dependency of another package. A target carrying
//     `unsafeFlags` cannot be, which is the usual reason a Swift C-interop
//     package that "works locally" cannot be consumed.
//
// Zero external dependencies.

import PackageDescription

let package = Package(
    name: "Patala",
    platforms: [
        // Tested on macOS 15.7.3 (Apple silicon) with Swift 6.1.2. The floor
        // is 13 for `URLSession.data(for:)`, which the sidecar example uses.
        .macOS(.v13)
    ],
    products: [
        .library(name: "Patala", targets: ["Patala"]),
        .executable(name: "patala-direct-example", targets: ["patala-direct-example"]),
        .executable(name: "patala-sidecar-example", targets: ["patala-sidecar-example"]),
        .executable(name: "patala-checks", targets: ["patala-checks"]),
    ],
    targets: [
        .target(name: "Patala"),
        .executableTarget(name: "patala-direct-example", dependencies: ["Patala"]),
        .executableTarget(name: "patala-sidecar-example", dependencies: ["Patala"]),
        // Deliberately an executable and not a `.testTarget`: XCTest ships
        // with Xcode, and on a Command Line Tools-only machine — which is
        // where these examples were written and run — `import XCTest` does not
        // compile at all. See Sources/patala-checks/main.swift.
        .executableTarget(name: "patala-checks", dependencies: ["Patala"]),
    ]
)
