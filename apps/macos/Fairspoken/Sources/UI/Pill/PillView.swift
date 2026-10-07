import FairspokenUI
import FairspokenCore
import SwiftUI

/// The floating recording pill: listening (live level + timer) → transcribing → inserted / error.
/// One Liquid Glass capsule that morphs between states.
struct PillView: View {
    var dictation: DictationController
    var shortcut: String
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        content
            .padding(.horizontal, 16)
            .frame(height: 46)
            .glassEffect(dictation.phase.isListening ? .regular.tint(Crystal.live.opacity(0.08)) : .regular, in: .capsule)
        .animation(reduceMotion ? nil : .spring(response: 0.42, dampingFraction: 0.82), value: stateKey)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.updatesFrequently)
    }

    @ViewBuilder private var content: some View {
        switch dictation.phase {
        case .listening(let since):
            HStack(spacing: 12) {
                StatusDot(color: Crystal.live, pulsing: true)
                LevelBars(meter: dictation.meter, active: true, bars: 22, color: Crystal.live)
                    .frame(width: 118, height: 24)
                TimelineView(.periodic(from: since, by: 1)) { ctx in
                    Text(Self.clock(ctx.date.timeIntervalSince(since)))
                        .font(.system(.callout, design: .rounded).weight(.semibold).monospacedDigit())
                        .foregroundStyle(Crystal.ink2)
                }
            }
            .accessibilityLabel("Listening")
        case .transcribing:
            HStack(spacing: 10) {
                ProgressView().controlSize(.small)
                Text("Transcribing").font(.callout.weight(.semibold))
            }
            .accessibilityLabel("Transcribing")
        case .done(let result):
            HStack(spacing: 9) {
                Image(systemName: icon(for: result.delivery)).foregroundStyle(Crystal.ok)
                    .symbolEffect(.bounce, value: result.text)
                Text(headline(for: result)).font(.callout.weight(.semibold)).lineLimit(1)
                Text("\(result.words) words · \(Format.ms(Double(result.latencyMs)))")
                    .font(.callout).foregroundStyle(Crystal.ink2).lineLimit(1)
            }
        case .failed(let message):
            HStack(spacing: 9) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Crystal.warn)
                Text(message.count > 64 ? message.prefix(62) + "…" : message)
                    .font(.callout.weight(.medium)).lineLimit(1)
            }
        case .idle:
            HStack(spacing: 8) {
                Image(systemName: "waveform").foregroundStyle(Crystal.clientAccent)
                Text("Ready").font(.callout.weight(.medium))
                Text(shortcut).font(.callout).foregroundStyle(Crystal.ink2)
            }
        }
    }

    private var stateKey: Int {
        switch dictation.phase {
        case .idle: 0
        case .listening: 1
        case .transcribing: 2
        case .done: 3
        case .failed: 4
        }
    }

    private func icon(for delivery: DictationController.Delivery) -> String {
        switch delivery {
        case .pasted: "checkmark.circle.fill"
        case .copied: "doc.on.clipboard.fill"
        case .shownInApp: "text.badge.checkmark"
        }
    }

    private func headline(for result: DictationController.Result) -> String {
        switch result.delivery {
        case .pasted(let app): app.map { "Inserted into \($0)" } ?? "Inserted"
        case .copied(let reason): reason
        case .shownInApp: "Done"
        }
    }

    static func clock(_ seconds: TimeInterval) -> String {
        let s = max(0, Int(seconds))
        return String(format: "%d:%02d", s / 60, s % 60)
    }
}
