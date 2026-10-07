import FairspokenUI
import Charts
import FairspokenCore
import SwiftUI

struct HomeView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ScrollView {
            GlassEffectContainer(spacing: Layout.gap) {
                VStack(alignment: .leading, spacing: Layout.gap) {
                    PageTitle(greeting, subtitle: Date.now.formatted(.dateTime.weekday(.wide).day().month(.wide)))
                        .padding(.bottom, 4)
                    HStack(alignment: .top, spacing: Layout.gap) {
                        TryDictationCard()
                            .frame(maxWidth: .infinity)
                        TimeSavedCard(usage: model.history.usage)
                            .frame(width: 300)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    StatsStrip()
                    RecentDictationsCard()
                }
                .padding(.horizontal, Layout.pageH)
                .padding(.vertical, Layout.pageTop)
            }
        }
        .scrollEdgeEffectStyle(.soft, for: .top)
    }

    private var greeting: String {
        let hour = Calendar.current.component(.hour, from: .now)
        return hour < 12 ? "Good morning" : hour < 18 ? "Good afternoon" : "Good evening"
    }
}

// MARK: - Try dictation

struct TryDictationCard: View {
    @Environment(AppModel.self) private var model
    @State private var copied = false

    var body: some View {
        let dictation = model.dictation
        GlassCard(cornerRadius: Layout.card, padding: 24, fillHeight: true) {
            HStack(alignment: .center, spacing: 24) {
                VStack(alignment: .leading, spacing: 12) {
                    Text(title)
                        .font(.system(size: 24, weight: .semibold))
                        .tracking(-0.3)
                        .foregroundStyle(Crystal.ink)
                        .contentTransition(.opacity)
                    instructions
                    resultArea
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                MicButton(dictation: dictation) { dictation.toggleFromUI() }
            }
            .frame(maxHeight: .infinity)
        }
    }

    private var title: String {
        switch model.dictation.phase {
        case .listening: "Listening…"
        case .transcribing: "Transcribing…"
        case .failed: "Try again"
        default: model.dictation.lastInAppResult == nil ? "Ready" : "You said"
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
        }
        .font(.callout)
        .foregroundStyle(Crystal.ink2)
        .fixedSize()
    }

    @ViewBuilder private var resultArea: some View {
        switch model.dictation.phase {
        case .listening:
            LevelBars(meter: model.dictation.meter, active: true, bars: 40, color: Crystal.live)
                .frame(height: 34)
                .padding(.top, 4)
        case .failed(let message):
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(Crystal.warn)
        default:
            if let result = model.dictation.lastInAppResult {
                VStack(alignment: .leading, spacing: 10) {
                    Text(result.text)
                        .font(.title3)
                        .foregroundStyle(Crystal.ink)
                        .textSelection(.enabled)
                        .lineLimit(4)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 10) {
                        Text("\(result.words) words · \(Format.ms(Double(result.latencyMs)))")
                            .font(.caption).foregroundStyle(Crystal.ink2).monospacedDigit()
                        Button(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc") {
                            TextInserter.copy(result.text)
                            copied = true
                            Task { try? await Task.sleep(for: .seconds(1.5)); copied = false }
                        }
                        .cardButton()
                        .controlSize(.small)
                    }
                }
                .padding(14)
                .frame(maxWidth: .infinity, alignment: .leading)
                .well()
            }
        }
    }
}

/// Big tactile mic: the screen's one accent control, rings that breathe with the live level.
struct MicButton: View {
    var dictation: DictationController
    var size: CGFloat = 112
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
                        let r = size / 2 + CGFloat(phase) * (24 + level * 36)
                        let alpha = listening ? (1 - phase) * (0.25 + Double(level) * 0.5) : 0
                        gc.stroke(Path(ellipseIn: CGRect(x: c.x - r, y: c.y - r, width: r * 2, height: r * 2)),
                                  with: .color(Crystal.live.opacity(alpha)), lineWidth: 2)
                    }
                    if !listening {
                        for (i, extra) in [16.0, 32.0].enumerated() {
                            let r = size / 2 + extra
                            gc.stroke(Path(ellipseIn: CGRect(x: c.x - r, y: c.y - r, width: r * 2, height: r * 2)),
                                      with: .color(Crystal.ink3.opacity(0.28 - Double(i) * 0.12)), lineWidth: 1)
                        }
                    }
                }
            }
            .frame(width: size + 84, height: size + 84)
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
                    // An explicit fill keeps the control's colour in inactive windows.
                    let base = listening ? Crystal.live : Crystal.clientAccent
                    Circle()
                        .fill(LinearGradient(colors: [base.mix(with: .white, by: 0.08), base.mix(with: .black, by: 0.16)],
                                             startPoint: .top, endPoint: .bottom))
                        .overlay(Circle().strokeBorder(LinearGradient(colors: [.white.opacity(0.45), .white.opacity(0.05)],
                                                                      startPoint: .top, endPoint: .bottom), lineWidth: 1))
                        .shadow(color: base.opacity(0.28), radius: 16, y: 8)
                }
                .contentShape(.circle)
            }
            .buttonStyle(PressScaleStyle())
            .disabled(busy)
            .keyboardShortcut(.space, modifiers: [])
            .accessibilityLabel(listening ? "Stop dictation" : "Start dictation")
        }
        .frame(width: size + 84, height: size + 84)
        .animation(reduceMotion ? nil : .smooth(duration: 0.3), value: listening)
    }
}

