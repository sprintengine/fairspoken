import FairspokenSpeech
import FairspokenUI
import FairspokenUpdates
import KeyboardShortcuts
import FairspokenCore
import SwiftUI

struct SettingsView: View {
    enum Tab: String, CaseIterable, Identifiable {
        case general, shortcut, audio, transcription, vocabulary, updates, about
        var id: String { rawValue }
        var title: String { rawValue.capitalized }
        var symbol: String {
            switch self {
            case .general: "gearshape"
            case .shortcut: "command"
            case .audio: "mic"
            case .transcription: "waveform"
            case .vocabulary: "character.book.closed"
            case .updates: "arrow.triangle.2.circlepath"
            case .about: "info.circle"
            }
        }
    }

    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Namespace private var tabSelection

    var body: some View {
        @Bindable var app = model
        VStack(alignment: .leading, spacing: Layout.gap) {
            PageTitle("Settings")
                .padding(.horizontal, Layout.pageH)
                .padding(.top, Layout.pageTop)
            GlassEffectContainer {
                tabStrip(selection: $app.settingsTab)
            }
            .padding(.horizontal, Layout.pageH)
            Group {
                switch model.settingsTab {
                case .general: GeneralSettings()
                case .shortcut: ShortcutSettings()
                case .audio: AudioSettings()
                case .transcription: TranscriptionSettings()
                case .vocabulary: VocabularySettings()
                case .updates: Form { UpdateSettingsSection(updates: model.updates, style: .client) }
                case .about: AboutSettings()
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            .frame(maxWidth: 760, alignment: .leading)
            .padding(.horizontal, Layout.pageH - 20)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear { model.permissions.beginPolling() }
        .onDisappear { model.permissions.endPolling() }
    }

    /// One glass capsule; the selected tab is a flat accent wash that slides between tabs.
    private func tabStrip(selection: Binding<Tab>) -> some View {
        HStack(spacing: 2) {
            ForEach(Tab.allCases) { tab in
                let selected = selection.wrappedValue == tab
                let badged = tab == .updates && model.updates.isUpdatePending
                Button {
                    withAnimation(reduceMotion ? nil : .spring(response: 0.35, dampingFraction: 0.85)) { selection.wrappedValue = tab }
                } label: {
                    Label(tab.title, systemImage: tab.symbol)
                        .font(.callout.weight(selected ? .semibold : .regular))
                        .foregroundStyle(selected ? Crystal.ink : Crystal.ink2)
                        .padding(.horizontal, 13)
                        .padding(.vertical, 7)
                        .background {
                            if selected {
                                Capsule().fill(Crystal.clientAccent.opacity(0.14))
                                    .overlay(Capsule().strokeBorder(Crystal.clientAccent.opacity(0.22)))
                                    .matchedGeometryEffect(id: "selection", in: tabSelection)
                            }
                        }
                        .contentShape(.capsule)
                }
                .buttonStyle(.plain)
                .overlay(alignment: .topTrailing) {
                    if badged {
                        Circle().fill(Crystal.clientAccent).frame(width: 7, height: 7).offset(x: -6, y: 5).accessibilityHidden(true)
                    }
                }
                .accessibilityLabel(badged ? "\(tab.title), update available" : tab.title)
                .accessibilityAddTraits(selected ? .isSelected : [])
            }
        }
        .padding(4)
        .glassEffect(.regular, in: .capsule)
    }
}

// MARK: - Sections

private struct GeneralSettings: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = model.settings
        Form {
            Section("Dictation") {
                Toggle("Paste into the current app", isOn: binding(\.insertAtCursor))
                    .tint(Crystal.clientAccent)
                    .help("Sends ⌘V to the frontmost app. Needs Accessibility. Never pastes into password fields.")
                Toggle("Restore clipboard afterwards", isOn: binding(\.restoreClipboard))
                    .tint(Crystal.clientAccent)
                    .disabled(!store.settings.insertAtCursor)
                Toggle("Start and stop sounds", isOn: binding(\.interactionSounds))
                    .tint(Crystal.clientAccent)
            }
            Section("Permissions") {
                PermissionRow(kind: .microphone)
                PermissionRow(kind: .accessibility)
                PermissionRow(kind: .inputMonitoring)
                Button("Setup guide…") { model.openOnboarding?() }
            }
        }
    }

    private func binding(_ key: WritableKeyPath<AppSettings, Bool>) -> Binding<Bool> {
        Binding(get: { model.settings.settings[keyPath: key] }, set: { v in model.settings.update { $0[keyPath: key] = v } })
    }
}

private struct ShortcutSettings: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let s = model.settings.settings
        Form {
            Section {
                LabeledContent("Shortcut") {
                    KeyboardShortcuts.Recorder(for: .dictation) { _ in model.mirrorShortcut() }
                }
                Picker("Mode", selection: Binding(get: { s.recordingShortcutMode }, set: { v in model.settings.update { $0.recordingShortcutMode = v } })) {
                    Text("Press to start and stop").tag(RecordingShortcutMode.toggle)
                    Text("Hold to talk").tag(RecordingShortcutMode.pushToTalk)
                }
                .pickerStyle(.radioGroup)
                .tint(Crystal.clientAccent)
            } footer: {
                Text("Esc cancels a recording.").font(.caption).foregroundStyle(Crystal.ink3)
            }
            Section {
                Toggle("Hold fn (🌐) to dictate", isOn: Binding(get: { s.fnPushToTalk }, set: { v in
                    model.settings.update { $0.fnPushToTalk = v }
                    if v && !model.permissions.inputMonitoring.isGranted { model.permissions.request(.inputMonitoring) }
                    model.applyFnSetting()
                }))
                .tint(Crystal.clientAccent)
                if s.fnPushToTalk {
                    PermissionRow(kind: .inputMonitoring)
                    if Permissions.globeKeyAction != 0 {
                        HStack {
                            Label("🌐 is set to “\(Permissions.globeKeyActionName)”. Set it to Do Nothing.", systemImage: "exclamationmark.triangle")
                                .font(.callout).foregroundStyle(Crystal.warn)
                            Spacer()
                            Button("Keyboard Settings…") {
                                NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.Keyboard-Settings.extension")!)
                            }
                        }
                    }
                }
            }
        }
        .onChange(of: model.permissions.inputMonitoring) { _, granted in
            if granted.isGranted { model.applyFnSetting() }
        }
    }
}

