import MultiVoiceCore
import FairspokenHost
import FairspokenUI
import SwiftUI

/// Edits host-config.json. Limits, the accelerator and model assignments apply at once
/// (the same path as `POST /v1/config`); address, port, token, workers and queue restart
/// the server.
struct ConfigurationView: View {
    @Environment(ServerController.self) private var controller
    @State private var draft = HostConfiguration()
    @State private var loaded = false
    @State private var showToken = false
    @State private var message: (text: String, isError: Bool)?
    @State private var saving = false

    enum AddressChoice: Hashable {
        case loopback, all, tailscale, custom
    }

    var body: some View {
        let saved = controller.configuration
        let restartNeeded = draft.bindAddress != saved.bindAddress || draft.port != saved.port || draft.token != saved.token
            || draft.workerCount != saved.workerCount || draft.queueCapacity != saved.queueCapacity
        VStack(alignment: .leading, spacing: 0) {
            PageHeader(title: "Configuration", subtitle: controller.configURL.path.replacingOccurrences(of: NSHomeDirectory(), with: "~")) {
                Button("Show in Finder", systemImage: "folder") { NSWorkspace.shared.activateFileViewerSelecting([controller.configURL]) }
                    .buttonStyle(.glass)
            }
            .padding(.horizontal, 28)
            .padding(.top, 22)
            Form {
                network
                workers
                limits
                thisMac
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            .frame(maxWidth: 860, alignment: .leading)
            .padding(.horizontal, 12)
            HStack(spacing: 12) {
                if let message {
                    Label(message.text, systemImage: message.isError ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                        .foregroundStyle(message.isError ? Color.fsError : Color.fsSuccess)
                        .font(.callout)
                } else if restartNeeded {
                    Label("Saving restarts the server. Connected clients retry on their next dictation.", systemImage: "arrow.clockwise")
                        .font(.callout).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Revert") { draft = saved; message = nil }
                    .buttonStyle(.glass)
                    .disabled(draft == saved)
                Button(saving ? "Saving…" : (restartNeeded ? "Save and restart" : "Save")) { save() }
                    .buttonStyle(.glassProminent).tint(.fsBlue)
                    .disabled(draft == saved || saving)
                    .keyboardShortcut("s", modifiers: .command)
            }
            .padding(.horizontal, 28)
            .padding(.vertical, 16)
            .frame(maxWidth: 884)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear {
            if !loaded { draft = controller.configuration; loaded = true }
            controller.refreshAddresses()
        }
        .onChange(of: controller.configuration) { _, next in
            // Edits made elsewhere (the web dashboard, the Models page) show up here.
            if !saving { draft.maxActiveStreams = next.maxActiveStreams; draft.maxRecordingSeconds = next.maxRecordingSeconds
                draft.useGpu = next.useGpu; if draft.workerCount == next.workerCount { draft.workerModels = next.workerModels } }
        }
    }

    // MARK: Sections

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

    private var network: some View {
        Section {
            Picker("Listen on", selection: addressChoice) {
                Text("This Mac only (127.0.0.1)").tag(AddressChoice.loopback)
                Text("Every network (0.0.0.0)").tag(AddressChoice.all)
                if let t = controller.addresses.tailscale { Text("Tailscale only (\(t.address))").tag(AddressChoice.tailscale) }
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
                            .labelsHidden().font(.fsData()).frame(minWidth: 300)
                    } else {
                        SecureField("Token", text: $draft.token, prompt: Text("none"))
                            .labelsHidden().frame(minWidth: 300)
                    }
                    Button(showToken ? "Hide" : "Show") { showToken.toggle() }
                    Button("New token") { draft.token = HostConfiguration.generateToken(); showToken = true }
                }
            }
            if draft.token.trimmingCharacters(in: .whitespaces).isEmpty {
                Label("Without a token, anything that can reach the address can use the server and change its settings.", systemImage: "exclamationmark.triangle")
                    .font(.caption).foregroundStyle(Color.fsGorseText)
            }
        } header: {
            Text("Network")
        } footer: {
            Text("Clients send the token as `Authorization: Bearer <token>`. Behind `tailscale serve`, keep 127.0.0.1: Tailscale forwards each device's address.")
                .font(.caption).foregroundStyle(.secondary)
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
                Picker("Worker \(i + 1) serves", selection: Binding(get: { draft.workerModels[i] }, set: { draft.workerModels[i] = $0 })) {
                    ForEach(controller.backend.catalog) { m in
                        Text(m.name + (controller.backend.isInstalled(m.id) ? "" : " (not installed)")).tag(m.id)
                    }
                }
            }
            Stepper(value: $draft.queueCapacity, in: HostConfiguration.queueCapacityRange) {
                LabeledContent("Queue", value: "\(draft.queueCapacity) waiting jobs")
            }
        } header: {
            Text("Workers and queue")
        } footer: {
            Text("Each worker transcribes one dictation at a time; workers on the same model share it in memory (about 0.45 GB for v3, 0.6 GB for Ultra). When every worker is busy and the queue is full, new requests get 429 and clients retry.")
                .font(.caption).foregroundStyle(.secondary)
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
            Toggle("Use the Neural Engine", isOn: $draft.useGpu)
            if !draft.useGpu {
                Text("Off runs the models on the CPU, several times slower. Only for testing.").font(.caption).foregroundStyle(Color.fsGorseText)
            }
        }
    }

    private var thisMac: some View {
        Section {
            Toggle("Start at login", isOn: Binding(get: { controller.loginItemEnabled }, set: { controller.setLoginItem($0) }))
            Toggle("Keep this Mac awake while serving", isOn: Binding(get: { controller.configuration.preventSleep }, set: {
                controller.setPreventSleep($0)
                draft.preventSleep = $0
            }))
        } header: {
            Text("This Mac")
        } footer: {
            Text("Start at login opens the app (menu bar only) when you log in. To serve with nobody logged in, run it headless from a LaunchAgent or LaunchDaemon: see apps/macos/README.md. Keeping awake stops idle sleep, not the display going dark.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private func save() {
        saving = true
        message = nil
        Task {
            let outcome = await controller.save(draft)
            saving = false
            switch outcome {
            case .saved: message = ("Saved.", false)
            case .restarted:
                if case .failed(let m) = controller.runState { message = (m, true) } else { message = ("Saved. The server restarted.", false) }
            case .failed(let m): message = (m, true)
            }
            draft = controller.configuration
        }
    }
}
