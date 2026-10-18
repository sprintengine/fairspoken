import AVFoundation
import FairspokenCore
import FairspokenUI
import SwiftUI

/// Post-training: keep dictations (audio + what the model heard) so a later step can teach the
/// speech model this voice. Saving is live; the training step itself is not built yet, so its
/// button says what it is waiting for.
struct PostTrainingView: View {
    @Environment(AppModel.self) private var model
    @State private var player = ClipPlayer()
    @State private var confirmDeleteAll = false

    var body: some View {
        let training = model.training
        ScrollView {
            GlassEffectContainer(spacing: Layout.gap) {
                VStack(alignment: .leading, spacing: Layout.gap) {
                    PageTitle("Post-training", subtitle: "Teach the speech model your voice and accent")
                        .padding(.bottom, 4)
                    SavingCard(training: training)
                    HStack(alignment: .top, spacing: Layout.gap) {
                        ProgressCard(summary: training.summary)
                        TrainCard(summary: training.summary, capturing: model.settings.settings.trainingCapture)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    RecordingsCard(training: training, player: player, confirmDeleteAll: $confirmDeleteAll)
                }
                .padding(.horizontal, Layout.pageH)
                .padding(.vertical, Layout.pageTop)
            }
        }
        .scrollEdgeEffectStyle(.soft, for: .top)
        .onAppear { training.refresh() }
        .onDisappear { player.stop() }
        .confirmationDialog("Delete all \(Format.number(training.summary.count)) recordings?", isPresented: $confirmDeleteAll) {
            Button("Delete All", role: .destructive) {
                player.stop()
                training.deleteAll()
            }
        } message: {
            Text("Their audio and text are removed from \(training.folder.path). This can't be undone.")
        }
    }
}

/// The opt-in and where recordings go.
private struct SavingCard: View {
    var training: TrainingLibrary
    @Environment(AppModel.self) private var model