private struct AudioSettings: View {
    @Environment(AppModel.self) private var model
    @State private var devices: [AudioCapture.InputDevice] = []
    @State private var testing = false

    var body: some View {
        let s = model.settings.settings
        Form {
            Section {
                Picker("Microphone", selection: Binding(get: { s.audioDevice }, set: { v in model.settings.update { $0.audioDevice = v } })) {
                    Text("System default\(AudioCapture.defaultInputName().map { " (\($0))" } ?? "")").tag("")
                    ForEach(devices) { Text($0.name).tag($0.name) }
                }
                LabeledContent("Level") {
                    HStack(spacing: 12) {
                        LevelBars(meter: model.dictation.meter, active: testing || model.dictation.phase.isListening, bars: 28,
                                  color: Crystal.clientAccent)
                            .frame(width: 200, height: 22)
                        Button(testing ? "Stop" : "Test") { toggleTest() }
                            .disabled(model.dictation.phase.isActive && !testing)
                    }
                }
                Stepper(value: Binding(get: { s.maxRecordingSeconds }, set: { v in model.settings.update { $0.maxRecordingSeconds = v } }),
                        in: 10...600, step: 10) {
                    LabeledContent("Longest recording", value: Format.duration(Double(s.maxRecordingSeconds)))
                }
            }
        }
        .onAppear { devices = AudioCapture.inputDevices() }
        .onDisappear { if testing { toggleTest() } }
    }

    private func toggleTest() {
        if testing {
            model.dictation.capture.stop()
            testing = false
        } else if model.permissions.microphone.isGranted {
            try? model.dictation.capture.start(deviceName: model.settings.settings.audioDevice, maxSeconds: 30, sink: nil)
            testing = true
        } else {
            model.permissions.request(.microphone)
        }
    }
}

// MARK: - Transcription

private struct TranscriptionSettings: View {
    @Environment(AppModel.self) private var model
    @State private var showManual = false

