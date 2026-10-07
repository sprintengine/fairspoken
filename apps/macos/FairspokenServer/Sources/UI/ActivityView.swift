import FairspokenHost
import Charts
import FairspokenUI
import FairspokenCore
import SwiftUI

/// Live view of the server: counters, the constellation and recent jobs.
struct ActivityView: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        CrystalPage {
            PageHeader(title: "Activity", subtitle: subtitle)
            NoticeBanner()
            if controller.source == .live && !controller.runState.isRunning {
                NotServingBanner()
            }
            MetricsRow(live: controller.live, stats: controller.stats)
            SurfaceCard(padding: 0) {
                HostGraphView(live: controller.live, animator: controller.animator, accelerated: controller.stats.useGpu || controller.source == .demo)
                    .padding(.horizontal, 22)
                    .padding(.vertical, 18)
                    .frame(height: 540)
            }
            HStack(alignment: .top, spacing: 16) {
                LatencyCard(live: controller.live).frame(maxWidth: .infinity)
                RecentJobsCard(records: controller.stats.recent).frame(width: 460)
            }
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var subtitle: String {
        if controller.source == .demo { return "Demo data, simulated traffic" }
        let s = controller.stats
        var parts = [controller.hostName]
        if !s.serverVersion.isEmpty { parts.append("v\(s.serverVersion)") }
        if controller.runState.isRunning {
            parts.append("up \(ServerFormat.duration(controller.live.estimatedUptime(now: Date().timeIntervalSince1970 * 1000)))")
        }
        return parts.joined(separator: " · ")
    }
}

private struct NotServingBanner: View {
    @Environment(ServerController.self) private var controller

    var body: some View {
        HStack(spacing: 12) {
            if case .failed(let message) = controller.runState {
                Image(systemName: "exclamationmark.triangle.fill").font(.title3).foregroundStyle(Crystal.error)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Not serving").font(.callout.weight(.semibold)).foregroundStyle(Crystal.ink)
                    Text(message).font(.caption).foregroundStyle(Crystal.ink2).lineLimit(2)
                }
            } else {
                Image(systemName: "pause.circle.fill").font(.title3).foregroundStyle(Crystal.warn)
                Text("Stopped. Clients can't connect.").font(.callout.weight(.semibold)).foregroundStyle(Crystal.ink)
            }
            Spacer()
        }
        .padding(14)
        .crystalWell()
    }
}

/// The five counters, in one glass strip.
struct MetricsRow: View {
    var live: HostLiveState
    var stats: HostStats

