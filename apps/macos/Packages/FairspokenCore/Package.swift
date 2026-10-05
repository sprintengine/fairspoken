// swift-tools-version: 6.2
// Platform-neutral core of the native macOS app: wire protocol codec, URL policy,
// SSE parser, host event model + reducer, settings/history schemas shared with the
// Rust app, audio maths and the engine seams. No AppKit, no FluidAudio: everything
// here is unit-testable on any Mac (and on CI runners without a Neural Engine).
import PackageDescription

let package = Package(
    name: "FairspokenCore",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "FairspokenCore", targets: ["FairspokenCore"])
    ],
    targets: [
        .target(name: "FairspokenCore"),
        .testTarget(name: "FairspokenCoreTests", dependencies: ["FairspokenCore"]),
    ]
)
