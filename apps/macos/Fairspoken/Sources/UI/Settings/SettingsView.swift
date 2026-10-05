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
    @Namespace private var tabGlass

    var body: some View {
        @Bindable var app = model
        VStack(alignment: .leading, spacing: 16) {
            Text("Settings").font(.system(size: 30, weight: .bold, design: .rounded))
                .padding(.horizontal, 28)
                .padding(.top, 22)
            tabStrip(selection: $app.settingsTab)
                .padding(.horizontal, 28)
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
            .padding(.horizontal, 12)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear { model.permissions.beginPolling() }
        .onDisappear { model.permissions.endPolling() }
    }

    /// Glass tab strip; the selection lozenge morphs between tabs.
    private func tabStrip(selection: Binding<Tab>) -> some View {
        GlassEffectContainer(spacing: 4) {
            HStack(spacing: 4) {
                ForEach(Tab.allCases) { tab in
                    let selected = selection.wrappedValue == tab
                    let badged = tab == .updates && model.updates.isUpdatePending
                    Button {
                        withAnimation(.spring(response: 0.35, dampingFraction: 0.8)) { selection.wrappedValue = tab }
                    } label: {
                        Label(tab.title, systemImage: tab.symbol)
                            .font(.callout.weight(selected ? .semibold : .regular))
                            .padding(.horizontal, 14)
                            .padding(.vertical, 8)
                            .contentShape(.capsule)
                    }
                    .buttonStyle(.plain)
                    .overlay(alignment: .topTrailing) {
                        if badged {
                            Circle().fill(Color.mvTeal).frame(width: 7, height: 7).offset(x: -6, y: 5).accessibilityHidden(true)
                        }
                    }
                    .accessibilityLabel(badged ? "\(tab.title), update available" : tab.title)
                    .foregroundStyle(selected ? Color.primary : .secondary)
                    .glassEffect(selected ? .regular.tint(Color.mvTeal.opacity(0.22)).interactive() : .identity, in: .capsule)
                    .glassEffectID(tab, in: tabGlass)
                    .accessibilityAddTraits(selected ? .isSelected : [])
                }
            }
            .padding(4)
            .glassEffect(.regular, in: .capsule)
        }
    }
}

// MARK: - Sections

