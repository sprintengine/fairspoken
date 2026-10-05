import SwiftUI

/// Colours the update UI borrows from the app: the client's teal, the server's Copybook Blue.
public struct UpdateStyle: Sendable {
    public var accent: Color
    public var warning: Color

    public init(accent: Color, warning: Color) {
        self.accent = accent
        self.warning = warning
    }
}

// MARK: - Glyph

/// The update icon in its state: sync (spinning while checking), download with a `1` badge,
/// a progress ring while downloading, restart with a dot, sync with a warning dot.
public struct UpdateGlyph: View {
    var presentation: UpdatePresentation
    var style: UpdateStyle
    var size: CGFloat
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    public init(presentation: UpdatePresentation, style: UpdateStyle, size: CGFloat = 30) {
        self.presentation = presentation
        self.style = style
        self.size = size
    }

    public var body: some View {
        let p = presentation
        ZStack {
            if p.showsRing {
                Circle().stroke(Color.primary.opacity(0.12), lineWidth: ringWidth)
                if let progress = p.progress {
                    Circle()
                        .trim(from: 0, to: max(progress, 0.02))
                        .stroke(style.accent, style: StrokeStyle(lineWidth: ringWidth, lineCap: .round))
                        .rotationEffect(.degrees(-90))
                        .animation(reduceMotion ? nil : .linear(duration: 0.25), value: progress)
                } else {
                    IndeterminateArc(color: style.accent, lineWidth: ringWidth, animated: !reduceMotion)
                }
            }
            Image(systemName: p.symbol)
                .font(.system(size: size * (p.showsRing ? 0.36 : 0.46), weight: .semibold))
                .foregroundStyle(p.showsRing || p.badge == .count(1) || p.badge == .dot ? style.accent : Color.primary.opacity(0.75))
                // Reduced motion: the spinner becomes a gentle pulse (opacity only).
                .symbolEffect(.rotate.wholeSymbol, options: .repeat(.continuous), isActive: p.spinning && !reduceMotion)
                .symbolEffect(.pulse, options: .repeat(.continuous), isActive: p.spinning && reduceMotion)
                .contentTransition(.symbolEffect(.replace))
        }
        .frame(width: size, height: size)
        .overlay(alignment: .topTrailing) { badge.offset(x: badgeOffset, y: -badgeOffset) }
        .accessibilityHidden(true)
    }

    private var ringWidth: CGFloat { max(2, size / 12) }
    private var badgeOffset: CGFloat { size * 0.12 }

    @ViewBuilder private var badge: some View {
        switch presentation.badge {
        case .none:
            EmptyView()
        case .count(let n):
            Text("\(n)")
                .font(.system(size: max(9, size * 0.3), weight: .bold, design: .rounded))
                .monospacedDigit()
                .foregroundStyle(.white)
                .padding(.horizontal, 3.5)
                .frame(minWidth: size * 0.46, minHeight: size * 0.46)
                .background(style.accent, in: .capsule)
                .overlay(Capsule().strokeBorder(.background, lineWidth: 1.5))
        case .dot:
            dot(style.accent)
        case .warning:
            dot(style.warning)
        }
    }

    private func dot(_ color: Color) -> some View {
        Circle()
            .fill(color)
            .frame(width: size * 0.3, height: size * 0.3)
            .overlay(Circle().strokeBorder(.background, lineWidth: 1.5))
    }
}

private struct IndeterminateArc: View {
    var color: Color
    var lineWidth: CGFloat
    var animated: Bool
    @State private var turning = false

    var body: some View {
        Circle()
            .trim(from: 0, to: 0.28)
            .stroke(color, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
            .rotationEffect(.degrees(turning ? 270 : -90))
            .animation(animated ? .linear(duration: 1).repeatForever(autoreverses: false) : nil, value: turning)
            .onAppear { if animated { turning = true } }
    }
}

// MARK: - Sidebar button

/// The update button at the foot of a sidebar: the glyph in a glass circle and a two-line
/// caption. Clicking checks, installs or restarts, by state.
public struct UpdateSidebarButton: View {
    var updates: UpdateController
    var style: UpdateStyle

    public init(updates: UpdateController, style: UpdateStyle) {
        self.updates = updates
        self.style = style
    }

