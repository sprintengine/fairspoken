import FairspokenUI
import MultiVoiceCore
import SwiftUI

/// Main window: Liquid Glass sidebar (system NavigationSplitView) over an aurora backdrop
/// that extends beneath it (`backgroundExtensionEffect`).
struct DashboardView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 210, ideal: 230, max: 280)
        } detail: {
            ZStack {
                AuroraBackground()
                    .backgroundExtensionEffect()
                detail
                    .transition(.opacity)
            }
            .animation(.smooth(duration: 0.25), value: model.section)
        }
        .frame(minWidth: 1080, minHeight: 740)
        .onAppear { if model.settings.settings.transcriptionLocation == .remoteHost { model.hostStatus.refresh() } }
    }

    private var sidebar: some View {
        List(selection: Binding<AppModel.Section?>(get: { model.section }, set: { if let s = $0 { model.section = s } })) {
            Section {
                ForEach(AppModel.Section.allCases) { section in
                    Label(section.title, systemImage: section.symbol)
                        .tag(section)
                        .accessibilityLabel(section.title)
                }
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .top) {
            HStack(spacing: 10) {
                BrandMark(size: 30)
                Text(AppInfo.displayName).font(.headline)
                Spacer()
            }
            .padding(.horizontal, 18)
            .padding(.top, 6)
            .padding(.bottom, 8)
        }
        .safeAreaInset(edge: .bottom) {
            SidebarStatus()
                .padding(12)
        }
    }

    @ViewBuilder private var detail: some View {
        switch model.section {
        case .home: HomeView()
        case .models: ModelsView()
        case .settings: SettingsView()
        }
    }
}

/// Compact engine + shortcut status at the foot of the sidebar.
private struct SidebarStatus: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let summary = model.engineSummary
        let state = model.models.engineState
        let remote = summary.placement == .remoteHost
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: summary.placement.symbol).foregroundStyle(summary.placement.tint)
                Text(remote ? "Your host" : model.models.activeModel.shortName).font(.callout.weight(.semibold)).lineLimit(1)
                Spacer(minLength: 0)
                StatusDot(color: remote ? .mvIndigo : state.isReady ? .mvGreen : .mvAmber, pulsing: state.isBusy)
            }
            Text(remote ? model.hostStatus.summary : state.isReady ? "Neural Engine · ready" : state.label)
                .font(.caption).foregroundStyle(.secondary).lineLimit(1)
            HStack(spacing: 4) {
                Text("Dictate").font(.caption).foregroundStyle(.secondary)
                Spacer(minLength: 0)
                ShortcutCaps(description: HotkeyController.currentShortcutSymbols).scaleEffect(0.85, anchor: .trailing)
            }
        }
        .padding(12)
        .glassEffect(.regular, in: .rect(cornerRadius: 16))
        .accessibilityElement(children: .combine)
    }
}