    var body: some View {
        let s = model.settings.settings
        let remote = s.transcriptionLocation == .remoteHost
        Form {
            Section {
                Picker("Transcribe on", selection: Binding(get: { remote ? TranscriptionLocation.remoteHost : .local },
                                                           set: { v in
                                                               model.settings.update { $0.transcriptionLocation = v }
                                                               if v == .local { model.models.prepare(model.settings.settings.model) }
                                                           })) {
                    Text("This Mac").tag(TranscriptionLocation.local)
                    Text("My host").tag(TranscriptionLocation.remoteHost)
                }
                .pickerStyle(.segmented)
                .tint(Crystal.clientAccent)
                if !remote {
                    Picker("Model", selection: Binding(get: { s.model }, set: { v in
                        model.settings.update { $0.model = v }
                        model.models.prepare(v)
                    })) {
                        ForEach(SpeechModelCatalog.all) { info in
                            Text(info.name + (FluidAudioEngine.isInstalled(info.id) ? "" : " (\(Format.bytes(info.approxBytes)) download)")).tag(info.id)
                        }
                    }
                    let state = model.models.engineState
                    LabeledContent("Status") {
                        HStack(spacing: 6) {
                            StatusDot(color: state.tint, pulsing: state.isBusy)
                            Text(state.label)
                        }
                    }
                }
            }
            if remote {
                HostListSection()
                ManualHostSection(expanded: $showManual)
            }
        }
        .onAppear { model.hostStatus.refresh() }
        // A host that takes a token opens manual entry with the token field waiting.
        .onChange(of: model.hostFinder.needsToken) { _, host in if host != nil { showManual = true } }
    }
}

/// My host: the one in use, then every host the tailnet scan found (this Mac's first). Pick one,
/// type its password if it has one, done.
private struct HostListSection: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let finder = model.hostFinder
        let s = model.settings.settings
        let searching = finder.scan == .searching
        let hosts = finder.hosts.filter(\.isThisMac) + finder.hosts.filter { !$0.isThisMac }
        Section {
            if !s.remoteUrl.isEmpty {
                CurrentHostRow(status: model.hostStatus)
            }
            ForEach(hosts) { host in
                DiscoveredHostRow(host: host,
                                  inUse: s.remoteUrl == host.url.absoluteString,
                                  pairing: finder.pairing?.id == host.id)
            }
            HStack(spacing: 10) {
                Button(searching ? "Searching…" : "Find hosts", systemImage: "magnifyingglass") { finder.findHosts() }
                    .disabled(searching)
                if searching { ProgressView().controlSize(.small) }
                Spacer()
                switch finder.scan {
                case .failed(let message):
                    Label(message, systemImage: "exclamationmark.triangle")
                        .font(.caption).foregroundStyle(Crystal.warn).lineLimit(2)
                case .done where finder.hosts.isEmpty:
                    Text("None found on your tailnet").font(.caption).foregroundStyle(Crystal.ink2)
                default:
                    EmptyView()
                }
            }
        } header: {
            Text("My host")
        }
        .onAppear {
            if finder.scan == .idle && AppInfo.screenshotDirectory == nil { finder.findHosts() }
        }
    }
}

/// The host in use: its address, whether it answers, and Test.
private struct CurrentHostRow: View {
    var status: HostStatus

    var body: some View {
        HStack(spacing: 12) {
            HostIcon(symbol: "link")
            VStack(alignment: .leading, spacing: 2) {
                Text(status.hostName).font(.fsData(.body, weight: .medium)).foregroundStyle(Crystal.ink)
                    .lineLimit(1).truncationMode(.middle).textSelection(.enabled)
                HStack(spacing: 6) {
                    StatusDot(color: status.tint, pulsing: status.state == .checking)
                    Text([status.stateLabel, status.detail].compactMap { $0 }.joined(separator: " · "))
                        .font(.caption).foregroundStyle(Crystal.ink2).lineLimit(2)
                }
            }
            Spacer()
            Button(status.state == .checking ? "Testing…" : "Test") { status.refresh(force: true) }
                .disabled(status.state == .checking)
        }
        .accessibilityElement(children: .combine)
    }
}

private struct HostIcon: View {
    var symbol: String
    var tint: Color = Crystal.ink2
    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: 15, weight: .medium))
            .foregroundStyle(tint)
            .frame(width: 30, height: 30)
            .well(cornerRadius: 8)
            .accessibilityHidden(true)
    }
}

/// One host that answered. Choosing a password host opens the password field in place.
private struct DiscoveredHostRow: View {
    var host: DiscoveredHost
    var inUse: Bool
    var pairing: Bool
    @Environment(AppModel.self) private var model
    @State private var password = ""
    @FocusState private var focused: Bool