    var body: some View {
        let on = model.settings.settings.trainingCapture
        GlassCard(cornerRadius: Layout.card) {
            VStack(alignment: .leading, spacing: 14) {
                Toggle(isOn: Binding(get: { on }, set: { v in model.settings.update { $0.trainingCapture = v } })) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Save recordings for post-training").cardHeading()
                        Text("Keeps the audio of each dictation and what the model heard, so it can learn your voice later. Nothing is uploaded.")
                            .font(.callout).foregroundStyle(Crystal.ink2).fixedSize(horizontal: false, vertical: true)
                    }
                }
                .toggleStyle(.switch)
                .tint(Crystal.clientAccent)

                Divider().overlay(Crystal.hairline)
                HStack(spacing: 10) {
                    Image(systemName: training.usesDefaultFolder ? "folder" : "externaldrive")
                        .foregroundStyle(Crystal.ink2)
                    VStack(alignment: .leading, spacing: 1) {
                        Text("Saved to").font(.caption).foregroundStyle(Crystal.ink2)
                        Text(training.folder.path.replacingOccurrences(of: NSHomeDirectory(), with: "~"))
                            .font(.fsData(.callout)).foregroundStyle(Crystal.ink)
                            .lineLimit(1).truncationMode(.middle).textSelection(.enabled)
                    }
                    Spacer(minLength: 8)
                    Button("Show in Finder") { training.revealInFinder() }
                        .cardButton().disabled(!training.isAvailable)
                    Button("Change…") { training.chooseFolder() }
                        .cardButton()
                    if !training.usesDefaultFolder {
                        Button("Use Default") { training.useDefaultFolder() }
                            .cardButton()
                            .help("Save to \(TrainingLibrary.defaultFolder.path) on this Mac")
                    }
                }
                if !training.isAvailable {
                    Label("This folder isn't available. Connect its drive, or choose another folder. Dictation keeps working, but nothing is saved until then.",
                          systemImage: "exclamationmark.triangle.fill")
                        .font(.callout).foregroundStyle(Crystal.warn)
                } else if let error = training.lastError {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(.callout).foregroundStyle(Crystal.warn)
                }
                Text("About 2 MB per minute of speech. Recordings can include anything you dictate, so keep them on a drive you control. Saved recordings stay until you delete them.")
                    .font(.caption).foregroundStyle(Crystal.ink3).fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// How much has been kept, against what training wants.
private struct ProgressCard: View {
    var summary: TrainingSummary

    var body: some View {
        GlassCard(cornerRadius: Layout.card, fillHeight: true) {
            VStack(alignment: .leading, spacing: 14) {
                Eyebrow(text: "Your recordings", symbol: "waveform")
                HStack(alignment: .firstTextBaseline, spacing: 24) {
                    stat(Format.duration(summary.seconds), "of speech")
                    stat(Format.number(summary.count), summary.count == 1 ? "recording" : "recordings")
                    stat(Format.bytes(summary.bytes), "on disk")
                }
                ProgressView(value: summary.progress)
                    .tint(Crystal.clientAccent)
                    .accessibilityLabel("Toward the recommended \(Int(TrainingReadiness.recommendedSeconds / 3600)) hours")
                Text(caption)
                    .font(.caption).foregroundStyle(Crystal.ink2).fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var caption: String {
        let minimum = Int(TrainingReadiness.minimumSeconds / 3600)
        let recommended = Int(TrainingReadiness.recommendedSeconds / 3600)
        if summary.seconds >= TrainingReadiness.recommendedSeconds {
            return "Plenty to train on. More still helps, especially of the words you use most."
        }
        if summary.isEnoughToTrain {
            return "Enough to try. \(recommended) hours is recommended for a dependable improvement."
        }
        return "Post-training needs at least \(minimum) hour of speech; \(recommended) hours is recommended."
    }

    private func stat(_ value: String, _ label: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(value).font(.mvNumber(24)).foregroundStyle(Crystal.ink).monospacedDigit()
            Text(label).font(.caption).foregroundStyle(Crystal.ink2)
        }
        .accessibilityElement(children: .combine)
    }
}

/// What post-training does, and its button.
private struct TrainCard: View {
    var summary: TrainingSummary
    var capturing: Bool

    var body: some View {
        GlassCard(cornerRadius: Layout.card, fillHeight: true) {
            VStack(alignment: .leading, spacing: 12) {
                Eyebrow(text: "Post-train", symbol: "graduationcap")
                VStack(alignment: .leading, spacing: 8) {
                    step(1, "Save your dictations as you work.", done: capturing)
                    step(2, "Re-transcribe them with a larger, slower model and find where Parakeet went wrong.")
                    step(3, "Fine-tune Parakeet on your voice and accent.")
                    step(4, "Test it on recordings it never trained on, and switch only if it's better.")
                }
                Spacer(minLength: 0)
                HStack(spacing: 12) {
                    Button("Post-train") {}
                        .buttonStyle(.glassProminent)
                        .tint(Crystal.clientAccent)
                        .disabled(true)
                    Text(status)
                        .font(.caption).foregroundStyle(Crystal.ink2).fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    private var status: String {
        if !summary.isEnoughToTrain {
            let left = TrainingReadiness.minimumSeconds - summary.seconds
            return "Available after \(Format.duration(TrainingReadiness.minimumSeconds)) of recordings (\(Format.duration(left)) to go). Training itself comes in a later update."
        }
        return "Your recordings are ready. Training itself comes in a later update."
    }

    private func step(_ n: Int, _ text: String, done: Bool = false) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: done ? "checkmark.circle.fill" : "\(n).circle")
                .foregroundStyle(done ? Crystal.ok : Crystal.ink3)
            Text(text).font(.callout).foregroundStyle(Crystal.ink).fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }
}

/// The latest kept dictations: listen to one, or delete it.
private struct RecordingsCard: View {
    var training: TrainingLibrary
    var player: ClipPlayer
    @Binding var confirmDeleteAll: Bool
    private static let shown = 30

    var body: some View {
        GlassCard(cornerRadius: Layout.card) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("Recent recordings").cardHeading()
                    Spacer()
                    Button("Delete All…", role: .destructive) { confirmDeleteAll = true }
                        .cardButton()
                        .disabled(training.entries.isEmpty || !training.isAvailable)
                }
                if training.entries.isEmpty {
                    Text(training.isAvailable ? "Nothing saved yet. Turn on saving above, then dictate as usual." : "Connect the drive to see saved recordings.")
                        .font(.callout).foregroundStyle(Crystal.ink2)
                        .padding(.vertical, 8)
                } else {
                    ForEach(training.entries.prefix(Self.shown)) { entry in
                        row(entry)
                        if entry.id != training.entries.prefix(Self.shown).last?.id { Divider().overlay(Crystal.hairline) }
                    }
                    if training.entries.count > Self.shown {
                        Text("and \(Format.number(training.entries.count - Self.shown)) more in the folder")
                            .font(.caption).foregroundStyle(Crystal.ink3)
                    }
                }
            }
        }
    }

    private func row(_ entry: TrainingEntry) -> some View {
        let playing = player.playingID == entry.id
        return HStack(alignment: .top, spacing: 12) {
            Button {
                playing ? player.stop() : player.play(entry.id, url: training.audioURL(for: entry.id))
            } label: {
                Image(systemName: playing ? "stop.fill" : "play.fill")
                    .frame(width: 28, height: 28)
                    .well(cornerRadius: 8)
            }
            .buttonStyle(.plain)
            .foregroundStyle(playing ? Crystal.clientAccent : Crystal.ink2)
            .accessibilityLabel(playing ? "Stop" : "Play recording")
            VStack(alignment: .leading, spacing: 3) {
                Text(entry.rawText).font(.callout).foregroundStyle(Crystal.ink)
                    .lineLimit(3).textSelection(.enabled)
                HStack(spacing: 6) {
                    Text("\(Format.relative(entry.date)) · \(Format.duration(entry.durationSeconds)) · \(entry.speechModel)")
                        .font(.caption).foregroundStyle(Crystal.ink3)
                    if entry.appCategory != AppCategory.other.rawValue {
                        Tag(text: entry.appCategory.capitalized, color: Crystal.ink2)
                    }
                }
            }
            Spacer(minLength: 8)
            Button {
                if playing { player.stop() }
                training.delete(entry.id)
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.plain)
            .foregroundStyle(Crystal.ink3)
            .help("Delete this recording")
            .accessibilityLabel("Delete recording")
        }
        .padding(.vertical, 2)
    }
}

/// Plays one kept recording at a time.
@Observable
final class ClipPlayer: NSObject, AVAudioPlayerDelegate {
    private(set) var playingID: String?
    @ObservationIgnored private var player: AVAudioPlayer?

    func play(_ id: String, url: URL) {
        stop()
        guard let next = try? AVAudioPlayer(contentsOf: url) else { return }
        next.delegate = self
        next.play()
        player = next
        playingID = id
    }

    func stop() {
        player?.stop()
        player = nil
        playingID = nil
    }

    nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        Task { @MainActor in self.stop() }
    }
}