private struct GeneralSettings: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = model.settings
        Form {
            Section("Inserting text") {
                Toggle("Paste into the app you're using", isOn: binding(\.insertAtCursor))
                Text("Copies the transcript, then sends ⌘V to the frontmost app. Needs Accessibility. Password fields are never pasted into.")
                    .font(.caption).foregroundStyle(.secondary)
                Toggle("Restore my clipboard afterwards", isOn: binding(\.restoreClipboard))
                    .disabled(!store.settings.insertAtCursor)
            }
            Section("Feedback") {
                Toggle("Play start and stop sounds", isOn: binding(\.interactionSounds))
            }
            Section("Permissions") {
                PermissionRow(kind: .microphone)
                PermissionRow(kind: .accessibility)
                PermissionRow(kind: .inputMonitoring)
                Button("Show setup guide…") { model.openOnboarding?() }
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
            Section("Dictation shortcut") {
                LabeledContent("Shortcut") {
                    KeyboardShortcuts.Recorder(for: .dictation) { _ in model.mirrorShortcut() }
                }
                Picker("Mode", selection: Binding(get: { s.recordingShortcutMode }, set: { v in model.settings.update { $0.recordingShortcutMode = v } })) {
                    Text("Press to start, press again to stop").tag(RecordingShortcutMode.toggle)
                    Text("Hold to talk, release to insert").tag(RecordingShortcutMode.pushToTalk)
                }
                .pickerStyle(.radioGroup)
                Text("Press Esc while recording to cancel.").font(.caption).foregroundStyle(.secondary)
            }
            Section("Hold the Fn / Globe key") {
                Toggle("Hold fn to dictate", isOn: Binding(get: { s.fnPushToTalk }, set: { v in
                    model.settings.update { $0.fnPushToTalk = v }
                    if v && !model.permissions.inputMonitoring.isGranted { model.permissions.request(.inputMonitoring) }
                    model.applyFnSetting()
                }))
                if s.fnPushToTalk {
                    PermissionRow(kind: .inputMonitoring)
                    if Permissions.globeKeyAction != 0 {
                        Label("Your 🌐 key is set to “\(Permissions.globeKeyActionName)”. Set System Settings › Keyboard › “Press 🌐 key to” › Do Nothing so it only dictates.",
                              systemImage: "globe")
                            .font(.caption).foregroundStyle(Color.mvAmber)
                        Button("Open Keyboard Settings") {
                            NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.Keyboard-Settings.extension")!)
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
            Section("Microphone") {
                Picker("Input", selection: Binding(get: { s.audioDevice }, set: { v in model.settings.update { $0.audioDevice = v } })) {
                    Text("System default\(AudioCapture.defaultInputName().map { " (\($0))" } ?? "")").tag("")
                    ForEach(devices) { Text($0.name).tag($0.name) }
                }
                LabeledContent("Level") {
                    HStack(spacing: 12) {
                        LevelBars(meter: model.dictation.meter, active: testing || model.dictation.phase.isListening, bars: 28,
                                  color: .mvTeal)
                            .frame(width: 200, height: 22)
                        Button(testing ? "Stop" : "Test") { toggleTest() }
                            .buttonStyle(.glass)
                            .disabled(model.dictation.phase.isActive && !testing)
                    }
                }
            }
            Section("Recording") {
                Stepper(value: Binding(get: { s.maxRecordingSeconds }, set: { v in model.settings.update { $0.maxRecordingSeconds = v } }),
                        in: 10...600, step: 10) {
                    LabeledContent("Maximum length", value: Format.duration(Double(s.maxRecordingSeconds)))
                }
                Text("Recordings shorter than 0.35 s or with no speech are ignored.").font(.caption).foregroundStyle(.secondary)
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

private struct TranscriptionSettings: View {
    @Environment(AppModel.self) private var model
    @State private var token = ""

    var body: some View {
        let s = model.settings.settings
        Form {
            Section("Where speech is recognised") {
                Picker("Engine", selection: Binding(get: { s.transcriptionLocation == .remoteHost ? TranscriptionLocation.remoteHost : .local },
                                                     set: { v in
                                                         model.settings.update { $0.transcriptionLocation = v }
                                                         if v == .local { model.models.prepare(model.settings.settings.model) }
                                                     })) {
                    Text("Neural Engine on this Mac").tag(TranscriptionLocation.local)
                    Text("My transcription host").tag(TranscriptionLocation.remoteHost)
                }
                .pickerStyle(.segmented)
                if s.transcriptionLocation == .local {
                    Picker("Model", selection: Binding(get: { s.model }, set: { v in
                        model.settings.update { $0.model = v }
                        model.models.prepare(v)
                    })) {
                        ForEach(SpeechModelCatalog.all) { info in
                            Text(info.name + (FluidAudioEngine.isInstalled(info.id) ? "" : " (downloads \(Format.bytes(info.approxBytes)))")).tag(info.id)
                        }
                    }
                    LabeledContent("Status", value: model.models.engineState.label)
                }
            }
            Section("My host") {
                TextField("Host URL", text: Binding(get: { s.remoteUrl }, set: { v in model.settings.update { $0.remoteUrl = v } }),
                          prompt: Text("http://practice-mini.local:48173"))
                    .onSubmit { model.hostStatus.refresh(force: true) }
                SecureField("Token", text: $token, prompt: Text("Bearer token"))
                    .onSubmit { model.settings.remoteToken = token; model.hostStatus.refresh(force: true) }
                    .onChange(of: token) { _, v in model.settings.remoteToken = v }
                Stepper(value: Binding(get: { s.remoteTimeoutSeconds }, set: { v in model.settings.update { $0.remoteTimeoutSeconds = v } }),
                        in: 5...300, step: 5) {
                    LabeledContent("Timeout", value: "\(s.remoteTimeoutSeconds) s")
                }
                HostStatusLine(status: model.hostStatus)
                HStack {
                    Button(model.hostStatus.state == .checking ? "Testing…" : "Test connection") { model.hostStatus.refresh(force: true) }
                        .buttonStyle(.glass)
                        .disabled(model.hostStatus.state == .checking || s.remoteUrl.isEmpty)
                }
                Text("HTTPS is required except for this Mac, your local network, Tailscale (100.64.0.0/10, *.ts.net) and .local names. The token is stored in your Keychain. Fairspoken Server shows its address and token under Connect.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .onAppear {
            token = model.settings.remoteToken
            model.hostStatus.refresh()
        }
    }
}

/// "Using host: practice-mini.local:48173 · connected", with the reason when it isn't.
struct HostStatusLine: View {
    var status: HostStatus

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            StatusDot(color: color, pulsing: status.state == .checking)
            VStack(alignment: .leading, spacing: 2) {
                Text(status.summary).font(.callout)
                if let detail = status.detail {
                    Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                }
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var color: Color {
        switch status.state {
        case .connected: .mvGreen
        case .checking: .mvAmber
        case .failed: .mvCoral
        case .notConfigured: .secondary
        }
    }
}

private struct VocabularySettings: View {
    @Environment(AppModel.self) private var model
    @State private var newTerm = ""

    var body: some View {
        let hints = model.settings.settings.vocabularyHints
        Form {
            Section {
                HStack {
                    TextField("Add a name, drug or term", text: $newTerm)
                        .onSubmit(add)
                    Button("Add", action: add).buttonStyle(.glass).disabled(newTerm.trimmingCharacters(in: .whitespaces).isEmpty)
                }
                if hints.isEmpty {
                    Text("No terms yet.").foregroundStyle(.secondary)
                } else {
                    ForEach(hints, id: \.self) { term in
                        HStack {
                            Text(term)
                            Spacer()
                            Button { remove(term) } label: { Image(systemName: "minus.circle.fill") }
                                .buttonStyle(.plain).foregroundStyle(.secondary)
                                .accessibilityLabel("Remove \(term)")
                        }
                    }
                }
            } header: {
                Text("Vocabulary (\(hints.count)/50)")
            } footer: {
                Text("Your spelling and capitalisation are applied to every dictation, and the list is sent to your host as hints. Neural Engine vocabulary boosting comes in a later update.")
                    .font(.caption).foregroundStyle(.secondary)
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
                HStack(spacing: 16) {
                    BrandMark(size: 56)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(AppInfo.displayName).font(.title2.weight(.semibold))
                        Text("Version \(AppInfo.version) (\(AppInfo.build))\(AppInfo.isDevBuild ? " · development build" : "")")
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(.vertical, 6)
            }
            Section("This build") {
                LabeledContent("Bundle identifier", value: AppInfo.bundleID)
                LabeledContent("Speech engine", value: "FluidAudio 0.17.5 · CoreML")
                LabeledContent("Settings folder") {
                    Button(AppInfo.supportDirectory.path.replacingOccurrences(of: NSHomeDirectory(), with: "~")) {
                        NSWorkspace.shared.open(AppInfo.supportDirectory)
                    }
                    .buttonStyle(.link)
                }
            }
            Section("Privacy") {
                Text("Audio is processed in memory and never written to disk. Recent dictations are kept on this Mac only (the last 50). When you use your own host, audio streams to that host and nowhere else.")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
    }
}

/// Live permission status with an action button.
struct PermissionRow: View {
    var kind: Permissions.Kind
    @Environment(AppModel.self) private var model

    var body: some View {
        let status = model.permissions.status(kind)
        HStack(spacing: 12) {
            Image(systemName: info.symbol)
                .font(.title3)
                .foregroundStyle(status.isGranted ? Color.mvGreen : Color.mvAmber)
                .frame(width: 30)
            VStack(alignment: .leading, spacing: 2) {
                Text(info.title).font(.body.weight(.medium))
                Text(info.why).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
            if status.isGranted {
                Label("Allowed", systemImage: "checkmark.circle.fill").foregroundStyle(Color.mvGreen).font(.callout.weight(.medium))
            } else {
                Button(status == .notDetermined ? "Allow…" : "Open Settings") { model.permissions.request(kind) }
                    .buttonStyle(.glassProminent)
                    .tint(.mvTeal)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var info: (title: String, why: String, symbol: String) {
        switch kind {
        case .microphone: ("Microphone", "Hear you while you dictate — only while the shortcut is active.", "mic.fill")
        case .accessibility: ("Accessibility", "Paste the text into the app you're using (sends ⌘V) and skip password fields.", "accessibility")
        case .inputMonitoring: ("Input Monitoring", "Notice when you hold the fn key. Only needed for hold-fn dictation.", "keyboard")
        }
    }
}
