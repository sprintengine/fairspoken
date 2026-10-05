// swift-tools-version: 6.2
// In-app updates for Fairspoken and Fairspoken Server: Sparkle 2 behind one UpdateController,
// the update channel (stable / nightly) and the shared SwiftUI pieces (sidebar button, toast,
// Settings section). Kept apart from MultiVoiceCore so the core stays free of Sparkle.
//
//   FairspokenUpdateModel   pure Swift, unit-tested: channel resolution, the state machine's
//                           states, what each state looks like and says
//   FairspokenUpdates       Sparkle (custom user driver) + SwiftUI
import PackageDescription

let package = Package(
    name: "FairspokenUpdates",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "FairspokenUpdates", targets: ["FairspokenUpdates"])
    ],
    dependencies: [
        // Keep in step with project.yml (packages.Sparkle).
        .package(url: "https://github.com/sparkle-project/Sparkle", exact: "2.10.0")
    ],
    targets: [
        .target(name: "FairspokenUpdateModel"),
        .target(name: "FairspokenUpdates",
                dependencies: ["FairspokenUpdateModel", .product(name: "Sparkle", package: "Sparkle")],
                swiftSettings: [.defaultIsolation(MainActor.self)]),
        .testTarget(name: "FairspokenUpdateModelTests", dependencies: ["FairspokenUpdateModel"]),
    ]
)
