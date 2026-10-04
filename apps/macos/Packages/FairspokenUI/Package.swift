// swift-tools-version: 6.2
// SwiftUI building blocks shared by Fairspoken and Fairspoken Server: the palette (brand
// colours from design/brand/README.md plus the client's status colours), glass and
// material cards, chips, tags, the brand mark and number formatting.
import PackageDescription

let package = Package(
    name: "FairspokenUI",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "FairspokenUI", targets: ["FairspokenUI"])
    ],
    dependencies: [
        .package(path: "../MultiVoiceCore")
    ],
    targets: [
        .target(name: "FairspokenUI", dependencies: ["MultiVoiceCore"],
                swiftSettings: [.defaultIsolation(MainActor.self)])
    ]
)
