import FairspokenUI
import Charts
import FairspokenCore
import SwiftUI

struct HomeView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ScrollView {
            GlassEffectContainer(spacing: 4) {
                VStack(alignment: .leading, spacing: 18) {
                    header
                    HStack(alignment: .top, spacing: 16) {
                        TryDictationCard()
                            .frame(maxWidth: .infinity)
                        TimeSavedCard(usage: model.history.usage)
                            .frame(width: 290)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    StatsRow()
                    HStack(alignment: .top, spacing: 16) {
                        RecentDictationsCard()
                            .frame(maxWidth: .infinity)
                        EngineCard()
                            .frame(width: 290)
                    }
                }
                .padding(.horizontal, 28)
                .padding(.vertical, 22)
            }
        }
        .scrollEdgeEffectStyle(.soft, for: .top)
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 4) {
                Text(greeting).font(.system(size: 30, weight: .bold, design: .rounded))
                Text(Date.now.formatted(.dateTime.weekday(.wide).day().month(.wide)))
                    .font(.title3).foregroundStyle(.secondary)
            }
            Spacer()
            HStack(spacing: 10) {
                if model.settings.settings.transcriptionLocation != .local {
                    Chip(text: "Sent to your host", symbol: "server.rack", tint: .mvIndigo)
                }
                EngineStatusChip()
            }
        }
    }

    private var greeting: String {
        let hour = Calendar.current.component(.hour, from: .now)
        return hour < 12 ? "Good morning" : hour < 18 ? "Good afternoon" : "Good evening"
    }
}

struct EngineStatusChip: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let remote = model.settings.settings.transcriptionLocation == .remoteHost
        let state = model.models.engineState
        Chip(text: remote ? "Host mode" : state.isReady ? "Ready" : state.label,
             dot: remote ? .mvIndigo : state.isReady ? .mvGreen : state.isBusy ? .mvAmber : .secondary)
            .accessibilityLabel("Engine status: \(remote ? "host" : state.label)")
    }
}

// MARK: - Try dictation

struct TryDictationCard: View {
    @Environment(AppModel.self) private var model
    @State private var copied = false

    var body: some View {
        let dictation = model.dictation
        GlassCard(cornerRadius: 28, padding: 26) {
            HStack(alignment: .center, spacing: 24) {
                VStack(alignment: .leading, spacing: 12) {
                    Eyebrow(text: "Dictation", symbol: "mic")
                    Text(title).font(.system(size: 26, weight: .semibold, design: .rounded))
                        .contentTransition(.opacity)
                    instructions
                    resultArea
                }
                Spacer(minLength: 8)
                MicButton(dictation: dictation) { dictation.toggleFromUI() }
            }
        }
    }

    private var title: String {
        switch model.dictation.phase {
        case .listening: "Listening…"
        case .transcribing: "Transcribing…"
        case .failed: "Let's try that again"
        default: model.dictation.lastInAppResult == nil ? "Ready when you are" : "Here's what you said"
        }
    }

    @ViewBuilder private var instructions: some View {
        let s = model.settings.settings
        HStack(spacing: 6) {
            Text(s.recordingShortcutMode == .pushToTalk ? "Hold" : "Press")
            ShortcutCaps(description: HotkeyController.currentShortcutSymbols)
            if s.fnPushToTalk {
                Text("or hold")
                Keycap(label: "fn")
            }
            Text("in any app, or click the mic.")
        }
        .font(.body)
        .foregroundStyle(.secondary)
    }

