import FairspokenHost
import FairspokenUI
import FairspokenCore
import FairspokenUpdates
import SwiftUI

/// Main window: the Crystal theme (design/crystal/README.md) with the server's garnet hint.
/// Liquid Glass sidebar and toolbar; each page's cards share one `GlassEffectContainer`.
struct ServerWindowView: View {
    @Environment(ServerController.self) private var controller
    @Environment(UpdateController.self) private var updates

    var body: some View {
        NavigationSplitView {
            sidebar.navigationSplitViewColumnWidth(min: 210, ideal: 230, max: 280)
        } detail: {
            ZStack {
                PageBackground().backgroundExtensionEffect()
                detail.transition(.opacity)
            }
            .animation(.smooth(duration: 0.25), value: controller.section)
            .updateToast(updates, style: .server)
            .toolbar { toolbar }
        }
        .frame(minWidth: 1120, minHeight: 760)
    }

    private var sidebar: some View {
        List(selection: Binding<ServerController.Section?>(get: { controller.section }, set: { if let s = $0 { controller.section = s } })) {
            Section {
                ForEach(ServerController.Section.allCases) { section in
                    // Configuration holds Updates, so it wears the pending-update badge.
                    let updatePending = section == .configuration && updates.isUpdatePending
                    Label(section.title, systemImage: section.symbol)
                        .badge(updatePending ? 1 : 0)
                        .tag(section)
                        .accessibilityLabel(updatePending ? "\(section.title), update available" : section.title)
                }
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .top) {
            HStack(spacing: 10) {
                FairspokenMark(size: 28, monochrome: Crystal.ink)
                VStack(alignment: .leading, spacing: 0) {
                    Text("Fairspoken").font(.headline).foregroundStyle(Crystal.ink)
                    Text("Server").font(.subheadline).foregroundStyle(Crystal.serverAccent)
                }
                Spacer()
            }
            .padding(.horizontal, 18)
            .padding(.top, 6)
            .padding(.bottom, 10)
            .accessibilityElement(children: .combine)
        }
        .safeAreaInset(edge: .bottom) {
            UpdateSidebarButton(updates: updates, style: .server)
                .padding(.horizontal, 16)
                .padding(.bottom, 12)
        }
    }

    @ViewBuilder private var detail: some View {
        if let issue = controller.configIssue {
            ConfigIssueView(message: issue)
        } else {
            switch controller.section {
            case .activity: ActivityView()
            case .clients: ClientsView()
            case .models: ModelsView()
            case .connect: ConnectView()
            case .configuration: ConfigurationView()
            }
        }
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarSpacer(.flexible)
        ToolbarItem(placement: .primaryAction) {
            ServingChip()
        }
        ToolbarSpacer(.fixed)
        ToolbarItem(placement: .primaryAction) {
            Picker("Data", selection: Binding(get: { controller.source }, set: { controller.source = $0 })) {
                Text("Live").tag(ServerController.Source.live)
                Text("Demo").tag(ServerController.Source.demo)
            }
            .pickerStyle(.segmented)
            .frame(width: 130)
            .help("Live shows this server; Demo shows a simulated one")
        }
        ToolbarSpacer(.fixed)
        ToolbarItem(placement: .primaryAction) {
            let running = controller.runState.isRunning
            let disabled = controller.runState == .starting || controller.configIssue != nil
            // Starting is the window's one primary action; stopping is not.
            if running {
                Button("Stop", systemImage: "stop.fill") { Task { await controller.stop() } }
                    .labelStyle(.titleAndIcon)
                    .buttonStyle(.glass)
                    .disabled(disabled)
                    .help("Stop serving")
            } else {
                Button("Start serving", systemImage: "play.fill") { Task { await controller.start() } }
                    .labelStyle(.titleAndIcon)
                    .buttonStyle(.glassProminent)
                    .tint(Crystal.serverAccent)
                    .disabled(disabled)
            }
        }
    }
}

/// The toolbar's status: "Serving · Tailnet", "Starting…", "Not serving".
private struct ServingChip: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        let state = controller.runState
        HStack(spacing: 8) {
            StatusDot(color: color(state), pulsing: state == .starting)
            Text(title(state)).font(.callout.weight(.medium)).foregroundStyle(Crystal.ink)
            if state.isRunning {
                Text(controller.accessSummary).font(.callout).foregroundStyle(Crystal.ink2)
            }
        }
        .lineLimit(1)
        .padding(.horizontal, 12)
        .help(help(state))
        .accessibilityElement(children: .combine)
    }

    private func title(_ s: ServerController.RunState) -> String {
        switch s {
        case .running: "Serving"
        case .starting: "Starting…"
        case .stopped: "Stopped"
        case .failed: "Not serving"
        }
    }

    private func color(_ s: ServerController.RunState) -> Color {
        switch s {
        case .running: Crystal.ok
        case .starting: Crystal.warn
        case .stopped: Crystal.ink3
        case .failed: Crystal.error
        }
    }

    private func help(_ s: ServerController.RunState) -> String {
        if case .failed(let m) = s { return m }
        return controller.endpoints.map(\.url).joined(separator: "\n")
    }
}

