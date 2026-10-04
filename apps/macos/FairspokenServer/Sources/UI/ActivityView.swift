import FairspokenHost
import Charts
import FairspokenUI
import MultiVoiceCore
import SwiftUI

/// Live view of the server: counters, the constellation and recent jobs.
struct ActivityView: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                PageHeader(title: "Activity", subtitle: subtitle)
                NoticeBanner()
                if controller.source == .live && !controller.runState.isRunning {
                    NotServingBanner()
                }
                MetricsRow(live: controller.live, stats: controller.stats)
                SurfaceCard(padding: 0) {
                    ZStack {
                        RuledBackground().clipShape(.rect(cornerRadius: 18))
                        HostGraphView(live: controller.live, animator: controller.animator, accelerated: controller.stats.useGpu || controller.source == .demo)
                            .padding(.horizontal, 20)
                            .padding(.vertical, 18)
                    }
                    .frame(height: 560)
                }
                HStack(alignment: .top, spacing: 16) {
                    LatencyCard(live: controller.live).frame(maxWidth: .infinity)
                    RecentJobsCard(records: controller.stats.recent).frame(width: 470)
                }
                .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.horizontal, 28)
            .padding(.vertical, 22)
        }
        .scrollEdgeEffectStyle(.soft, for: .top)
    }

    private var subtitle: String {
        if controller.source == .demo { return "Demo · a simulated practice server · nothing here is real traffic" }
        let s = controller.stats
        var parts = [controller.addresses.localHostName ?? "This Mac"]
        if !s.serverVersion.isEmpty { parts.append("v\(s.serverVersion)") }
        if controller.runState.isRunning {
            parts.append("up \(ServerFormat.duration(controller.live.estimatedUptime(now: Date().timeIntervalSince1970 * 1000)))")
            parts.append("\(s.workerCount) worker\(s.workerCount == 1 ? "" : "s")")
        }
        return parts.joined(separator: " · ")
    }
}

private struct NotServingBanner: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "pause.circle.fill").font(.title2).foregroundStyle(Color.fsGorseText)
            VStack(alignment: .leading, spacing: 2) {
                if case .failed(let message) = controller.runState {
                    Text("The server isn't running").font(.callout.weight(.semibold))
                    Text(message).font(.caption).foregroundStyle(.secondary)
                } else {
                    Text("The server is stopped").font(.callout.weight(.semibold))
                    Text("Clients can't connect until you start it. Switch to Demo to see what a busy server looks like.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
            Button("Start serving") { Task { await controller.start() } }.buttonStyle(.glass)
        }
        .padding(14)
        .background(Color.fsGorse.opacity(0.14), in: .rect(cornerRadius: 14))
    }
}

struct MetricsRow: View {
    var live: HostLiveState
    var stats: HostStats

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { ctx in
            let now = ctx.date.timeIntervalSince1970 * 1000
            let m = live.metrics(now: now)
            let longestWait = live.queue.map { now - $0.enqueuedAt }.max()
            HStack(spacing: 14) {
                MetricTile(title: "In flight", value: "\(m.running + m.queueDepth)", unit: "",
                           footnote: "\(m.activeStreams) streaming · \(m.running) of \(live.workers.count) workers busy")
                MetricTile(title: "Queue", value: "\(m.queueDepth)", unit: "/ \(max(live.queueCapacity, 0))",
                           footnote: longestWait.map { "longest wait \(ServerFormat.ms($0))" } ?? "nothing waiting")
                MetricTile(title: "Release → text, p50", value: m.latencyP50Ms.map { "\(Int($0.rounded()))" } ?? "–", unit: m.latencyP50Ms == nil ? "" : "ms",
                           footnote: "p95 \(ServerFormat.ms(m.latencyP95Ms))")
                MetricTile(title: "Throughput", value: String(format: "%.0f", m.jobsPerMinute), unit: "jobs/min",
                           footnote: String(format: "%.1f min of audio per min", m.audioSecondsPerMinute / 60))
                MetricTile(title: "Served", value: live.totalTranscriptions.formatted(), unit: "",
                           footnote: "\(stats.failedJobs) failed · \(stats.rejectedJobs) turned away")
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }
}

struct MetricTile: View {
    var title: String
    var value: String
    var unit: String
    var footnote: String

