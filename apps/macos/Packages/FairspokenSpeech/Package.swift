// swift-tools-version: 6.2
// Speech recognition on the Apple Neural Engine, shared by Fairspoken (the dictation client)
// and Fairspoken Server: the only code that touches FluidAudio (Parakeet CoreML). Absorbs
// FluidAudio API churn in one place; FluidAudio is pinned exactly.
import PackageDescription

let package = Package(
    name: "FairspokenSpeech",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "FairspokenSpeech", targets: ["FairspokenSpeech"])
    ],
    dependencies: [
        .package(url: "https://github.com/FluidInference/FluidAudio.git", exact: "0.17.5"),
        .package(path: "../FairspokenCore"),
    ],
    targets: [
        .target(name: "FairspokenSpeech", dependencies: [
            .product(name: "FluidAudio", package: "FluidAudio"),
            "FairspokenCore",
        ]),
        // Opt-in checks against real models (skipped unless FAIRSPOKEN_REAL_MODEL_LINK is set);
        // not part of scripts/test.sh.
        .testTarget(name: "FairspokenSpeechTests", dependencies: [
            "FairspokenSpeech",
            "FairspokenCore",
            .product(name: "FluidAudio", package: "FluidAudio"),
        ]),
    ]
)
