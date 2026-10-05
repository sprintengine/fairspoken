// swift-tools-version: 6.2
// The transcription host behind Fairspoken Server: an HTTP/1.1 server on Network.framework
// that speaks the host protocol in src-tauri/src/host/PROTOCOL.md byte for byte (the Rust
// `transcription-host` is the reference implementation), plus the worker pool, job queue,
// metrics and the `/v1/events` fan-out. The speech engine is injected (`HostSpeechBackend`),
// so this package has no FluidAudio dependency and its tests run on any Mac, CI included.
import PackageDescription

let package = Package(
    name: "FairspokenHost",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "FairspokenHost", targets: ["FairspokenHost"]),
        .executable(name: "fairspoken-host-dev", targets: ["fairspoken-host-dev"]),
    ],
    dependencies: [
        .package(path: "../FairspokenCore")
    ],
    targets: [
        .target(name: "FairspokenHost", dependencies: ["FairspokenCore"]),
        .executableTarget(name: "fairspoken-host-dev", dependencies: ["FairspokenHost"]),
        .testTarget(name: "FairspokenHostTests", dependencies: ["FairspokenHost", "FairspokenCore"]),
    ]
)