    var body: some View {
        SurfaceCard(padding: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.callout).foregroundStyle(.secondary).lineLimit(1)
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(value).font(.mvNumber(30, weight: .semibold)).contentTransition(.numericText())
                    if !unit.isEmpty { Text(unit).font(.callout).foregroundStyle(.secondary) }
                }
                Text(footnote).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

struct LatencyCard: View {
    var live: HostLiveState

    var body: some View {
        let points = Array(live.completions.filter { !$0.failed }.suffix(60).enumerated())
        let m = live.metrics(now: Date().timeIntervalSince1970 * 1000)
        SurfaceCard(padding: 20) {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("Release → text, last \(points.count) jobs").font(.headline)
                    Spacer()
                    HStack(spacing: 6) {
                        StatusDot(color: .fsSuccess)
                        Text("p50 \(ServerFormat.ms(m.latencyP50Ms)) · p95 \(ServerFormat.ms(m.latencyP95Ms))").font(.fsData(.caption))
                    }
                    .padding(.horizontal, 10).padding(.vertical, 5)
                    .background(Color.primary.opacity(0.05), in: .capsule)
                }
                if points.isEmpty {
                    Text("No finished jobs yet").font(.callout).foregroundStyle(.secondary).frame(maxWidth: .infinity, minHeight: 150)
                } else {
                    Chart(points, id: \.offset) { i, c in
                        AreaMark(x: .value("Job", i), y: .value("ms", c.latencyMs))
                            .foregroundStyle(LinearGradient(colors: [Color.fsBlue.opacity(0.22), Color.fsBlue.opacity(0.0)], startPoint: .top, endPoint: .bottom))
                            .interpolationMethod(.monotone)
                        LineMark(x: .value("Job", i), y: .value("ms", c.latencyMs))
                            .foregroundStyle(Color.fsBlue)
                            .lineStyle(StrokeStyle(lineWidth: 2))
                            .interpolationMethod(.monotone)
                    }
                    .chartXAxis(.hidden)
                    .chartYAxis {
                        AxisMarks(position: .leading) { v in
                            AxisGridLine().foregroundStyle(Color.fsRuling.opacity(0.6))
                            AxisValueLabel { if let ms = v.as(Double.self) { Text("\(Int(ms)) ms").font(.fsData(.caption2)) } }
                        }
                    }
                    .frame(height: 150)
                    .accessibilityLabel("Release to text for recent jobs")
                }
                Text("From the end of the upload to the transcript leaving the server, including any wait for a worker.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

struct RecentJobsCard: View {
    var records: [HostStats.Record]

    var body: some View {
        SurfaceCard(padding: 20) {
            VStack(alignment: .leading, spacing: 10) {
                Text("Recent jobs").font(.headline)
                if records.isEmpty {
                    Text("No jobs yet").font(.callout).foregroundStyle(.secondary).frame(maxWidth: .infinity, minHeight: 150)
                }
                ForEach(Array(records.prefix(6).enumerated()), id: \.offset) { _, r in
                    HStack(spacing: 10) {
                        Text(ServerFormat.clock(r.completedAtMs)).font(.fsData(.callout)).foregroundStyle(.secondary)
                        Text(HostLiveState.displayName(forClient: HostLiveState.key(r.client))).font(.callout.weight(.semibold)).lineLimit(1)
                        Text(String(format: "%.1f s", r.durationSeconds)).font(.callout).foregroundStyle(.secondary)
                        Spacer(minLength: 4)
                        Text(HostLiveState.prettyModelName(r.model)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        Text(ServerFormat.ms(r.source == "stream" ? r.processingMs : r.processingMs + r.queueWaitMs)).font(.fsData(.callout, weight: .semibold))
                            .frame(width: 64, alignment: .trailing)
                    }
                    Divider().opacity(0.5)
                }
                Spacer(minLength: 0)
                Label("The server keeps timings only. Transcripts go back to the client and aren't stored.", systemImage: "lock")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}