/// A gentle press response for the mic (it sits inside a glass card, so it isn't glass itself).
private struct PressScaleStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed && !reduceMotion ? 0.96 : 1)
            .brightness(configuration.isPressed ? -0.04 : 0)
            .animation(.smooth(duration: 0.15), value: configuration.isPressed)
    }
}

// MARK: - Stats

struct TimeSavedCard: View {
    var usage: UsageStats

    var body: some View {
        let saved = usage.timeSavedSeconds
        GlassCard(cornerRadius: Layout.card, padding: 22, fillHeight: true) {
            VStack(alignment: .leading, spacing: 10) {
                Eyebrow(text: "Time saved")
                HStack(alignment: .firstTextBaseline, spacing: 2) {
                    Text("\(Int(saved / 3600))").font(.mvNumber(46))
                    Text("h").font(.title3.weight(.medium)).foregroundStyle(Crystal.ink2)
                    Text(" \(Int(saved.truncatingRemainder(dividingBy: 3600) / 60))").font(.mvNumber(46))
                    Text("m").font(.title3.weight(.medium)).foregroundStyle(Crystal.ink2)
                }
                .foregroundStyle(Crystal.ink)
                Text("vs typing at 40 wpm").font(.caption).foregroundStyle(Crystal.ink2)
                Divider().overlay(Crystal.hairline).padding(.vertical, 4)
                row("Words", Format.number(usage.totalWords))
                row("Dictations", Format.number(usage.totalDictations))
                if let wpm = usage.speakingWPM { row("Pace", "\(Int(wpm)) wpm") }
            }
            .font(.callout)
        }
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label).foregroundStyle(Crystal.ink2)
            Spacer()
            Text(value).foregroundStyle(Crystal.ink).monospacedDigit()
        }
        .accessibilityElement(children: .combine)
    }
}

