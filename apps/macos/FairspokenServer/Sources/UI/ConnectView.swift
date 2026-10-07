import FairspokenCore
import FairspokenHost
import CoreImage.CIFilterBuiltins
import FairspokenUI
import SwiftUI

/// How other devices connect, built around one choice: who can connect. Then the pairing
/// password, the addresses and the QR code. Other bind addresses and `tailscale serve` live
/// under Configuration › Advanced.
struct ConnectView: View {
    @Environment(ServerController.self) private var controller
    @State private var applying: ServerController.Access?
    @State private var problem: String?

    var body: some View {
        CrystalPage {
            PageHeader(title: "Connect", subtitle: "On your other devices, choose Find hosts and type the pairing password.")
            accessCard
            HStack(alignment: .top, spacing: 16) {
                SurfaceCard(fillHeight: true) {
                    VStack(alignment: .leading, spacing: 10) {
                        PairingPasswordRow()
                        ForEach(controller.endpoints) { AddressRow(endpoint: $0) }
                        if !controller.runState.isRunning {
                            Label("Not serving, so these addresses don't answer yet.", systemImage: "pause.circle")
                                .font(.callout).foregroundStyle(Crystal.warn)
                                .padding(.top, 2)
                        }
                    }
                }
                QRCard()
                    .frame(width: 220)
            }
            .fixedSize(horizontal: false, vertical: true)
        }
        .onAppear { controller.refreshAddresses() }
    }

    // MARK: Who can connect

    private var accessCard: some View {
        let access = controller.access
        let tailscale = controller.addresses.tailscale
        let port = controller.configuration.port
        return SurfaceCard {
            VStack(alignment: .leading, spacing: 14) {
                Text("Who can connect").font(.headline).foregroundStyle(Crystal.ink)
                HStack(spacing: 12) {
                    AccessOption(title: "This Mac only", detail: "Apps on this Mac", symbol: "desktopcomputer",
                                 selected: access == .thisMac, busy: applying == .thisMac) { choose(.thisMac) }
                    AccessOption(title: "This Mac and my tailnet",
                                 detail: tailscale.map { "Your devices on Tailscale · \($0.address)" } ?? "Tailscale isn't running on this Mac",
                                 symbol: "network",
                                 selected: access == .tailnet, busy: applying == .tailnet) { choose(.tailnet) }
                        .disabled(tailscale == nil && access != .tailnet)
                }
                .disabled(applying != nil)
                if access == .custom {
                    HStack(spacing: 8) {
                        Image(systemName: "slider.horizontal.3").foregroundStyle(Crystal.ink2)
                        Text("Custom: listening on \(controller.configuration.bindAddr)").font(.callout).foregroundStyle(Crystal.ink2)
                        Button("Advanced settings") { controller.section = .configuration }
                            .buttonStyle(.borderless).foregroundStyle(Crystal.serverAccent)
                    }
                } else if access == .tailnet && port != HostConfiguration.defaultPort {
                    Label("Find hosts looks on port \(String(HostConfiguration.defaultPort)); this server uses \(String(port)).", systemImage: "exclamationmark.triangle")
                        .font(.callout).foregroundStyle(Crystal.warn)
                }
                if let problem { ErrorLabel(text: problem) }
            }
        }
    }

    private func choose(_ next: ServerController.Access) {
        // Choosing the tailnet again re-binds if this Mac's Tailscale address changed.
        let stale = next == .tailnet && controller.configuration.bindAddress != controller.addresses.tailscale?.address
        guard next != controller.access || stale else { return }
        applying = next
        problem = nil
        Task {
            if case .failed(let m) = await controller.setAccess(next) { problem = m }
            else if case .failed(let m) = controller.runState { problem = m }
            applying = nil
        }
    }
}

