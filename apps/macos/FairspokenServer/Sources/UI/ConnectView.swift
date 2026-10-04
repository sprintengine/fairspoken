import MultiVoiceCore
import FairspokenHost
import CoreImage.CIFilterBuiltins
import FairspokenUI
import SwiftUI

/// The server address and token clients need, with copy buttons and a QR code.
struct ConnectView: View {
    @Environment(ServerController.self) private var controller
    @State private var showToken = false
    @State private var copied: String?

    var body: some View {
        let config = controller.configuration
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                PageHeader(title: "Connect", subtitle: "What to type into Fairspoken on each computer that should use this server")
                HStack(alignment: .top, spacing: 18) {
                    SurfaceCard(padding: 22) {
                        VStack(alignment: .leading, spacing: 16) {
                            Text("Server address for clients").font(.headline)
                            ForEach(controller.endpoints) { endpoint in
                                HStack(spacing: 12) {
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(endpoint.label).font(.caption).foregroundStyle(.secondary)
                                        Text(endpoint.url).font(.fsData(.title3, weight: .medium)).textSelection(.enabled)
                                    }
                                    Spacer()
                                    copyButton(endpoint.url, label: "Copy address")
                                }
                                Divider().opacity(0.6)
                            }
                            HStack(spacing: 12) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("Token").font(.caption).foregroundStyle(.secondary)
                                    if config.token.isEmpty {
                                        Text("None. Any client that can reach the address can use the server.")
                                            .font(.callout).foregroundStyle(Color.fsGorseText)
                                    } else {
                                        Text(showToken ? config.token : String(repeating: "•", count: min(32, config.token.count)))
                                            .font(.fsData(.title3, weight: .medium)).textSelection(.enabled)
                                    }
                                }
                                Spacer()
                                if !config.token.isEmpty {
                                    Button(showToken ? "Hide" : "Show") { showToken.toggle() }.buttonStyle(.glass)
                                    copyButton(config.token, label: "Copy token")
                                }
                            }
                            if !controller.runState.isRunning {
                                Label("The server is not running, so these addresses don't answer yet.", systemImage: "pause.circle")
                                    .font(.callout).foregroundStyle(Color.fsGorseText)
                            }
                        }
                    }
                    SurfaceCard(padding: 22) {
                        VStack(spacing: 12) {
                            if let url = controller.pairingURL, let image = QRCode.image(for: url) {
                                Image(nsImage: image)
                                    .interpolation(.none)
                                    .resizable()
                                    .frame(width: 196, height: 196)
                                    .padding(10)
                                    .background(.white, in: .rect(cornerRadius: 12))
                                    .accessibilityLabel("QR code for \(url)")
                            }
                            Text("Opens the web dashboard on a phone or tablet, signed in. Fairspoken for phones will use it to pair.")
                                .font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
                            if let url = controller.pairingURL, let open = URL(string: url) {
                                Button("Open the dashboard", systemImage: "safari") { NSWorkspace.shared.open(open) }
                                    .buttonStyle(.glass)
                                    .disabled(!controller.runState.isRunning)
                            }
                        }
                        .frame(maxWidth: .infinity)
                    }
                    .frame(width: 290)
                }
                SurfaceCard(padding: 22) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Setting up a client").font(.headline)
                        step(1, "On the Mac: open Fairspoken › Settings › Transcription, choose My transcription host, paste the address and the token, then Test connection.")
                        step(2, "On Windows or Linux: Fairspoken › Settings › Transcription › My host, with the same address as the Host URL and the same token.")
                        step(3, "Dictate as usual. Each dictation streams to this server while you speak; the text comes back about a tenth of a second after you let go.")
                    }
                }
                if controller.isLoopbackOnly {
                    SurfaceCard(padding: 22) {
                        VStack(alignment: .leading, spacing: 10) {
                            Label("Only this Mac can connect", systemImage: "lock").font(.headline)
                            Text("The server listens on 127.0.0.1. To serve other computers, either set the address to 0.0.0.0 (or this Mac's Tailscale address) under Configuration, or keep it private and publish it on your tailnet with Tailscale:")
                                .font(.callout)
                            HStack {
                                Text(tailscaleCommand).font(.fsData(.callout, weight: .medium)).textSelection(.enabled)
                                    .padding(.horizontal, 10).padding(.vertical, 6)
                                    .background(Color.primary.opacity(0.06), in: .rect(cornerRadius: 8))
                                copyButton(tailscaleCommand, label: "Copy command")
                            }
                            Text("Clients then use https://<this Mac's name>.<your tailnet>.ts.net. The server sees each device's tailnet address or login.")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .padding(.horizontal, 28)
            .padding(.vertical, 22)
        }
        .onAppear { controller.refreshAddresses() }
    }

    private var tailscaleCommand: String { "tailscale serve --bg \(controller.configuration.port)" }

    private func step(_ n: Int, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text("\(n)").font(.fsData(.callout, weight: .semibold)).foregroundStyle(Color.fsBlue).frame(width: 18)
            Text(text).font(.callout)
        }
    }

    private func copyButton(_ value: String, label: String) -> some View {
        Button(copied == value ? "Copied" : label, systemImage: copied == value ? "checkmark" : "doc.on.doc") {
            controller.copy(value)
            copied = value
            Task {
                try? await Task.sleep(for: .seconds(1.5))
                if copied == value { copied = nil }
            }
        }
        .buttonStyle(.glass)
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