/// Today, this week, latency and streak in one card.
struct StatsStrip: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let usage = model.history.usage
        let today = usage.bucket(daysAgo: 0)
        let week = usage.words(lastDays: 7)
        let lastWeek = usage.words(lastDays: 14) - week
        let streak = usage.streak()
        GlassCard(cornerRadius: Layout.card, padding: 0) {
            HStack(spacing: 0) {
                StatColumn(title: "Today", value: Format.number(today.words), unit: "words") {
                    Text("\(today.dictations) dictation\(today.dictations == 1 ? "" : "s")")
                }
                divider
                StatColumn(title: "This week", value: Format.number(week), unit: "words") {
                    if lastWeek > 0 {
                        let change = Double(week - lastWeek) / Double(lastWeek) * 100
                        Label("\(Int(abs(change).rounded()))% vs last week", systemImage: change >= 0 ? "arrow.up.right" : "arrow.down.right")
                            .foregroundStyle(change >= 0 ? Crystal.ok : Crystal.ink2)
                    } else {
                        Text("Last 7 days")
                    }
                } accessory: {
                    WeekBars(usage: usage)
                }
                divider
                StatColumn(title: "Latency", value: model.history.typicalLatencyMs.map { "\($0)" } ?? "—", unit: "ms") {
                    Text("Median, release to text")
                }
                divider
                StatColumn(title: "Streak", value: "\(streak)", unit: streak == 1 ? "day" : "days") {
                    Text("In a row")
                }
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private var divider: some View {
        Rectangle().fill(Crystal.hairline).frame(width: 1).padding(.vertical, 18)
    }
}

private struct StatColumn<Footnote: View, Accessory: View>: View {
    var title: String
    var value: String
    var unit: String
    @ViewBuilder var footnote: Footnote
    @ViewBuilder var accessory: Accessory

    init(title: String, value: String, unit: String, @ViewBuilder footnote: () -> Footnote,
         @ViewBuilder accessory: () -> Accessory = { EmptyView() }) {
        self.title = title
        self.value = value
        self.unit = unit
        self.footnote = footnote()
        self.accessory = accessory()
    }

    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            VStack(alignment: .leading, spacing: 6) {
                Eyebrow(text: title)
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(value).font(.mvNumber(28)).foregroundStyle(Crystal.ink).lineLimit(1).fixedSize()
                    Text(unit).font(.callout).foregroundStyle(Crystal.ink2).lineLimit(1).fixedSize()
                }
                footnote.font(.caption).foregroundStyle(Crystal.ink2).lineLimit(1)
            }
            .layoutPriority(1)
            Spacer(minLength: 0)
            accessory
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

private struct WeekBars: View {
    var usage: UsageStats

    var body: some View {
        let series = usage.series(days: 7)
        Chart(Array(series.enumerated()), id: \.offset) { i, day in
            BarMark(x: .value("Day", day.date, unit: .day), y: .value("Words", day.bucket.words), width: 6)
                .foregroundStyle(i == series.count - 1 ? Crystal.clientAccent : Crystal.ink3.opacity(0.45))
                .clipShape(Capsule())
        }
        .chartXAxis(.hidden)
        .chartYAxis(.hidden)
        .frame(width: 56, height: 36)
        .accessibilityLabel("Words per day this week")
    }
}

// MARK: - Recent

struct RecentDictationsCard: View {
    @Environment(AppModel.self) private var model
    @State private var copiedID: String?
    @State private var hovered: String?

    var body: some View {
        GlassCard(cornerRadius: Layout.card, padding: 16) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Recent").cardHeading()
                    .padding(.horizontal, 8)
                    .padding(.top, 4)
                    .accessibilityAddTraits(.isHeader)
                if model.history.items.isEmpty {
                    ContentUnavailableView("No dictations yet", systemImage: "waveform",
                                           description: Text("Press \(HotkeyController.currentShortcutSymbols) in any app."))
                        .frame(maxWidth: .infinity, minHeight: 140)
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
                Text(item.text).foregroundStyle(Crystal.ink).lineLimit(1).truncationMode(.tail)
                Text(meta(item)).font(.caption).foregroundStyle(Crystal.ink2).lineLimit(1)
            }
            Spacer(minLength: 8)
            Text(Format.relative(item.date)).font(.caption).foregroundStyle(Crystal.ink3).monospacedDigit()
            Button {
                TextInserter.copy(item.text)
                copiedID = item.id
                Task { try? await Task.sleep(for: .seconds(1.4)); if copiedID == item.id { copiedID = nil } }
            } label: {
                Image(systemName: copiedID == item.id ? "checkmark" : "doc.on.doc")
                    .foregroundStyle(copiedID == item.id ? Crystal.ok : Crystal.ink2)
                    .frame(width: 16, height: 16)
            }
            .buttonStyle(.borderless)
            .opacity(hovered == item.id || copiedID == item.id ? 1 : 0.35)
            .help("Copy")
            .accessibilityLabel("Copy dictation")
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 8)
        .background(hovered == item.id ? Crystal.well : .clear, in: .rect(cornerRadius: Layout.row))
        .onHover { hovered = $0 ? item.id : (hovered == item.id ? nil : hovered) }
        .contextMenu {
            Button("Copy") { TextInserter.copy(item.text) }
            Button("Delete", role: .destructive) { model.history.delete(item.id) }
        }
    }

    private func meta(_ item: TranscriptHistoryItem) -> String {
        var parts = [item.appName ?? "Dictation", "\(item.wordCount) words"]
        if let ms = item.timings?.totalMs, ms > 0 { parts.append("\(ms) ms") }
        return parts.joined(separator: " · ")
    }
}

struct AppIconView: View {
    var bundleID: String?
    var body: some View {
        Group {
            if let bundleID, let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID) {
                Image(nsImage: NSWorkspace.shared.icon(forFile: url.path)).resizable()
            } else {
                Image(systemName: "text.bubble").resizable().scaledToFit().padding(7)
                    .foregroundStyle(Crystal.ink2)
                    .background(Crystal.well, in: .rect(cornerRadius: 7))
            }
        }
        .frame(width: 28, height: 28)
        .accessibilityHidden(true)
    }
}