/// One of the two "Who can connect" choices: a flat tile, outlined in the accent when chosen.
private struct AccessOption: View {
    var title: String
    var detail: String
    var symbol: String
    var selected: Bool
    var busy: Bool
    var action: () -> Void
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: symbol)
                    .font(.system(size: 18, weight: .medium))
                    .foregroundStyle(selected ? Crystal.serverAccent : Crystal.ink2)
                    .frame(width: 26)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.body.weight(.semibold)).foregroundStyle(enabled || selected ? Crystal.ink : Crystal.ink3)
                    if enabled || selected {
                        Text(detail).font(.callout).foregroundStyle(Crystal.ink2).lineLimit(1)
                    } else {
                        // The reason it's unavailable, at full strength so it reads.
                        Label(detail, systemImage: "info.circle").font(.callout).foregroundStyle(Crystal.ink2).lineLimit(1)
                    }
                }
                Spacer(minLength: 8)
                if busy {
                    ProgressView().controlSize(.small)
                } else {
                    Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                        .font(.system(size: 18))
                        .foregroundStyle(selected ? Crystal.serverAccent : Crystal.ink3)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(selected ? Crystal.serverAccent.opacity(0.07) : .clear, in: .rect(cornerRadius: 12))
            .crystalWell(stroke: selected ? Crystal.serverAccent.opacity(0.55) : nil)
            .contentShape(.rect(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// The pairing password: set, change or turn off inline, with show and copy.
private struct PairingPasswordRow: View {
    @Environment(ServerController.self) private var controller
    @State private var editing = false
    @State private var draft = ""
    @State private var reveal = false
    @State private var saving = false
    @State private var problem: String?
    @FocusState private var focused: Bool

    var body: some View {
        let password = controller.configuration.pairingPassword
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Text("Pairing password").font(.callout).foregroundStyle(Crystal.ink2).frame(width: 130, alignment: .leading)
                if editing {
                    TextField("Pairing password", text: $draft, prompt: Text("6 or more characters"))
                        .textFieldStyle(.plain)
                        .font(.fsData(.body, weight: .medium))
                        .focused($focused)
                        .onSubmit(save)
                    Spacer(minLength: 0)
                    if !password.isEmpty {
                        Button("Turn off") { submit("") }.buttonStyle(.borderless).foregroundStyle(Crystal.ink2)
                    }
                    Button("Cancel") { editing = false; problem = nil }.buttonStyle(.bordered)
                    Button(saving ? "Saving…" : "Save", action: save)
                        .buttonStyle(.borderedProminent).tint(Crystal.serverAccent)
                        .disabled(saving || HostConfiguration.pairingPasswordProblem(draft) != nil)
                        .keyboardShortcut(.defaultAction)
                } else if password.isEmpty {
                    Text("Not set, so devices can't pair").font(.body).foregroundStyle(Crystal.ink3)
                    Spacer(minLength: 0)
                    Button("Set password", action: startEditing)
                        .buttonStyle(.borderedProminent).tint(Crystal.serverAccent)
                } else {
                    Text(reveal ? password : String(repeating: "•", count: min(password.count, 16)))
                        .font(.fsData(.body, weight: .medium)).foregroundStyle(Crystal.ink)
                        .textSelection(.enabled)
                    Spacer(minLength: 0)
                    Button { reveal.toggle() } label: { Image(systemName: reveal ? "eye.slash" : "eye").frame(width: 16) }
                        .buttonStyle(.bordered)
                        .help(reveal ? "Hide" : "Show")
                        .accessibilityLabel(reveal ? "Hide password" : "Show password")
                    CopyButton(value: password)
                    Button("Change", action: startEditing).buttonStyle(.bordered)
                }
            }
            .buttonBorderShape(.capsule)
            if let problem { ErrorLabel(text: problem).font(.caption) }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .crystalWell()
    }

    private func startEditing() {
        let current = controller.configuration.pairingPassword
        // A fresh six-digit code to start from; easy to type on another device.
        draft = current.isEmpty ? String(format: "%06d", Int.random(in: 0..<1_000_000)) : current
        problem = nil
        editing = true
        focused = true
    }

    private func save() {
        guard HostConfiguration.pairingPasswordProblem(draft) == nil else {
            problem = ServerController.pairingPasswordHint
            return
        }
        submit(draft)
    }

    private func submit(_ value: String) {
        saving = true
        Task {
            let outcome = await controller.setPairingPassword(value)
            saving = false
            if case .failed(let m) = outcome { problem = m } else { editing = false; problem = nil; reveal = !value.isEmpty }
        }
    }
}

private struct AddressRow: View {
    var endpoint: ServerController.Endpoint

    var body: some View {
        HStack(spacing: 10) {
            Text(endpoint.label).font(.callout).foregroundStyle(Crystal.ink2).frame(width: 130, alignment: .leading)
            Text(endpoint.url).font(.fsData(.body, weight: .medium)).foregroundStyle(Crystal.ink)
                .textSelection(.enabled).lineLimit(1)
            Spacer(minLength: 0)
            CopyButton(value: endpoint.url)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .crystalWell()
    }
}

/// The dashboard, signed in, as a QR code for a phone, when another device can reach it.
private struct QRCard: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        SurfaceCard(fillHeight: true) {
            VStack(spacing: 10) {
                if let url = controller.pairingURL, let image = QRCode.image(for: url) {
                    Image(nsImage: image)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 124, height: 124)
                        .padding(8)
                        .background(.white, in: .rect(cornerRadius: 12))
                        .accessibilityLabel("QR code for the dashboard")
                    Text("Scan to open the dashboard").font(.caption).foregroundStyle(Crystal.ink2)
                } else {
                    Image(systemName: "qrcode")
                        .font(.system(size: 40, weight: .light))
                        .foregroundStyle(Crystal.ink3)
                        .frame(width: 140, height: 140)
                        .crystalWell()
                    Text("Needs the tailnet option").font(.caption).foregroundStyle(Crystal.ink2)
                        .multilineTextAlignment(.center)
                }
                if let url = controller.dashboardURL, let open = URL(string: url) {
                    Button("Open dashboard", systemImage: "safari") { NSWorkspace.shared.open(open) }
                        .buttonStyle(.bordered).buttonBorderShape(.capsule).controlSize(.small)
                        .disabled(!controller.runState.isRunning)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

enum QRCode {
    static func image(for text: String) -> NSImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 8, y: 8)) else { return nil }
        let rep = NSCIImageRep(ciImage: output)
        let image = NSImage(size: rep.size)
        image.addRepresentation(rep)
        return image
    }
}