/// The still Crystal backdrop with the server's garnet wash.
struct PageBackground: View {
    var body: some View {
        CrystalBackground(accent: Crystal.serverAccent)
    }
}

private struct ConfigIssueView: View {
    var message: String
    @Environment(ServerController.self) private var controller

    var body: some View {
        CrystalPage {
            PageHeader(title: "Configuration can't be read")
            SurfaceCard {
                VStack(alignment: .leading, spacing: 14) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .font(.fsData(.callout)).foregroundStyle(Crystal.error).textSelection(.enabled)
                    Text("Fix the file, or reset it. Resetting makes a new token, so clients pair again.")
                        .font(.callout).foregroundStyle(Crystal.ink2)
                    HStack {
                        Button("Show in Finder", systemImage: "folder") { NSWorkspace.shared.activateFileViewerSelecting([controller.configURL]) }
                            .buttonStyle(.bordered)
                        Button("Reset to defaults") { Task { await controller.resetConfiguration() } }
                            .buttonStyle(.borderedProminent).tint(Crystal.serverAccent)
                    }
                    .buttonBorderShape(.capsule)
                }
            }
            .frame(maxWidth: 720, alignment: .leading)
        }
    }
}

// MARK: - Shared pieces

/// A page: one `GlassEffectContainer` around every card, the standard margins and spacing.
struct CrystalPage<Content: View>: View {
    var scrolls = true
    @ViewBuilder var content: Content

    var body: some View {
        if scrolls {
            ScrollView { stack }
                .scrollEdgeEffectStyle(.soft, for: .top)
        } else {
            stack.frame(maxHeight: .infinity, alignment: .top)
        }
    }

    private var stack: some View {
        GlassEffectContainer(spacing: 6) {
            VStack(alignment: .leading, spacing: 16) { content }
                .padding(.horizontal, 28)
                .padding(.top, 18)
                .padding(.bottom, 28)
                .frame(maxWidth: .infinity, maxHeight: scrolls ? nil : .infinity, alignment: .topLeading)
        }
    }
}

/// A Liquid Glass card (radius 20). What sits inside is flat (`CrystalWell`), never glass.
struct SurfaceCard<Content: View>: View {
    var padding: CGFloat = 20
    var fillHeight = false
    @ViewBuilder var content: Content
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    var body: some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, maxHeight: fillHeight ? .infinity : nil, alignment: .topLeading)
            .glassEffect(reduceTransparency ? .identity : .regular, in: .rect(cornerRadius: 20))
            .background {
                if reduceTransparency { RoundedRectangle(cornerRadius: 20).fill(Crystal.pageSheen) }
            }
    }
}

/// Flat fill for a row, field or node inside a glass card (radius 12 by default).
struct CrystalWell: ViewModifier {
    var cornerRadius: CGFloat = 12
    var stroke: Color? = nil

    func body(content: Content) -> some View {
        content
            .background(Crystal.well, in: .rect(cornerRadius: cornerRadius))
            .overlay(RoundedRectangle(cornerRadius: cornerRadius).strokeBorder(stroke ?? Crystal.hairline, lineWidth: stroke == nil ? 1 : 1.5))
    }
}