    public var body: some View {
        let p = updates.presentation
        Button {
            updates.performPrimaryAction()
        } label: {
            HStack(spacing: 10) {
                UpdateGlyph(presentation: p, style: style, size: 30)
                    .padding(4)
                    .glassEffect(.regular.interactive(p.isEnabled), in: .circle)
                VStack(alignment: .leading, spacing: 1) {
                    Text(p.title)
                        .font(.callout.weight(p.isEnabled && p.badge != .none ? .semibold : .medium))
                        .foregroundStyle(.primary)
                        .contentTransition(.numericText())
                    Text(p.detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                Spacer(minLength: 0)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .allowsHitTesting(p.isEnabled)
        .help(p.accessibilityLabel)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(p.accessibilityLabel)
        .accessibilityAddTraits(.isButton)
        .accessibilityRespondsToUserInteraction(p.isEnabled)
        .animation(.smooth(duration: 0.25), value: p)
    }
}

// MARK: - Toast

/// "Fairspoken 0.3.0 is available" with Update / Later, bottom-trailing over the content.
/// Shown once per version; "Up to date" only after a check the person asked for.
public struct UpdateToastView: View {
    var updates: UpdateController
    var style: UpdateStyle

    public init(updates: UpdateController, style: UpdateStyle) {
        self.updates = updates
        self.style = style
    }

    public var body: some View {
        if let toast = updates.toast {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: symbol(toast.kind))
                    .font(.title2)
                    .foregroundStyle(toast.kind == .failed ? style.warning : style.accent)
                    .frame(width: 28)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 3) {
                    Text(toast.title).font(.callout.weight(.semibold))
                    if let message = toast.message {
                        Text(message).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                    if let action = toast.actionTitle {
                        HStack(spacing: 8) {
                            Button("Later") { updates.dismissToast() }
                                .buttonStyle(.glass)
                            Button(action) { updates.performPrimaryAction() }
                                .buttonStyle(.glassProminent)
                                .tint(style.accent)
                                .keyboardShortcut(.defaultAction)
                        }
                        .controlSize(.small)
                        .padding(.top, 6)
                    }
                }
                Spacer(minLength: 0)
                Button {
                    updates.dismissToast()
                } label: {
                    Image(systemName: "xmark").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                        .frame(width: 18, height: 18)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Dismiss")
            }
            .padding(14)
            .frame(width: 340)
            .glassEffect(.regular, in: .rect(cornerRadius: 18))
            .accessibilityElement(children: .contain)
            .id(toast.id)
        }
    }

    private func symbol(_ kind: UpdateController.Toast.Kind) -> String {
        switch kind {
        case .available: UpdatePresentation.downloadSymbol
        case .ready: UpdatePresentation.restartSymbol
        case .upToDate: "checkmark.circle"
        case .failed: "exclamationmark.triangle"
        }
    }
}

extension View {
    /// Overlays the update toast at the bottom-trailing corner and announces it to VoiceOver.
    public func updateToast(_ updates: UpdateController, style: UpdateStyle) -> some View {
        modifier(UpdateToastModifier(updates: updates, style: style))
    }
}

private struct UpdateToastModifier: ViewModifier {
    var updates: UpdateController
    var style: UpdateStyle
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content
            .overlay(alignment: .bottomTrailing) {
                UpdateToastView(updates: updates, style: style)
                    .padding(20)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
            .animation(reduceMotion ? .easeInOut(duration: 0.15) : .spring(response: 0.4, dampingFraction: 0.85), value: updates.toast)
            .onChange(of: updates.toast) { _, toast in
                if let toast { AccessibilityNotification.Announcement(toast.title).post() }
            }
    }
}

// MARK: - Settings

/// Settings › Updates, as one Form section: version and channel, the Stable/Nightly picker with
/// a line on each, the move-to-Stable note, status, last check, and Check for Updates.
public struct UpdateSettingsSection: View {
    var updates: UpdateController
    var style: UpdateStyle

    public init(updates: UpdateController, style: UpdateStyle) {
        self.updates = updates
        self.style = style
    }

    public var body: some View {
        let p = updates.presentation
        Section {
            LabeledContent("Current version") {
                Text("\(updates.currentVersion) (\(updates.build))")
                    .monospacedDigit()
                    .textSelection(.enabled)
            }
            Picker("Channel", selection: Binding(get: { updates.channel }, set: { updates.setChannel($0) })) {
                ForEach(UpdateChannel.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented)
            .fixedSize()
            VStack(alignment: .leading, spacing: 4) {
                ForEach(UpdateChannel.allCases) { channel in
                    Text("\(Text(channel.title).fontWeight(.semibold)): \(channel.explanation)")
                        .foregroundStyle(channel == updates.channel ? .primary : .secondary)
                }
                if updates.showsMoveToStableNote {
                    Label("You'll move to Stable with the next stable release", systemImage: "info.circle")
                        .foregroundStyle(style.accent)
                        .padding(.top, 2)
                }
            }
            .font(.caption)
            LabeledContent("Status") {
                HStack(spacing: 8) {
                    UpdateGlyph(presentation: p, style: style, size: 20)
                    VStack(alignment: .trailing, spacing: 0) {
                        Text(p.title)
                        if updates.state != .idle && updates.state != .checking {
                            Text(p.detail).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                        }
                    }
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(p.accessibilityLabel)
            }
            LabeledContent("Last checked") {
                if let date = updates.lastChecked {
                    Text(date.formatted(date: .abbreviated, time: .shortened))
                } else {
                    Text("Never")
                }
            }
            HStack {
                Spacer()
                switch updates.state {
                case .available(let version):
                    Button("Install \(version)") { updates.installUpdate() }
                        .buttonStyle(.glassProminent).tint(style.accent)
                case .ready:
                    Button("Restart to Update") { updates.relaunchToUpdate() }
                        .buttonStyle(.glassProminent).tint(style.accent)
                default:
                    EmptyView()
                }
                Button(updates.state == .checking ? "Checking…" : "Check for Updates") { updates.checkForUpdates() }
                    .buttonStyle(.glass)
                    .disabled(updates.state == .checking || updates.state.primaryAction == .none)
            }
        } header: {
            Text("Updates")
        } footer: {
            Text("\(updates.appName) checks shortly after it opens and every 6 hours, and asks before installing. Updates are signed; only ones signed by Fairspoken install.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }
}
