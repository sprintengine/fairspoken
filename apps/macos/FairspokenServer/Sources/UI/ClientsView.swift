import FairspokenHost
import FairspokenUI
import FairspokenCore
import SwiftUI

/// Every client the server has seen (up to 32, most recent first), recent pairing attempts
/// and the last 50 jobs.
struct ClientsView: View {
    @Environment(ServerController.self) private var controller

    struct ClientRow: Identifiable {
        var id: String
        var name: String
        var address: String
        var streaming: Bool
        var requests: Int
        var completed: Int
        var rejected: Int
        var failed: Int
        var audioMinutes: Double
        var lastSeenMs: Double
        var lastModel: String
    }

    struct JobRow: Identifiable {
        var id: String
        var at: Double
        var client: String
        var source: String
        var model: String
        var audio: Double
        var waitMs: Double
        var processingMs: Double
    }

    var body: some View {
        let stats = controller.stats
        let streaming = Set(controller.live.streams.values)
        let clients = stats.clients.map { c in
            ClientRow(id: c.address, name: controller.pairedName(for: c.address) ?? HostLiveState.displayName(forClient: c.address), address: c.address,
                      streaming: streaming.contains(c.address),
                      requests: c.requests, completed: c.completed, rejected: c.rejected, failed: c.failed,
                      audioMinutes: c.totalAudioSeconds / 60, lastSeenMs: c.lastSeenMs, lastModel: c.lastModel.map(HostLiveState.prettyModelName) ?? "–")
        }
        let jobs = stats.recent.map { r in
            JobRow(id: r.id, at: r.completedAtMs, client: HostLiveState.displayName(forClient: HostLiveState.key(r.client)), source: r.source,
                   model: HostLiveState.prettyModelName(r.model), audio: r.durationSeconds, waitMs: r.queueWaitMs, processingMs: r.processingMs)
        }
        CrystalPage(scrolls: false) {
            PageHeader(title: "Clients", subtitle: clients.isEmpty ? "No client has connected yet" :
                "\(clients.count) seen · \(streaming.count) speaking now")
            SurfaceCard(padding: 6) {
                Table(clients) {
                    TableColumn("Client") { c in
                        HStack(spacing: 8) {
                            Circle().fill(c.streaming ? Crystal.serverAccent : Crystal.hairline).frame(width: 7, height: 7)
                            Text(c.name).fontWeight(.medium).foregroundStyle(Crystal.ink)
                            if c.name != c.address { Text(c.address).font(.caption).foregroundStyle(Crystal.ink3).lineLimit(1) }
                        }
                    }
                    .width(min: 220, ideal: 320)
                    TableColumn("Requests") { c in Text("\(c.requests)").monospacedDigit() }.width(70)
                    TableColumn("Turned away") { c in Text("\(c.rejected)").monospacedDigit().foregroundStyle(c.rejected > 0 ? Crystal.warn : Crystal.ink3) }.width(90)
                    TableColumn("Failed") { c in failed(c.failed) }.width(60)
                    TableColumn("Audio") { c in Text(String(format: "%.1f min", c.audioMinutes)).monospacedDigit() }.width(80)
                    TableColumn("Last model") { c in Text(c.lastModel).foregroundStyle(Crystal.ink2) }.width(min: 110, ideal: 150)
                    TableColumn("Last seen") { c in Text(c.streaming ? "speaking now" : ServerFormat.ago(c.lastSeenMs)).foregroundStyle(c.streaming ? Crystal.serverAccent : Crystal.ink3) }
                        .width(min: 90, ideal: 110)
                }
                .scrollContentBackground(.hidden)
                .alternatingRowBackgrounds(.disabled)
                .frame(height: 250)
            }
            if !controller.pairingAttempts.isEmpty {
                SectionTitle(text: "Pairing")
                SurfaceCard(padding: 6) {
                    Table(controller.pairingAttempts) {
                        TableColumn("When") { a in Text(ServerFormat.clock(Double(a.atMs))).font(.fsData(.callout)).foregroundStyle(Crystal.ink3) }.width(80)
                        TableColumn("Device") { a in Text(a.clientName ?? "–").fontWeight(.medium) }.width(min: 140, ideal: 200)
                        TableColumn("Address") { a in Text(a.client ?? "unknown").foregroundStyle(Crystal.ink2) }.width(min: 120, ideal: 160)
                        TableColumn("Result") { a in Self.outcome(a.outcome) }.width(min: 140, ideal: 200)
                    }
                    .scrollContentBackground(.hidden)
                    .alternatingRowBackgrounds(.disabled)
                    .frame(height: 130)
                }
            }
            SectionTitle(text: "Recent jobs")
                .help("The server keeps timings only; transcripts aren't stored.")
            SurfaceCard(padding: 6) {
                Table(jobs) {
                    TableColumn("Finished") { j in Text(ServerFormat.clock(j.at)).font(.fsData(.callout)).foregroundStyle(Crystal.ink3) }.width(80)
                    TableColumn("Client") { j in Text(j.client).fontWeight(.medium) }.width(min: 140, ideal: 220)
                    TableColumn("Kind") { j in Text(j.source == "stream" ? "Streamed" : "Upload").foregroundStyle(Crystal.ink2) }.width(80)
                    TableColumn("Model") { j in Text(j.model).foregroundStyle(Crystal.ink2) }.width(min: 110, ideal: 160)
                    TableColumn("Audio") { j in Text(String(format: "%.1f s", j.audio)).monospacedDigit() }.width(70)
                    TableColumn("Queue wait") { j in Text(ServerFormat.ms(j.waitMs)).font(.fsData(.callout)).foregroundStyle(Crystal.ink2) }.width(90)
                    TableColumn("Release → text") { j in Text(ServerFormat.ms(j.source == "stream" ? j.processingMs : j.waitMs + j.processingMs)).font(.fsData(.callout, weight: .semibold)) }.width(110)
                }
                .scrollContentBackground(.hidden)
                .alternatingRowBackgrounds(.disabled)
                .frame(minHeight: 220, maxHeight: .infinity)
            }
            .frame(maxHeight: .infinity)
        }
    }

    @ViewBuilder private func failed(_ n: Int) -> some View {
        if n > 0 {
            Label("\(n)", systemImage: "exclamationmark.triangle.fill").monospacedDigit().foregroundStyle(Crystal.error)
        } else {
            Text("0").monospacedDigit().foregroundStyle(Crystal.ink3)
        }
    }

    @ViewBuilder private static func outcome(_ o: PairingAttempt.Outcome) -> some View {
        switch o {
        case .paired: Label("Paired", systemImage: "checkmark.circle.fill").foregroundStyle(Crystal.ok)
        case .open: Label("No token needed", systemImage: "lock.open").foregroundStyle(Crystal.ink2)
        case .wrongPassword: Label("Wrong password", systemImage: "exclamationmark.triangle.fill").foregroundStyle(Crystal.error)
        case .disabled: Label("Pairing off", systemImage: "circle.dashed").foregroundStyle(Crystal.ink2)
        case .limited: Label("Too many tries", systemImage: "hourglass").foregroundStyle(Crystal.warn)
        }
    }
}
