import FairspokenHost
import FairspokenUI
import MultiVoiceCore
import SwiftUI

/// Main window: Liquid Glass sidebar and toolbar (the control layer) over a limestone /
/// night page; content sits on opaque surfaces, per design/brand/README.md §8.
struct ServerWindowView: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        NavigationSplitView {
            sidebar.navigationSplitViewColumnWidth(min: 220, ideal: 240, max: 290)
        } detail: {
            ZStack {
                PageBackground().backgroundExtensionEffect()
                detail.transition(.opacity)
            }
            .animation(.smooth(duration: 0.25), value: controller.section)
            .toolbar { toolbar }
        }
        .frame(minWidth: 1120, minHeight: 760)
    }

    private var sidebar: some View {
        List(selection: Binding<ServerController.Section?>(get: { controller.section }, set: { if let s = $0 { controller.section = s } })) {
            Section {
                ForEach(ServerController.Section.allCases) { section in
                    Label(section.title, systemImage: section.symbol).tag(section)
                }
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .top) {
            HStack(spacing: 10) {
                FairspokenMark(size: 30)
                VStack(alignment: .leading, spacing: 0) {
                    Text("Fairspoken").font(.headline)
                    Text("Server").font(.subheadline).foregroundStyle(.secondary)
                }
                Spacer()
            }
            .padding(.horizontal, 18)
            .padding(.top, 6)
            .padding(.bottom, 10)
        }
        .safeAreaInset(edge: .bottom) {
            SidebarServerStatus().padding(12)
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
            .frame(width: 140)
            .help("Live shows this server; Demo shows a simulated practice server")
        }
        ToolbarSpacer(.fixed)
        ToolbarItem(placement: .primaryAction) {
            let running = controller.runState.isRunning
            Button {
                Task { running ? await controller.stop() : await controller.start() }
            } label: {
                Label(running ? "Stop serving" : "Start serving", systemImage: running ? "stop.fill" : "play.fill")
                    .labelStyle(.titleAndIcon)
            }
            .buttonStyle(.glassProminent)
            .tint(running ? .secondary : .fsBlue)
            .disabled(controller.runState == .starting || controller.configIssue != nil)
        }
    }
}

/// "Serving · 0.0.0.0:48173", the toolbar's status chip.
private struct ServingChip: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        let state = controller.runState
        HStack(spacing: 8) {
            StatusDot(color: state.isRunning ? .fsSuccess : state == .starting ? .fsGorse : .fsError, pulsing: state == .starting)
            Text(state.isRunning ? "Serving · \(controller.configuration.bindAddr)" : state == .starting ? "Starting…" : "Not serving")
                .font(.callout.weight(.medium))
                .lineLimit(1)
        }
        .padding(.horizontal, 12)
        .accessibilityElement(children: .combine)
    }
}

/// Limestone (light) or Night (dark) with a faint Copybook Blue / Margin Red field for the
/// glass to refract. Static, so idle CPU stays at zero.
struct PageBackground: View {
    var body: some View {
        AuroraBackground(accent: .fsBlue, secondary: .fsRed, base: .fsPage)
            .opacity(0.9)
    }
}

/// Serving state at the foot of the sidebar.
private struct SidebarServerStatus: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        let state = controller.runState
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                StatusDot(color: dotColor(state), pulsing: state == .starting)
                Text(title(state)).font(.callout.weight(.semibold)).lineLimit(1)
                Spacer(minLength: 0)
            }
            Text(detail(state)).font(.fsData(.caption)).foregroundStyle(.secondary).lineLimit(2)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.primary.opacity(0.05), in: .rect(cornerRadius: 14))
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

    private func detail(_ s: ServerController.RunState) -> String {
        switch s {
        case .running: controller.configuration.bindAddr
        case .starting: "Binding \(controller.configuration.bindAddr)"
        case .stopped: "Clients can't connect"
        case .failed(let m): m
        }
    }

    private func dotColor(_ s: ServerController.RunState) -> Color {
        switch s {
        case .running: .fsSuccess
        case .starting: .fsGorse
        case .stopped: .secondary
        case .failed: .fsError
        }
    }
}

private struct ConfigIssueView: View {
    var message: String
    @Environment(ServerController.self) private var controller

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label("The host configuration can't be read", systemImage: "exclamationmark.triangle.fill")
                .font(.title2.weight(.semibold)).foregroundStyle(Color.fsError)
            Text(message).font(.fsData(.callout)).textSelection(.enabled)
            Text("The server won't start with a configuration nobody chose. Fix the file, or reset it to the defaults (this makes a new token, so clients need the new one).")
                .foregroundStyle(.secondary)
            HStack {
                Button("Show the file in Finder") { NSWorkspace.shared.activateFileViewerSelecting([controller.configURL]) }
                    .buttonStyle(.glass)
                Button("Reset to defaults") { Task { await controller.resetConfiguration() } }
                    .buttonStyle(.glassProminent).tint(.fsBlue)
            }
        }
        .padding(40)
        .frame(maxWidth: 720, maxHeight: .infinity, alignment: .topLeading)
    }
}

// MARK: - Shared pieces

/// An opaque content card (cards, lists and charts are not glass, per the brand rules).
struct SurfaceCard<Content: View>: View {
    var padding: CGFloat = 18
    @ViewBuilder var content: Content
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .topLeading)
            .background(Color.fsSurface.opacity(scheme == .dark ? 0.86 : 0.92), in: .rect(cornerRadius: 18))
            .overlay(RoundedRectangle(cornerRadius: 18).strokeBorder(Color.primary.opacity(scheme == .dark ? 0.10 : 0.07)))
    }
}

struct PageHeader<Trailing: View>: View {
    var title: String
    var subtitle: String
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.system(size: 30, weight: .bold))
                Text(subtitle).font(.title3).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            trailing
        }
    }
}

extension PageHeader where Trailing == EmptyView {
    init(title: String, subtitle: String) {
        self.init(title: title, subtitle: subtitle) { EmptyView() }
    }
}

/// Notices raised by actions (a failed login-item change, a download refused).
struct NoticeBanner: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        if let notice = controller.notice {
            HStack(spacing: 10) {
                Image(systemName: "info.circle.fill").foregroundStyle(Color.fsBlue)
                Text(notice).font(.callout)
                Spacer()
                Button("Dismiss") { controller.dismissNotice() }.buttonStyle(.borderless)
            }
            .padding(12)
            .background(Color.fsBlue.opacity(0.10), in: .rect(cornerRadius: 12))
        }
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