extension View {
    func crystalWell(cornerRadius: CGFloat = 12, stroke: Color? = nil) -> some View {
        modifier(CrystalWell(cornerRadius: cornerRadius, stroke: stroke))
    }
}

/// Page title with at most one line of subtitle.
struct PageHeader<Trailing: View>: View {
    var title: String
    var subtitle: String? = nil
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(alignment: .center) {
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.system(size: 26, weight: .semibold)).tracking(-0.4).foregroundStyle(Crystal.ink)
                if let subtitle {
                    Text(subtitle).font(.body).foregroundStyle(Crystal.ink2).lineLimit(1)
                }
            }
            Spacer()
            trailing
        }
        .padding(.bottom, 4)
    }
}

extension PageHeader where Trailing == EmptyView {
    init(title: String, subtitle: String? = nil) {
        self.init(title: title, subtitle: subtitle) { EmptyView() }
    }
}

/// A small heading above a card.
struct SectionTitle: View {
    var text: String
    var body: some View {
        Text(text).font(.headline).foregroundStyle(Crystal.ink).padding(.top, 6)
    }
}

/// An error line: always with the triangle, so it never reads as the garnet accent.
struct ErrorLabel: View {
    var text: String
    var body: some View {
        Label(text, systemImage: "exclamationmark.triangle.fill")
            .font(.callout).foregroundStyle(Crystal.error)
    }
}

/// Notices raised by actions (a failed login-item change, a download refused).
struct NoticeBanner: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        if let notice = controller.notice {
            HStack(spacing: 10) {
                Image(systemName: "info.circle").foregroundStyle(Crystal.ink2)
                Text(notice).font(.callout).foregroundStyle(Crystal.ink)
                Spacer()
                Button("Dismiss") { controller.dismissNotice() }.buttonStyle(.borderless)
            }
            .padding(12)
            .crystalWell()
        }
    }
}

/// Copy with a moment of "copied" feedback; icon-only when `label` is nil.
struct CopyButton: View {
    var value: String
    var label: String? = nil
    @Environment(ServerController.self) private var controller
    @State private var copied = false

    var body: some View {
        Button {
            controller.copy(value)
            copied = true
            Task {
                try? await Task.sleep(for: .seconds(1.5))
                copied = false
            }
        } label: {
            if let label {
                Label(copied ? "Copied" : label, systemImage: copied ? "checkmark" : "doc.on.doc")
            } else {
                Image(systemName: copied ? "checkmark" : "doc.on.doc").frame(width: 16)
            }
        }
        .buttonStyle(.bordered)
        .buttonBorderShape(.capsule)
        .help("Copy")
        .accessibilityLabel(copied ? "Copied" : (label ?? "Copy"))
    }
}

enum ServerFormat {
    static func ms(_ value: Double?) -> String {
        guard let value else { return "–" }
        return value >= 1000 ? String(format: "%.1f s", value / 1000) : "\(Int(value.rounded())) ms"
    }

    static func duration(_ seconds: Double) -> String {
        let s = Int(seconds)
        if s >= 86_400 { return "\(s / 86_400) d \(s % 86_400 / 3600) h" }
        if s >= 3600 { return "\(s / 3600) h \(s % 3600 / 60) m" }
        if s >= 60 { return "\(s / 60) m \(s % 60) s" }
        return "\(s) s"
    }

    static func ago(_ epochMs: Double, now: Date = Date()) -> String {
        guard epochMs > 0 else { return "–" }
        let delta = now.timeIntervalSince1970 - epochMs / 1000
        if delta < 60 { return "just now" }
        if delta < 3600 { return "\(Int(delta / 60)) min ago" }
        if delta < 86_400 { return "\(Int(delta / 3600)) h ago" }
        return Date(timeIntervalSince1970: epochMs / 1000).formatted(.dateTime.day().month(.abbreviated))
    }

    static func clock(_ epochMs: Double) -> String {
        Date(timeIntervalSince1970: epochMs / 1000).formatted(.dateTime.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits).second(.twoDigits))
    }
}

extension UpdateStyle {
    /// The server's garnet hint for actions; warnings in the Crystal warning amber.
    static let server = UpdateStyle(accent: Crystal.serverAccent, warning: Crystal.warn)
}
