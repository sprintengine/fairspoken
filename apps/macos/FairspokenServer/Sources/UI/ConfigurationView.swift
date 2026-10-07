import FairspokenCore
import FairspokenHost
import FairspokenUI
import FairspokenUpdates
import SwiftUI

/// Edits host-config.json. Limits, the accelerator, model assignments and the name apply at
/// once (the same path as `POST /v1/config`); address, port, token, workers and queue restart
/// the server. Who can connect and the pairing password live on Connect; any other address,
/// the port, the token and `tailscale serve` sit under Advanced.
struct ConfigurationView: View {
    @Environment(ServerController.self) private var controller
    @Environment(UpdateController.self) private var updates
    @State private var draft = HostConfiguration()
    @State private var loaded = false
    @State private var showToken = false
    @State private var showAdvanced = false
    @State private var message: (text: String, isError: Bool)?
    @State private var saving = false

    enum AddressChoice: Hashable {
        case loopback, tailscale, all, custom
    }

    var body: some View {
        let saved = controller.configuration
        let restartNeeded = draft.bindAddress != saved.bindAddress || draft.port != saved.port || draft.token != saved.token
            || draft.workerCount != saved.workerCount || draft.queueCapacity != saved.queueCapacity
            || (draft.pairingEnabled && draft.token.trimmingCharacters(in: .whitespaces).isEmpty)
        VStack(alignment: .leading, spacing: 0) {
            PageHeader(title: "Configuration")
                .padding(.horizontal, 28)
                .padding(.top, 18)
            ScrollViewReader { proxy in
                Form {
                    server
                    workers
                    limits
                    thisMac
                    // Applies at once, like This Mac; Save and Revert are for the host's file only.
                    UpdateSettingsSection(updates: updates, style: .server)
                    advanced
                }
                .formStyle(.grouped)
                .scrollContentBackground(.hidden)
                .frame(maxWidth: 820, alignment: .leading)
                .padding(.horizontal, 12)
                .task {
                    // Screenshot mode: show the Advanced rows.
                    guard controller.revealAdvanced else { return }
                    try? await Task.sleep(for: .milliseconds(300))
                    proxy.scrollTo("advanced-end", anchor: .bottom)
                }
            }
            HStack(spacing: 12) {
                if let message {
                    if message.isError {
                        ErrorLabel(text: message.text)
                    } else {
                        Label(message.text, systemImage: "checkmark.circle.fill").font(.callout).foregroundStyle(Crystal.ok)
                    }
                } else if restartNeeded {
                    Label("Saving restarts the server.", systemImage: "arrow.clockwise")
                        .font(.callout).foregroundStyle(Crystal.ink2)
                }
                Spacer()
                Button("Revert") { draft = saved; message = nil }
                    .buttonStyle(.glass)
                    .disabled(draft == saved)
                Button(saving ? "Saving…" : (restartNeeded ? "Save and restart" : "Save")) { save() }
                    .buttonStyle(.glassProminent).tint(Crystal.serverAccent)
                    .disabled(draft == saved || saving)
                    .keyboardShortcut("s", modifiers: .command)
            }
            .padding(.horizontal, 28)
            .padding(.vertical, 14)
            .frame(maxWidth: 844)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear {
            if !loaded { draft = controller.configuration; loaded = true }
            if controller.revealAdvanced { showAdvanced = true }
            controller.refreshAddresses()
        }
        .onChange(of: controller.configuration) { old, next in
            // Edits made elsewhere (Connect, the web dashboard, the Models page) show up here.
            guard !saving else { return }
            draft.maxActiveStreams = next.maxActiveStreams
            draft.maxRecordingSeconds = next.maxRecordingSeconds
            draft.useGpu = next.useGpu
            if draft.workerCount == next.workerCount { draft.workerModels = next.workerModels }
            if draft.pairingPassword == old.pairingPassword { draft.pairingPassword = next.pairingPassword }
            if draft.token == old.token { draft.token = next.token }
            if draft.bindAddress == old.bindAddress { draft.bindAddress = next.bindAddress }
            if draft.port == old.port { draft.port = next.port }
        }
    }

    // MARK: Sections

    private var server: some View {
        Section("Server") {
            LabeledContent("Name on your tailnet") {
                TextField("Name", text: $draft.displayName, prompt: Text(MachineName.current()))
                    .labelsHidden().multilineTextAlignment(.trailing).frame(width: 260)
            }
            LabeledContent("Who can connect") {
                HStack(spacing: 8) {
                    Text(accessLabel).foregroundStyle(Crystal.ink2)
                    Button("Change…") { controller.section = .connect }
                        .buttonStyle(.borderless).foregroundStyle(Crystal.serverAccent)
                }
            }
        }
    }

    private var accessLabel: String {
        switch controller.access {
        case .thisMac: "This Mac only"
        case .tailnet: "This Mac and my tailnet"
        case .custom: "Custom (\(controller.configuration.bindAddr))"
        }
    }

    private var workers: some View {
        Section {
            Stepper(value: Binding(get: { draft.workerCount }, set: { n in
                draft.workerCount = n
                if draft.workerModels.count < n {
                    draft.workerModels += Array(repeating: draft.workerModels.last ?? HostConfiguration.defaultModel, count: n - draft.workerModels.count)
                } else {
                    draft.workerModels = Array(draft.workerModels.prefix(n))
                }
            }), in: HostConfiguration.workerCountRange) {
                LabeledContent("Workers", value: "\(draft.workerCount)")
            }
            ForEach(0..<draft.workerModels.count, id: \.self) { i in
                Picker("Worker \(i + 1)", selection: Binding(get: { draft.workerModels[i] }, set: { draft.workerModels[i] = $0 })) {
                    ForEach(controller.backend.catalog) { m in
                        Text(m.name + (controller.backend.isInstalled(m.id) ? "" : " (not installed)")).tag(m.id)
                    }
                }
            }
        } header: {
            Text("Workers")
        } footer: {
            Text("Each worker transcribes one dictation at a time. Workers on the same model share it in memory.")
                .font(.caption).foregroundStyle(Crystal.ink2)
        }
    }

    private var limits: some View {
        Section("Limits") {
            Stepper(value: $draft.maxActiveStreams, in: HostConfiguration.maxActiveStreamsRange) {
                LabeledContent("Dictations at once", value: "\(draft.maxActiveStreams)")
            }
            Stepper(value: $draft.maxRecordingSeconds, in: HostConfiguration.maxRecordingSecondsRange, step: 10) {
                LabeledContent("Longest recording", value: ServerFormat.duration(Double(draft.maxRecordingSeconds)))
            }
            Stepper(value: $draft.queueCapacity, in: HostConfiguration.queueCapacityRange) {
                LabeledContent("Queue", value: "\(draft.queueCapacity) waiting")
            }
            .help("When every worker is busy and the queue is full, new requests get 429 and clients retry.")
        }
    }

    private var thisMac: some View {
        Section("This Mac") {
            Toggle("Start at login", isOn: Binding(get: { controller.loginItemEnabled }, set: { controller.setLoginItem($0) }))
                .help("Opens the app in the menu bar when you log in. To serve with nobody logged in, see apps/macos/README.md.")
            Toggle("Keep this Mac awake while serving", isOn: Binding(get: { controller.configuration.preventSleep }, set: {
                controller.setPreventSleep($0)
                draft.preventSleep = $0
            }))
        }
    }

    // MARK: Advanced

    private var addressChoice: Binding<AddressChoice> {
        Binding {
            switch draft.bindAddress {
            case "127.0.0.1", "::1": .loopback
            case "0.0.0.0", "::": .all
            case controller.addresses.tailscale?.address ?? "-": .tailscale
            default: .custom
            }
        } set: { choice in
            switch choice {
            case .loopback: draft.bindAddress = "127.0.0.1"
            case .all: draft.bindAddress = "0.0.0.0"
            case .tailscale: draft.bindAddress = controller.addresses.tailscale?.address ?? draft.bindAddress
            case .custom: if ["127.0.0.1", "0.0.0.0"].contains(draft.bindAddress) { draft.bindAddress = controller.addresses.lan.first?.address ?? "" }
            }
        }
    }

    private var tailscaleCommand: String { "tailscale serve --bg \(String(draft.port))" }

    private var advanced: some View {
        Section("Advanced", isExpanded: $showAdvanced) {
                Picker("Listen on", selection: addressChoice) {
                    Text("This Mac only (127.0.0.1)").tag(AddressChoice.loopback)
                    if let t = controller.addresses.tailscale { Text("This Mac and tailnet (\(t.address))").tag(AddressChoice.tailscale) }
                    Text("Every network (0.0.0.0)").tag(AddressChoice.all)
                    Text("One address…").tag(AddressChoice.custom)
                }
                if addressChoice.wrappedValue == .custom {
                    LabeledContent("Address") {
                        TextField("Address", text: $draft.bindAddress, prompt: Text("192.168.1.20"))
                            .labelsHidden().font(.fsData()).multilineTextAlignment(.trailing).frame(width: 200)
                    }
                }
                LabeledContent("Port") {
                    TextField("Port", value: $draft.port, format: .number.grouping(.never))
                        .labelsHidden().font(.fsData()).multilineTextAlignment(.trailing).frame(width: 90)
                }
                LabeledContent("Token") {
                    HStack {
                        if showToken {
                            TextField("Token", text: $draft.token, prompt: Text("none"))
                                .labelsHidden().font(.fsData()).frame(minWidth: 280)
                        } else {
                            SecureField("Token", text: $draft.token, prompt: Text("none"))
                                .labelsHidden().frame(minWidth: 280)
                        }
                        Button(showToken ? "Hide" : "Show") { showToken.toggle() }
                        Button("New") { draft.token = HostConfiguration.generateToken(); showToken = true }
                            .help("Make a new token. Every client then pairs again.")
                        CopyButton(value: draft.token).disabled(draft.token.isEmpty)
                    }
                }
                if draft.token.trimmingCharacters(in: .whitespaces).isEmpty {
                    Label("Without a token, anything that can reach the server can use it.", systemImage: "exclamationmark.triangle")
                        .font(.caption).foregroundStyle(Crystal.warn)
                }
                LabeledContent("Publish with Tailscale") {
                    HStack {
                        Text(tailscaleCommand).font(.fsData(.callout)).textSelection(.enabled)
                        CopyButton(value: tailscaleCommand)
                    }
                }
                .help("Keeps the server on 127.0.0.1 and gives it an HTTPS name on your tailnet.")
                Toggle("Use the Neural Engine", isOn: $draft.useGpu)
                if !draft.useGpu {
                    Label("The CPU is several times slower. Only for testing.", systemImage: "exclamationmark.triangle")
                        .font(.caption).foregroundStyle(Crystal.warn)
                }
                LabeledContent("Configuration file") {
                    Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([controller.configURL]) }
                }
                .help(controller.configURL.path)
                .id("advanced-end")
        }
    }

    private func save() {
        saving = true
        message = nil
        Task {
            let outcome = await controller.save(draft)
            saving = false
            switch outcome {
            case .saved: message = ("Saved", false)
            case .restarted:
                if case .failed(let m) = controller.runState { message = (m, true) } else { message = ("Saved and restarted", false) }
            case .failed(let m): message = (m, true)
            }
            draft = controller.configuration
        }
    }
}