    @ViewBuilder private var resultArea: some View {
        switch model.dictation.phase {
        case .listening:
            LevelBars(meter: model.dictation.meter, active: true, bars: 40, color: .mvCoral)
                .frame(height: 34)
                .padding(.top, 4)
        case .failed(let message):
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(Color.mvAmber)
        default:
            if let result = model.dictation.lastInAppResult {
                VStack(alignment: .leading, spacing: 8) {
                    Text(result.text)
                        .font(.title3)
                        .textSelection(.enabled)
                        .lineLimit(4)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 10) {
                        Tag(text: "\(result.words) words", symbol: "text.word.spacing")
                        Tag(text: "\(Format.ms(Double(result.latencyMs))) to text", symbol: "bolt.fill", color: .mvTeal)
                        Button(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc") {
                            TextInserter.copy(result.text)
                            copied = true
                            Task { try? await Task.sleep(for: .seconds(1.5)); copied = false }
                        }
                        .buttonStyle(.glass)
                        .controlSize(.small)
                    }
                }
            } else {
                Text("Your words appear at the cursor. Try a sentence here first: the text will show up in this card.")
                    .font(.callout).foregroundStyle(.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// Big tactile mic: interactive glass, rings that breathe with the live level.
struct MicButton: View {
    var dictation: DictationController
    var size: CGFloat = 128
    var action: () -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let listening = dictation.phase.isListening
        let busy = dictation.phase == .transcribing
        ZStack {
            TimelineView(.animation(minimumInterval: 1 / 60, paused: !listening || reduceMotion)) { ctx in
                Canvas { gc, sz in
                    let level = CGFloat(dictation.meter.snapshot().level)
                    let t = ctx.date.timeIntervalSinceReferenceDate
                    let c = CGPoint(x: sz.width / 2, y: sz.height / 2)
                    for i in 0..<3 {
                        let phase = (t * 0.6 + Double(i) / 3).truncatingRemainder(dividingBy: 1)
                        let r = size / 2 + CGFloat(phase) * (26 + level * 40)
                        let alpha = listening ? (1 - phase) * (0.25 + Double(level) * 0.5) : 0
                        gc.stroke(Path(ellipseIn: CGRect(x: c.x - r, y: c.y - r, width: r * 2, height: r * 2)),
                                  with: .color(Color.mvCoral.opacity(alpha)), lineWidth: 2)
                    }
                    if !listening {
                        for (i, extra) in [18.0, 36.0].enumerated() {
                            let r = size / 2 + extra
                            gc.stroke(Path(ellipseIn: CGRect(x: c.x - r, y: c.y - r, width: r * 2, height: r * 2)),
                                      with: .color(Color.mvTeal.opacity(0.22 - Double(i) * 0.08)), lineWidth: 1)
                        }
                    }
                }
            }
            .frame(width: size + 100, height: size + 100)
            .allowsHitTesting(false)

            Button(action: action) {
                ZStack {
                    if busy {
                        ProgressView().controlSize(.large).tint(.white)
                    } else {
                        Image(systemName: listening ? "stop.fill" : "mic.fill")
                            .font(.system(size: size * 0.3, weight: .semibold))
                            .contentTransition(.symbolEffect(.replace))
                    }
                }
                .foregroundStyle(.white)
                .frame(width: size, height: size)
                .background {
                    // Explicit fill so the control keeps its colour in inactive windows;
                    // interactive glass on top gives the Liquid Glass press response.
                    Circle().fill(LinearGradient(colors: listening ? [Color.mvCoral, Color.mvCoral.mix(with: .black, by: 0.12)]
                                                                  : [Color.mvTeal.mix(with: .white, by: 0.1), Color.mvTeal.mix(with: .mvIndigo, by: 0.35)],
                                                 startPoint: .topLeading, endPoint: .bottomTrailing))
                        .shadow(color: (listening ? Color.mvCoral : Color.mvTeal).opacity(0.45), radius: 18, y: 8)
                }
                .contentShape(.circle)
            }
            .buttonStyle(.plain)
            .glassEffect(.clear.interactive(), in: .circle)
            .disabled(busy)
            .keyboardShortcut(.space, modifiers: [])
            .accessibilityLabel(listening ? "Stop dictation" : "Start dictation")
        }
        .frame(width: size + 100, height: size + 100)
        .animation(reduceMotion ? nil : .smooth(duration: 0.3), value: listening)
    }
}

// MARK: - Stats

struct TimeSavedCard: View {
    var usage: UsageStats

    var body: some View {
        let saved = usage.timeSavedSeconds
        GlassCard(cornerRadius: 28, padding: 24) {
            VStack(alignment: .leading, spacing: 10) {
                Eyebrow(text: "Time saved", symbol: "hourglass")
                HStack(alignment: .firstTextBaseline, spacing: 2) {
                    Text("\(Int(saved / 3600))").font(.mvNumber(54, weight: .bold))
                    Text("h").font(.title2.weight(.semibold)).foregroundStyle(.secondary)
                    Text(" \(Int(saved.truncatingRemainder(dividingBy: 3600) / 60))").font(.mvNumber(54, weight: .bold))
                    Text("m").font(.title2.weight(.semibold)).foregroundStyle(.secondary)
                }
                Text("compared with typing at 40 wpm")
                    .font(.callout).foregroundStyle(.secondary)
                Divider().padding(.vertical, 4)
                LabeledContent("Words dictated") { Text(Format.number(usage.totalWords)).monospacedDigit() }
                LabeledContent("Dictations") { Text(Format.number(usage.totalDictations)).monospacedDigit() }
                if let wpm = usage.speakingWPM {
                    LabeledContent("Speaking pace") { Text("\(Int(wpm)) wpm").monospacedDigit() }
                }
            }
            .font(.callout)
        }
    }
}

struct StatsRow: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let usage = model.history.usage
        let today = usage.bucket(daysAgo: 0)
        let week = usage.words(lastDays: 7)
        let lastWeek = usage.words(lastDays: 14) - week
        HStack(spacing: 16) {
            StatTile(title: "Today", value: Format.number(today.words), unit: "words",
                     footnote: "\(today.dictations) dictation\(today.dictations == 1 ? "" : "s")", symbol: "sun.max")
            WeekTile(usage: usage, words: week, previous: lastWeek)
            StatTile(title: "Release → text", value: model.history.typicalLatencyMs.map { "\($0)" } ?? "—", unit: "ms",
                     footnote: "median of recent dictations", symbol: "bolt")
            StatTile(title: "Streak", value: "\(usage.streak())", unit: usage.streak() == 1 ? "day" : "days",
                     footnote: "days in a row with a dictation", symbol: "flame")
        }
        .fixedSize(horizontal: false, vertical: true)
    }
}

struct StatTile: View {
    var title: String
    var value: String
    var unit: String
    var footnote: String
    var symbol: String

