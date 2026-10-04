import FairspokenHost
import FairspokenUI
import MultiVoiceCore
import SwiftUI

/// Every client the server has seen (up to 32, most recent first) and the last 50 jobs.
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
            ClientRow(id: c.address, name: HostLiveState.displayName(forClient: c.address), address: c.address, streaming: streaming.contains(c.address),
                      requests: c.requests, completed: c.completed, rejected: c.rejected, failed: c.failed,
                      audioMinutes: c.totalAudioSeconds / 60, lastSeenMs: c.lastSeenMs, lastModel: c.lastModel.map(HostLiveState.prettyModelName) ?? "–")
        }
        let jobs = stats.recent.map { r in
            JobRow(id: r.id, at: r.completedAtMs, client: HostLiveState.displayName(forClient: HostLiveState.key(r.client)), source: r.source,
                   model: HostLiveState.prettyModelName(r.model), audio: r.durationSeconds, waitMs: r.queueWaitMs, processingMs: r.processingMs)
        }
        VStack(alignment: .leading, spacing: 18) {
            PageHeader(title: "Clients", subtitle: clients.isEmpty ? "No client has connected yet" :
                "\(clients.count) client\(clients.count == 1 ? "" : "s") seen · \(streaming.count) speaking now")
            SurfaceCard(padding: 0) {
                Table(clients) {
                    TableColumn("Client") { c in
                        HStack(spacing: 8) {
                            Circle().fill(c.streaming ? Color.fsRed : Color.primary.opacity(0.15)).frame(width: 8, height: 8)
                            VStack(alignment: .leading, spacing: 0) {
                                Text(c.name).fontWeight(.semibold)
                                if c.name != c.address { Text(c.address).font(.caption).foregroundStyle(.secondary) }
                            }
                        }
                    }
                    .width(min: 200, ideal: 260)
                    TableColumn("Requests") { c in Text("\(c.requests)").monospacedDigit() }.width(70)
                    TableColumn("Completed") { c in Text("\(c.completed)").monospacedDigit() }.width(80)
                    TableColumn("Turned away") { c in Text("\(c.rejected)").monospacedDigit().foregroundStyle(c.rejected > 0 ? Color.fsGorseText : .primary) }.width(90)
                    TableColumn("Failed") { c in Text("\(c.failed)").monospacedDigit().foregroundStyle(c.failed > 0 ? Color.fsError : .primary) }.width(60)
                    TableColumn("Audio") { c in Text(String(format: "%.1f min", c.audioMinutes)).monospacedDigit() }.width(80)
                    TableColumn("Last model") { c in Text(c.lastModel) }.width(min: 110, ideal: 150)
                    TableColumn("Last seen") { c in Text(c.streaming ? "speaking now" : ServerFormat.ago(c.lastSeenMs)).foregroundStyle(c.streaming ? Color.fsRedText : .secondary) }
                        .width(min: 90, ideal: 110)
                }
                .scrollContentBackground(.hidden)
                .frame(height: 280)
            }
            Text("Recent jobs").font(.title3.weight(.semibold))
            SurfaceCard(padding: 0) {
                Table(jobs) {
                    TableColumn("Finished") { j in Text(ServerFormat.clock(j.at)).font(.fsData(.callout)) }.width(80)
                    TableColumn("Client") { j in Text(j.client).fontWeight(.medium) }.width(min: 140, ideal: 200)
                    TableColumn("Kind") { j in Text(j.source == "stream" ? "Streamed" : "Upload") }.width(80)
                    TableColumn("Model") { j in Text(j.model) }.width(min: 110, ideal: 160)
                    TableColumn("Audio") { j in Text(String(format: "%.1f s", j.audio)).monospacedDigit() }.width(70)
                    TableColumn("Queue wait") { j in Text(ServerFormat.ms(j.waitMs)).font(.fsData(.callout)) }.width(90)
                    TableColumn("Release → text") { j in Text(ServerFormat.ms(j.source == "stream" ? j.processingMs : j.waitMs + j.processingMs)).font(.fsData(.callout, weight: .semibold)) }.width(110)
                }
                .scrollContentBackground(.hidden)
                .frame(minHeight: 240, maxHeight: .infinity)
            }
            Text("Clients behind `tailscale serve` show their tailnet address or login. The server keeps timings only; transcripts aren't stored.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .padding(.horizontal, 28)
        .padding(.vertical, 22)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}
