import FairspokenUI
import FairspokenCore
import FairspokenUpdates
import SwiftUI

/// Main window: Liquid Glass sidebar (system NavigationSplitView) over the Crystal backdrop,
/// which extends beneath it (`backgroundExtensionEffect`).
struct DashboardView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 210, ideal: 230, max: 280)
        } detail: {
            ZStack {
                CrystalBackground(accent: Crystal.clientAccent)
                    .backgroundExtensionEffect()
                detail
                    .transition(.opacity)
            }
            .animation(.smooth(duration: 0.25), value: model.section)
            .updateToast(model.updates, style: .client)
        }
        .frame(minWidth: 1080, minHeight: 740)
        .onAppear { if model.settings.settings.transcriptionLocation == .remoteHost { model.hostStatus.refresh() } }
    }

    private var sidebar: some View {
        List(selection: Binding<AppModel.Section?>(get: { model.section }, set: { if let s = $0 { model.section = s } })) {
            Section {
                ForEach(AppModel.Section.allCases) { section in
                    let updatePending = section == .settings && model.updates.isUpdatePending
                    Label(section.title, systemImage: section.symbol)
                        .badge(updatePending ? 1 : 0)
                        .listItemTint(section == model.section ? Crystal.clientAccent : Crystal.ink2)
                        .tag(section)
                        .accessibilityLabel(updatePending ? "\(section.title), update available" : section.title)
                }
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .top) {
            HStack(spacing: 10) {
                BrandMark(size: 28)
                Text(AppInfo.displayName).font(.headline).foregroundStyle(Crystal.ink)
                Spacer()
            }
            .padding(.horizontal, 18)
            .padding(.top, 6)
            .padding(.bottom, 8)
        }
        .safeAreaInset(edge: .bottom) {
            VStack(alignment: .leading, spacing: 12) {
                SidebarStatus()
                UpdateSidebarButton(updates: model.updates, style: .client)
                    .padding(.horizontal, 4)
            }
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

/// The one engine status in the window: what transcribes, whether it's ready, and the shortcut.
/// Sits on the glass sidebar, so it is a flat well, not more glass.
private struct SidebarStatus: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let state = model.models.engineState
        let remote = model.engineSummary.placement == .remoteHost
        let host = model.hostStatus
        Button {
            if remote { model.settingsTab = .transcription; model.section = .settings } else { model.section = .models }
        } label: {
            content(state: state, remote: remote, host: host)
        }
        .buttonStyle(.plain)
        .help(remote ? "Host settings" : "Models")
        .accessibilityHint(remote ? "Opens host settings" : "Opens Models")
    }

    private func content(state: ModelLibrary.EngineState, remote: Bool, host: HostStatus) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                StatusDot(color: remote ? host.tint : state.tint, pulsing: remote ? host.state == .checking : state.isBusy)
                VStack(alignment: .leading, spacing: 1) {
                    Text(remote ? "My host" : model.models.activeModel.shortName)
                        .font(.callout.weight(.semibold)).foregroundStyle(Crystal.ink).lineLimit(1)
                    Text(remote ? host.stateLabel : state.label)
                        .font(.caption).foregroundStyle(Crystal.ink2).lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            HStack(spacing: 4) {
                Text("Dictate").font(.caption).foregroundStyle(Crystal.ink2)
                Spacer(minLength: 0)
                ShortcutCaps(description: HotkeyController.currentShortcutSymbols).scaleEffect(0.85, anchor: .trailing)
            }
        }
        .padding(12)
        .well()
        .overlay(RoundedRectangle(cornerRadius: Layout.row).strokeBorder(Crystal.hairline))
        .contentShape(.rect(cornerRadius: Layout.row))
        .accessibilityElement(children: .combine)
    }
}

extension UpdateStyle {
    /// The client's Crystal green, with the Crystal warning colour.
    static let client = UpdateStyle(accent: Crystal.clientAccent, warning: Crystal.warn)
}