    var body: some View {
        let finder = model.hostFinder
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                HostIcon(symbol: host.isThisMac ? "desktopcomputer" : "server.rack")
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(host.name).font(.body.weight(.medium)).foregroundStyle(Crystal.ink)
                        if host.auth == .password {
                            Image(systemName: "lock.fill").font(.caption2).foregroundStyle(Crystal.ink3)
                                .accessibilityLabel("Needs a password")
                        }
                    }
                    Text(host.isThisMac ? "This Mac" : host.machine)
                        .font(.caption).foregroundStyle(Crystal.ink2).lineLimit(1)
                }
                .help(host.url.absoluteString)
                Spacer()
                if inUse {
                    Label("In use", systemImage: "checkmark").font(.callout.weight(.medium)).foregroundStyle(Crystal.ok)
                } else if !pairing {
                    Button(host.auth == .password ? "Pair" : "Use") { finder.choose(host) }
                }
            }
            if pairing {
                HStack(spacing: 8) {
                    SecureField("Password", text: $password, prompt: Text("Password set on \(host.machine)"))
                        .textFieldStyle(.roundedBorder)
                        .labelsHidden()
                        .focused($focused)
                        .onSubmit(pair)
                    if finder.pairingBusy { ProgressView().controlSize(.small) }
                    Button("Cancel") { finder.cancelPairing() }
                        .keyboardShortcut(.cancelAction)
                    Button("Pair", action: pair)
                        .buttonStyle(.glassProminent)
                        .tint(Crystal.clientAccent)
                        .keyboardShortcut(.defaultAction)
                        .disabled(!canPair)
                }
                .padding(.leading, 42)
                if let error = finder.pairingError {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .font(.caption).foregroundStyle(Crystal.error)
                        .padding(.leading, 42)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .onChange(of: pairing, initial: true) { _, on in
            if on { password = ""; focused = true }
        }
    }

    /// Pairing passwords are 6–128 characters.
    private var canPair: Bool { (6...128).contains(password.count) && !model.hostFinder.pairingBusy }

    private func pair() {
        guard canPair else { return }
        model.hostFinder.pair(password: password)
    }
}

/// "Enter address manually": look a machine up by name, or type the URL and token.
private struct ManualHostSection: View {
    @Binding var expanded: Bool
    @Environment(AppModel.self) private var model
    @State private var machine = ""
    @State private var token = ""
    @FocusState private var tokenFocused: Bool

    var body: some View {
        let s = model.settings.settings
        let finder = model.hostFinder
        Section {
            DisclosureGroup("Enter address manually", isExpanded: $expanded) {
                LabeledContent("Machine name") {
                    HStack {
                        TextField("Machine name", text: $machine, prompt: Text("studio-mac or studio-mac:48173"))
                            .labelsHidden()
                            .onSubmit(lookUp)
                        Button(finder.lookingUp ? "Looking…" : "Look up", action: lookUp)
                            .disabled(finder.lookingUp || machine.trimmingCharacters(in: .whitespaces).isEmpty)
                    }
                }
                if let error = finder.lookupError {
                    Text(error).font(.caption).foregroundStyle(Crystal.error)
                }
                TextField("Address", text: Binding(get: { s.remoteUrl }, set: { v in model.settings.update { $0.remoteUrl = v } }),
                          prompt: Text("http://practice-mini.local:48173"))
                    .font(.fsData(.body))
                    .onSubmit { model.hostStatus.refresh(force: true) }
                SecureField("Token", text: $token, prompt: Text("From Fairspoken Server › Connect"))
                    .focused($tokenFocused)
                    .onSubmit { model.settings.remoteToken = token; model.hostStatus.refresh(force: true) }
                    .onChange(of: token) { _, v in model.settings.remoteToken = v }
                if let host = finder.needsToken, host.url.absoluteString == s.remoteUrl, token.isEmpty {
                    Label("\(host.name) needs its token", systemImage: "key")
                        .font(.caption).foregroundStyle(Crystal.warn)
                }
                Stepper(value: Binding(get: { s.remoteTimeoutSeconds }, set: { v in model.settings.update { $0.remoteTimeoutSeconds = v } }),
                        in: 5...300, step: 5) {
                    LabeledContent("Timeout", value: "\(s.remoteTimeoutSeconds) s")
                }
                Text("HTTPS is required outside this Mac, your network and your tailnet. Tokens are kept in your Keychain.")
                    .font(.caption).foregroundStyle(Crystal.ink3)
            }
        }
        .onAppear { token = model.settings.remoteToken }
        // Pairing writes the token; show it here too.
        .onChange(of: model.settings.remoteToken) { _, v in if token != v { token = v } }
        .onChange(of: finder.needsToken) { _, host in if host != nil { tokenFocused = true } }
    }