    var body: some View {
        GlassCard(cornerRadius: 22, padding: 18, fillHeight: true) {
            VStack(alignment: .leading, spacing: 6) {
                Eyebrow(text: title, symbol: symbol)
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(value).font(.mvNumber(32, weight: .bold))
                    Text(unit).font(.callout.weight(.medium)).foregroundStyle(.secondary)
                }
                Text(footnote).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

struct WeekTile: View {
    var usage: UsageStats
    var words: Int
    var previous: Int

    var body: some View {
        let series = usage.series(days: 7)
        GlassCard(cornerRadius: 22, padding: 18, fillHeight: true) {
            VStack(alignment: .leading, spacing: 6) {
                Eyebrow(text: "This week", symbol: "calendar")
                HStack(alignment: .bottom, spacing: 10) {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(alignment: .firstTextBaseline, spacing: 4) {
                            Text(Format.number(words)).font(.mvNumber(32, weight: .bold)).lineLimit(1).fixedSize()
                            Text("words").font(.callout.weight(.medium)).foregroundStyle(.secondary)
                        }
                        if previous > 0 {
                            let change = Double(words - previous) / Double(previous) * 100
                            Label("\(Int(abs(change).rounded()))% vs last week", systemImage: change >= 0 ? "arrow.up.right" : "arrow.down.right")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(change >= 0 ? Color.mvGreen : .secondary)
                                .lineLimit(1)
                        } else {
                            Text("last 7 days").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .layoutPriority(1)
                    Spacer(minLength: 0)
                    Chart(Array(series.enumerated()), id: \.offset) { i, day in
                        BarMark(x: .value("Day", day.date, unit: .day), y: .value("Words", day.bucket.words), width: 7)
                            .foregroundStyle(i == series.count - 1 ? Color.mvTeal : Color.mvTeal.opacity(0.35))
                            .clipShape(Capsule())
                    }
                    .chartXAxis(.hidden)
                    .chartYAxis(.hidden)
                    .frame(width: 74, height: 46)
                    .accessibilityLabel("Words per day this week")
                }
            }
        }
    }
}

// MARK: - Recent + engine

struct RecentDictationsCard: View {
    @Environment(AppModel.self) private var model
    @State private var copiedID: String?
    @State private var hovered: String?

    var body: some View {
        GlassCard(cornerRadius: 26, padding: 20) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("Recent dictations").font(.headline)
                    Spacer()
                    Text("\(model.history.items.count) kept on this Mac").font(.caption).foregroundStyle(.secondary)
                }
                if model.history.items.isEmpty {
                    ContentUnavailableView("No dictations yet", systemImage: "waveform",
                                           description: Text("Press \(HotkeyController.currentShortcutSymbols) anywhere and start talking."))
                        .frame(maxWidth: .infinity, minHeight: 160)
                } else {
                    VStack(spacing: 2) {
                        ForEach(model.history.items.prefix(6)) { item in
                            row(item)
                        }
                    }
                }
            }
        }
    }

    private func row(_ item: TranscriptHistoryItem) -> some View {
        HStack(alignment: .center, spacing: 12) {
            AppIconView(bundleID: item.appBundleId)
            VStack(alignment: .leading, spacing: 2) {
                Text(item.text).lineLimit(1).truncationMode(.tail)
                HStack(spacing: 6) {
                    Text(item.appName ?? "Dictation")
                    Text("·")
                    Text("\(item.wordCount) words")
                    if let ms = item.timings?.totalMs, ms > 0 {
                        Text("·")
                        Text("\(ms) ms")
                    }
                }
                .font(.caption).foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            Text(Format.relative(item.date)).font(.caption).foregroundStyle(.secondary).monospacedDigit()
            Button {
                TextInserter.copy(item.text)
                copiedID = item.id
                Task { try? await Task.sleep(for: .seconds(1.4)); if copiedID == item.id { copiedID = nil } }
            } label: {
                Image(systemName: copiedID == item.id ? "checkmark" : "doc.on.doc")
                    .frame(width: 16, height: 16)
            }
            .buttonStyle(.glass)
            .controlSize(.small)
            .opacity(hovered == item.id || copiedID == item.id ? 1 : 0.55)
            .help("Copy")
            .accessibilityLabel("Copy dictation")
        }
        .padding(.vertical, 7)
        .padding(.horizontal, 8)
        .background(hovered == item.id ? Color.primary.opacity(0.05) : .clear, in: .rect(cornerRadius: 12))
        .onHover { hovered = $0 ? item.id : (hovered == item.id ? nil : hovered) }
        .contextMenu {
            Button("Copy") { TextInserter.copy(item.text) }
            Button("Delete", role: .destructive) { model.history.delete(item.id) }
        }
    }
}

struct AppIconView: View {
    var bundleID: String?
    var body: some View {
        Group {
            if let bundleID, let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID) {
                Image(nsImage: NSWorkspace.shared.icon(forFile: url.path)).resizable()
            } else {
                Image(systemName: "text.bubble.fill").resizable().scaledToFit().padding(6)
                    .foregroundStyle(Color.mvTeal)
                    .background(Color.mvTeal.opacity(0.14), in: .rect(cornerRadius: 7))
            }
        }
        .frame(width: 28, height: 28)
        .accessibilityHidden(true)
    }
}

struct EngineCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let summary = model.engineSummary
        let state = model.models.engineState
        GlassCard(cornerRadius: 26, padding: 20) {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Text("Speech engine").font(.headline)
                    Spacer()
                    Button("Models") { model.section = .models }
                        .buttonStyle(.glass).controlSize(.small)
                }
                HStack(spacing: 12) {
                    Image(systemName: summary.placement.symbol)
                        .font(.title2)
                        .foregroundStyle(summary.placement.tint)
                        .frame(width: 44, height: 44)
                        .background(summary.placement.tint.opacity(0.14), in: .rect(cornerRadius: 12))
                    VStack(alignment: .leading, spacing: 2) {
                        Text(summary.title).font(.body.weight(.semibold)).lineLimit(1)
                        Text(summary.detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                if summary.placement == .neuralEngine {
                    HStack(spacing: 8) {
                        StatusDot(color: state.isReady ? .mvGreen : state.isBusy ? .mvAmber : .secondary, pulsing: state.isBusy)
                        Text(state.label).font(.callout)
                    }
                    if case .downloading(let f) = state { ProgressView(value: f).tint(.mvTeal) }
                    LabeledContent("Memory while loaded") { Text("≈ \(Format.bytes(model.models.activeModel.memoryBytes))") }
                        .font(.caption).foregroundStyle(.secondary)
                    if let load = model.models.lastLoadSeconds {
                        LabeledContent("Loaded in") { Text(String(format: "%.1f s", load)) }
                            .font(.caption).foregroundStyle(.secondary)
                    }
                } else {
                    Text(model.hostStatus.summary).font(.callout).foregroundStyle(.secondary).lineLimit(1)
                    Button("Host settings", systemImage: "server.rack") {
                        model.settingsTab = .transcription
                        model.section = .settings
                    }
                        .buttonStyle(.glass).controlSize(.small)
                }
            }
        }
    }
}
