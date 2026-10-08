import Foundation
import Testing
@testable import FairspokenUpdateModel

@Suite("Update channel")
struct UpdateChannelTests {
    @Test func nightlyIsRecognisedByItsVersion() {
        #expect(UpdateChannel.isNightly(version: "0.3.0-nightly.20261005.4"))
        #expect(!UpdateChannel.isNightly(version: "0.3.0"))
        #expect(!UpdateChannel.isNightly(version: "0.3.0-beta.1"))
        // "nightly" without the dot is not the nightly scheme.
        #expect(!UpdateChannel.isNightly(version: "0.3.0-nightly"))
    }

    @Test func absentChoiceFollowsTheBuild() {
        #expect(UpdateChannel.resolve(saved: nil, version: "0.3.0") == .stable)
        #expect(UpdateChannel.resolve(saved: nil, version: "0.3.0-nightly.20261005.4") == .nightly)
    }

    @Test func savedChoiceWins() {
        #expect(UpdateChannel.resolve(saved: "nightly", version: "0.3.0") == .nightly)
        #expect(UpdateChannel.resolve(saved: "stable", version: "0.3.0-nightly.20261005.4") == .stable)
    }

    @Test func unknownSavedValueFallsBackToTheBuild() {
        #expect(UpdateChannel.resolve(saved: "beta", version: "0.3.0-nightly.20261005.4") == .nightly)
        #expect(UpdateChannel.resolve(saved: "", version: "0.3.0") == .stable)
    }

    @Test func firstLaunchSavesTheBuildsChannelSoAPromotedStableKeepsNightly() throws {
        let suite = "UpdateChannelTests.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }

        // A nightly install that never opened the picker.
        #expect(UpdateChannel.resolveAndRemember(in: defaults, version: "0.5.0-nightly.20261005.4") == .nightly)
        #expect(defaults.string(forKey: UpdateChannel.defaultsKey) == "nightly")
        // It takes the promoted stable 0.5.0 and relaunches: still nightly.
        #expect(UpdateChannel.resolveAndRemember(in: defaults, version: "0.5.0") == .nightly)

        // A choice made in Settings is left alone; an unknown value is replaced by what runs.
        defaults.set("stable", forKey: UpdateChannel.defaultsKey)
        #expect(UpdateChannel.resolveAndRemember(in: defaults, version: "0.6.0-nightly.20261007.1") == .stable)
        defaults.set("beta", forKey: UpdateChannel.defaultsKey)
        #expect(UpdateChannel.resolveAndRemember(in: defaults, version: "0.5.0") == .stable)
        #expect(defaults.string(forKey: UpdateChannel.defaultsKey) == "stable")
    }

    @Test func sparkleChannels() {
        #expect(UpdateChannel.stable.sparkleChannels.isEmpty)
        #expect(UpdateChannel.nightly.sparkleChannels == ["nightly"])
    }

    @Test func moveToStableNoteOnlyForANightlyOnStable() {
        #expect(UpdateChannel.showsMoveToStableNote(channel: .stable, currentVersion: "0.3.0-nightly.20261005.4"))
        #expect(!UpdateChannel.showsMoveToStableNote(channel: .nightly, currentVersion: "0.3.0-nightly.20261005.4"))
        #expect(!UpdateChannel.showsMoveToStableNote(channel: .stable, currentVersion: "0.3.0"))
        #expect(!UpdateChannel.showsMoveToStableNote(channel: .nightly, currentVersion: "0.3.0"))
    }
}

@Suite("Update state")
struct UpdateStateTests {
    @Test func clickActions() {
        #expect(UpdateState.idle.primaryAction == .check)
        #expect(UpdateState.checking.primaryAction == .none)
        #expect(UpdateState.available(version: "0.3.0").primaryAction == .install)
        #expect(UpdateState.downloading(version: "0.3.0", progress: 0.4).primaryAction == .none)
        #expect(UpdateState.ready(version: "0.3.0").primaryAction == .relaunch)
        #expect(UpdateState.error(message: "offline").primaryAction == .check)
    }

    @Test func pendingMeansSomethingWaitsForThePerson() {
        #expect(UpdateState.available(version: "0.3.0").isPending)
        #expect(UpdateState.ready(version: "0.3.0").isPending)
        #expect(!UpdateState.downloading(version: "0.3.0", progress: nil).isPending)
        #expect(!UpdateState.idle.isPending)
        #expect(!UpdateState.checking.isPending)
        #expect(!UpdateState.error(message: "x").isPending)
    }