    private func lookUp() {
        let entry = machine.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !entry.isEmpty else { return }
        model.hostFinder.lookUp(entry)
    }
}

// MARK: - Vocabulary, About

private struct VocabularySettings: View {
    @Environment(AppModel.self) private var model
    @State private var newTerm = ""

    var body: some View {
        let hints = model.settings.settings.vocabularyHints
        Form {
            Section {
                HStack {
                    TextField("Add a term", text: $newTerm, prompt: Text("A name, drug or term"))
                        .labelsHidden()
                        .onSubmit(add)
                    Button("Add", action: add).disabled(newTerm.trimmingCharacters(in: .whitespaces).isEmpty)
                }
                ForEach(hints, id: \.self) { term in
                    HStack {
                        Text(term)
                        Spacer()
                        Button { remove(term) } label: { Image(systemName: "minus.circle.fill") }
                            .buttonStyle(.plain).foregroundStyle(Crystal.ink3)
                            .accessibilityLabel("Remove \(term)")
                    }
                }
            } header: {
                HStack {
                    Text("Vocabulary")
                    Spacer()
                    Text("\(hints.count) of 50").monospacedDigit()
                }
            } footer: {
                Text("Your spelling is applied to every dictation and sent to your host as hints.")
                    .font(.caption).foregroundStyle(Crystal.ink3)
            }
        }
    }

    private func add() {
        let term = newTerm.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !term.isEmpty else { return }
        model.settings.update { $0.vocabularyHints.append(term) }
        newTerm = ""
    }

    private func remove(_ term: String) {
        model.settings.update { $0.vocabularyHints.removeAll { $0 == term } }
    }
}

private struct AboutSettings: View {
    var body: some View {
        Form {
            Section {
                HStack(spacing: 14) {
                    BrandMark(size: 52)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(AppInfo.displayName).font(.title3.weight(.semibold))
                        Text("Version \(AppInfo.version) (\(AppInfo.build))\(AppInfo.isDevBuild ? " · development build" : "")")
                            .font(.callout).foregroundStyle(Crystal.ink2).textSelection(.enabled)
                    }
                }
                .padding(.vertical, 4)
            }
            Section {
                LabeledContent("Bundle identifier") { Text(AppInfo.bundleID).font(.fsData(.callout)).textSelection(.enabled) }
                LabeledContent("Speech engine", value: "FluidAudio 0.17.5 · CoreML")
                LabeledContent("Settings folder") {
                    Button(AppInfo.supportDirectory.path.replacingOccurrences(of: NSHomeDirectory(), with: "~")) {
                        NSWorkspace.shared.open(AppInfo.supportDirectory)
                    }
                    .buttonStyle(.link)
                    .tint(Crystal.clientAccent)
                }
            }
            Section("Privacy") {
                Text("Audio stays in memory and is never saved. Your last 50 dictations stay on this Mac. With your own host, audio goes to that host only.")
                    .font(.callout).foregroundStyle(Crystal.ink2)
            }
        }
    }
}

/// Live permission status with an action button.
struct PermissionRow: View {
    var kind: Permissions.Kind
    var optional = false
    @Environment(AppModel.self) private var model

    var body: some View {
        let status = model.permissions.status(kind)
        HStack(spacing: 12) {
            Image(systemName: info.symbol)
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(Crystal.ink2)
                .frame(width: 30, height: 30)
                .well(cornerRadius: 8)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 6) {
                    Text(info.title).font(.body.weight(.medium)).foregroundStyle(Crystal.ink)
                    if optional { Tag(text: "Optional", color: Crystal.ink3) }
                }
                Text(info.why).font(.caption).foregroundStyle(Crystal.ink2).fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
            if status.isGranted {
                Label("Allowed", systemImage: "checkmark").foregroundStyle(Crystal.ok).font(.callout.weight(.medium))
            } else {
                Button(status == .notDetermined ? "Allow" : "Open Settings") { model.permissions.request(kind) }
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var info: (title: String, why: String, symbol: String) {
        switch kind {
        case .microphone: ("Microphone", "Only while you dictate", "mic")
        case .accessibility: ("Accessibility", "Paste into the current app", "accessibility")
        case .inputMonitoring: ("Input Monitoring", "Hold fn to dictate", "keyboard")
        }
    }
}