    var body: some View {
        SurfaceCard(padding: 0) {
            TimelineView(.periodic(from: .now, by: 1)) { ctx in
                let now = ctx.date.timeIntervalSince1970 * 1000
                let m = live.metrics(now: now)
                let longestWait = live.queue.map { now - $0.enqueuedAt }.max()
                HStack(spacing: 0) {
                    MetricTile(title: "In flight", value: "\(m.running + m.queueDepth)", unit: "",
                               footnote: "\(m.running) of \(live.workers.count) workers busy")
                    divider
                    MetricTile(title: "Queue", value: "\(m.queueDepth)", unit: "/ \(max(live.queueCapacity, 0))",
                               footnote: longestWait.map { "longest \(ServerFormat.ms($0))" } ?? "nothing waiting")
                    divider
                    MetricTile(title: "Release → text", value: m.latencyP50Ms.map { "\(Int($0.rounded()))" } ?? "–", unit: m.latencyP50Ms == nil ? "" : "ms",
                               footnote: "p50 · p95 \(ServerFormat.ms(m.latencyP95Ms))")
                    divider
                    MetricTile(title: "Throughput", value: String(format: "%.0f", m.jobsPerMinute), unit: "jobs/min",
                               footnote: String(format: "%.1f min audio/min", m.audioSecondsPerMinute / 60))
                    divider
                    MetricTile(title: "Served", value: live.totalTranscriptions.formatted(), unit: "",
                               footnote: "\(stats.failedJobs) failed · \(stats.rejectedJobs) turned away")
                }
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private var divider: some View {
        Rectangle().fill(Crystal.hairline).frame(width: 1).padding(.vertical, 16)
    }
}

struct MetricTile: View {
    var title: String
    var value: String
    var unit: String
    var footnote: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.callout).foregroundStyle(Crystal.ink2).lineLimit(1)
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Text(value).font(.mvNumber(28, weight: .semibold)).foregroundStyle(Crystal.ink).contentTransition(.numericText())
                if !unit.isEmpty { Text(unit).font(.callout).foregroundStyle(Crystal.ink3) }
            }
            Text(footnote).font(.caption).foregroundStyle(Crystal.ink3).lineLimit(1)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

/// Release → text over the last jobs: a graphite line, with the median as the one accent.
struct LatencyCard: View {
    var live: HostLiveState

    var body: some View {
        let points = Array(live.completions.filter { !$0.failed }.suffix(60).enumerated())
        let m = live.metrics(now: Date().timeIntervalSince1970 * 1000)
        SurfaceCard {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .firstTextBaseline) {
                    Text("Release → text").font(.headline).foregroundStyle(Crystal.ink)
                    Text("last \(points.count) jobs").font(.callout).foregroundStyle(Crystal.ink3)
                    Spacer()
                    HStack(spacing: 10) {
                        legend(Crystal.serverAccent, "p50 \(ServerFormat.ms(m.latencyP50Ms))")
                        Text("p95 \(ServerFormat.ms(m.latencyP95Ms))").foregroundStyle(Crystal.ink2)
                    }
                    .font(.fsData(.caption))
                }
                if points.isEmpty {
                    Text("No finished jobs yet").font(.callout).foregroundStyle(Crystal.ink3).frame(maxWidth: .infinity, minHeight: 150)
                } else {
                    Chart {
                        ForEach(points, id: \.offset) { i, c in
                            AreaMark(x: .value("Job", i), y: .value("ms", c.latencyMs))
                                .foregroundStyle(LinearGradient(colors: [Crystal.ink2.opacity(0.16), Crystal.ink2.opacity(0)], startPoint: .top, endPoint: .bottom))
                                .interpolationMethod(.monotone)
                            LineMark(x: .value("Job", i), y: .value("ms", c.latencyMs))
                                .foregroundStyle(Crystal.ink2)
                                .lineStyle(StrokeStyle(lineWidth: 1.6))
                                .interpolationMethod(.monotone)
                        }
                        if let p50 = m.latencyP50Ms {
                            RuleMark(y: .value("p50", p50))
                                .foregroundStyle(Crystal.serverAccent)
                                .lineStyle(StrokeStyle(lineWidth: 1.4, dash: [4, 4]))
                        }
                    }
                    .chartXAxis(.hidden)
                    .chartYAxis {
                        AxisMarks(position: .leading, values: .automatic(desiredCount: 4)) { v in
                            AxisGridLine().foregroundStyle(Crystal.hairline)
                            AxisValueLabel { if let ms = v.as(Double.self) { Text("\(Int(ms)) ms").font(.fsData(.caption2)).foregroundStyle(Crystal.ink3) } }
                        }
                    }
                    .frame(height: 150)
                    .accessibilityLabel("Release to text for recent jobs")
                    .help("From the end of the upload to the transcript leaving the server, including any wait for a worker.")
                }
            }
        }
    }

    private func legend(_ color: Color, _ text: String) -> some View {
        HStack(spacing: 5) {
            Capsule().fill(color).frame(width: 12, height: 2)
            Text(text).foregroundStyle(Crystal.ink)
        }
    }
}

struct RecentJobsCard: View {
    var records: [HostStats.Record]

    var body: some View {
        SurfaceCard {
            VStack(alignment: .leading, spacing: 8) {
                Text("Recent jobs").font(.headline).foregroundStyle(Crystal.ink)
                    .help("The server keeps timings only. Transcripts go back to the client and aren't stored.")
                if records.isEmpty {
                    Text("No jobs yet").font(.callout).foregroundStyle(Crystal.ink3).frame(maxWidth: .infinity, minHeight: 150)
                }
                ForEach(Array(records.prefix(6).enumerated()), id: \.offset) { i, r in
                    if i > 0 { Rectangle().fill(Crystal.hairline).frame(height: 1) }
                    HStack(spacing: 10) {
                        Text(ServerFormat.clock(r.completedAtMs)).font(.fsData(.callout)).foregroundStyle(Crystal.ink3)
                        Text(HostLiveState.displayName(forClient: HostLiveState.key(r.client))).font(.callout.weight(.medium))
                            .foregroundStyle(Crystal.ink).lineLimit(1)
                        Spacer(minLength: 4)
                        Text(HostLiveState.prettyModelName(r.model)).font(.caption).foregroundStyle(Crystal.ink3).lineLimit(1)
                        Text(ServerFormat.ms(r.source == "stream" ? r.processingMs : r.processingMs + r.queueWaitMs))
                            .font(.fsData(.callout, weight: .semibold)).foregroundStyle(Crystal.ink)
                            .frame(width: 64, alignment: .trailing)
                    }
                    .padding(.vertical, 2)
                }
            }
        }
    }
}