    @Test func downloadProgressFillsNinetyPercentThenUnpacks() {
        #expect(DownloadProgress().fraction == nil)
        #expect(abs(DownloadProgress(expectedLength: 200, receivedLength: 100).fraction! - 0.45) < 1e-9)
        #expect(abs(DownloadProgress(expectedLength: 100, receivedLength: 150).fraction! - 0.9) < 1e-9)
        #expect(abs(DownloadProgress(expectedLength: 100, receivedLength: 100, extraction: 0.5).fraction! - 0.95) < 1e-9)
        #expect(abs(DownloadProgress(extraction: 2).fraction! - 1) < 1e-9)
    }

    @Test func offeredOncePerVersionUnlessAsked() {
        #expect(UpdateOfferLedger.shouldOffer(version: "0.3.0", lastOffered: nil, userInitiated: false))
        #expect(!UpdateOfferLedger.shouldOffer(version: "0.3.0", lastOffered: "0.3.0", userInitiated: false))
        #expect(UpdateOfferLedger.shouldOffer(version: "0.3.0", lastOffered: "0.3.0", userInitiated: true))
        #expect(UpdateOfferLedger.shouldOffer(version: "0.3.1", lastOffered: "0.3.0", userInitiated: false))
    }

    @Test func displayNameCarriesTheOfferedVersionsChannel() {
        #expect(updateDisplayName(appName: "Fairspoken", version: "0.3.0") == "Fairspoken 0.3.0")
        #expect(updateDisplayName(appName: "Fairspoken", version: "0.3.0-nightly.20261005.4") == "Fairspoken 0.3.0-nightly.20261005.4 (Nightly)")
    }
}

@Suite("Update samples")
struct UpdateSampleTests {
    @Test func everyNamedSampleExists() {
        for name in UpdateState.sampleNames { #expect(UpdateState.sample(named: name) != nil) }
        #expect(UpdateState.sample(named: "Ready", version: "1.0.0") == .ready(version: "1.0.0"))
        #expect(UpdateState.sample(named: "nonsense") == nil)
    }
}

@Suite("Update presentation")
struct UpdatePresentationTests {
    private func make(_ state: UpdateState, channel: UpdateChannel = .stable) -> UpdatePresentation {
        UpdatePresentation.make(state: state, appName: "Fairspoken", currentVersion: "0.2.0", channel: channel)
    }

    @Test func idleIsSyncAndChecks() {
        let p = make(.idle)
        #expect(p.symbol == UpdatePresentation.syncSymbol)
        #expect(!p.spinning && p.isEnabled && p.badge == .none && !p.showsRing)
        #expect(p.accessibilityLabel.contains("Fairspoken 0.2.0 (Stable)"))
    }

    @Test func checkingSpinsAndIsDisabled() {
        let p = make(.checking, channel: .nightly)
        #expect(p.symbol == UpdatePresentation.syncSymbol)
        #expect(p.spinning && !p.isEnabled)
        #expect(p.accessibilityLabel.contains("(Nightly)"))
    }

    @Test func availableIsDownloadWithACountBadge() {
        let p = make(.available(version: "0.3.0-nightly.20261005.4"))
        #expect(p.symbol == UpdatePresentation.downloadSymbol)
        #expect(p.badge == .count(1) && p.isEnabled)
        #expect(p.accessibilityLabel == "Update available: Fairspoken 0.3.0-nightly.20261005.4 (Nightly) — click to install")
    }

    @Test func downloadingShowsTheRing() {
        let p = make(.downloading(version: "0.3.0", progress: 0.42))
        #expect(p.showsRing && p.progress == 0.42 && !p.isEnabled && p.badge == .none)
        #expect(p.title == "Downloading… 42 %")
        let unknown = make(.downloading(version: "0.3.0", progress: nil))
        #expect(unknown.showsRing && unknown.progress == nil && unknown.title == "Downloading…")
    }

    @Test func readyIsRestartWithADot() {
        let p = make(.ready(version: "0.3.0"))
        #expect(p.symbol == UpdatePresentation.restartSymbol)
        #expect(p.badge == .dot && p.isEnabled)
        #expect(p.menuTitle == "Restart to Install Fairspoken 0.3.0")
    }

    @Test func errorIsSyncWithAWarningDotAndTheMessage() {
        let p = make(.error(message: "The network connection was lost."))
        #expect(p.symbol == UpdatePresentation.syncSymbol)
        #expect(p.badge == .warning && p.isEnabled)
        #expect(p.accessibilityLabel.contains("The network connection was lost."))
    }
}
