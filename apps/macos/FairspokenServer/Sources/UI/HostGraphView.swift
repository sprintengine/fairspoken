import FairspokenHost
import FairspokenUI
import MultiVoiceCore
import SwiftUI

/// Where every node of the constellation sits. Shared by the Canvas (edges, particles,
/// pulses) and the glass node views so both always agree.
struct GraphLayout {
    static let clientSize = CGSize(width: 190, height: 54)
    static let queueSize = CGSize(width: 156, height: 132)
    static let workerSize = CGSize(width: 212, height: 84)
    static let modelSize = CGSize(width: 236, height: 92)
    static let maxClients = 7

    let size: CGSize
    let clients: [HostLiveState.ClientNode]
    let hiddenClients: Int
    let workers: [HostLiveState.WorkerNode]
    let models: [HostLiveState.ModelNode]

    init(size: CGSize, state: HostLiveState) {
        self.size = size
        // Streaming clients first, then the most recently seen.
        let sorted = state.clients.sorted { ($0.activeStreams > 0 ? 0 : 1, -$0.lastSeenMs) < ($1.activeStreams > 0 ? 0 : 1, -$1.lastSeenMs) }
        let visible = Array(sorted.prefix(Self.maxClients))
        // Keep a stable vertical order among the visible ones.
        clients = state.clients.filter { c in visible.contains { $0.id == c.id } }
        hiddenClients = max(0, sorted.count - Self.maxClients)
        workers = state.workers
        models = state.models.sorted { ($0.installed ? 0 : 1, $0.assignedWorkers.isEmpty ? 1 : 0) < ($1.installed ? 0 : 1, $1.assignedWorkers.isEmpty ? 1 : 0) }
    }

    private let top: CGFloat = 52
    private var bottom: CGFloat { size.height - 20 }

    /// Column centres. The width left after the four node columns is shared between the three
    /// gaps (clients→queue widest, where the request strands fan in).
    func x(_ column: Int) -> CGFloat {
        let widths = [Self.clientSize.width, Self.queueSize.width, Self.workerSize.width, Self.modelSize.width]
        let spare = max(60, size.width - widths.reduce(0, +))
        let gaps = [spare * 0.44, spare * 0.30, spare * 0.26]
        var x: CGFloat = 0
        for i in 0..<column { x += widths[i] + gaps[i] }
        return x + widths[column] / 2
    }

    private func y(_ index: Int, of count: Int, nodeHeight: CGFloat) -> CGFloat {
        guard count > 1 else { return (top + bottom) / 2 }
        let span = bottom - top - nodeHeight
        let step = min(span / CGFloat(count - 1), nodeHeight + 26)
        let used = step * CGFloat(count - 1)
        return (top + bottom) / 2 - used / 2 + step * CGFloat(index)
    }

    func center(_ node: FlowAnimator.Node) -> CGPoint? {
        switch node {
        case .client(let id):
            guard let i = clients.firstIndex(where: { $0.id == id }) else { return CGPoint(x: x(0), y: bottom - 6) }
            return CGPoint(x: x(0), y: y(i, of: clients.count, nodeHeight: Self.clientSize.height - 4))
        case .queue:
            return CGPoint(x: x(1), y: (top + bottom) / 2)
        case .worker(let index):
            guard let i = workers.firstIndex(where: { $0.id == index }) else { return nil }
            return CGPoint(x: x(2), y: y(i, of: workers.count, nodeHeight: Self.workerSize.height))
        case .model(let id):
            guard let i = models.firstIndex(where: { $0.id == id }) else { return nil }
            return CGPoint(x: x(3), y: y(i, of: models.count, nodeHeight: Self.modelSize.height))
        }
    }

    func anchor(_ node: FlowAnimator.Node, outgoing: Bool) -> CGPoint? {
        guard let c = center(node) else { return nil }
        switch node {
        case .client:
            // Edges leave a client from the right of its label column.
            return CGPoint(x: c.x + Self.clientSize.width / 2, y: c.y)
        case .queue:
            return CGPoint(x: c.x + (outgoing ? 1 : -1) * Self.queueSize.width / 2, y: c.y)
        case .worker:
            return CGPoint(x: c.x + (outgoing ? 1 : -1) * Self.workerSize.width / 2, y: c.y)
        case .model:
            // Results travel back from the model to the client: they leave on its left.
            return CGPoint(x: c.x - Self.modelSize.width / 2, y: c.y)
        }
    }

    func curve(from a: FlowAnimator.Node, to b: FlowAnimator.Node) -> (CGPoint, CGPoint, CGPoint, CGPoint)? {
        guard let p0 = anchor(a, outgoing: true), let p3 = anchor(b, outgoing: false) else { return nil }
        if case .model = a, case .client = b {
            // The return path arcs under the graph so it doesn't cross the request edges.
            let dip = min(size.height + 10, max(p0.y, p3.y) + 120)
            return (p0, CGPoint(x: p0.x - 90, y: dip), CGPoint(x: p3.x + 200, y: dip), p3)
        }
        let dx = max(40, (p3.x - p0.x) * 0.5)
        return (p0, CGPoint(x: p0.x + dx, y: p0.y), CGPoint(x: p3.x - dx, y: p3.y), p3)
    }
}

/// The live constellation: clients → queue → workers → models as glass nodes over a Canvas
/// of edges and comets (requests in Margin Red, finished transcripts back in Copybook Blue).
/// The TimelineView only ticks while something moves.
struct HostGraphView: View {
    var live: HostLiveState
    var animator: FlowAnimator
    var accelerated: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        GeometryReader { geo in
            let layout = GraphLayout(size: geo.size, state: live)
            let moving = live.workers.contains(where: \.isBusy) || !live.streams.isEmpty || animator.isAnimating()
            ZStack(alignment: .topLeading) {
                TimelineView(.animation(minimumInterval: 1 / 60, paused: !moving)) { ctx in
                    Canvas { gc, _ in
                        draw(in: &gc, layout: layout, time: FlowAnimator.now(), wall: ctx.date)
                    }
                }
                .accessibilityHidden(true)

                columnTitles(layout)

                GlassEffectContainer(spacing: 8) {
                    ZStack(alignment: .topLeading) {
                        ForEach(layout.clients) { client in
                            ClientNodeView(client: client)
                                .position(layout.center(.client(client.id)) ?? .zero)
                        }
                        if layout.hiddenClients > 0 {
                            Text("+\(layout.hiddenClients) more")
                                .font(.caption).foregroundStyle(.secondary)
                                .position(x: layout.x(0), y: geo.size.height - 8)
                        }
                        QueueNodeView(queue: live.queue, capacity: live.queueCapacity, streams: live.streams.count)
                            .position(layout.center(.queue) ?? .zero)
                        ForEach(layout.workers) { worker in
                            WorkerNodeView(worker: worker, accelerated: accelerated)
                                .position(layout.center(.worker(worker.id)) ?? .zero)
                        }
                        ForEach(layout.models) { m in
                            ModelNodeView(model: m, busy: live.workers.contains { $0.isBusy && $0.model == m.id })
                                .position(layout.center(.model(m.id)) ?? .zero)
                        }
                    }
                    .frame(width: geo.size.width, height: geo.size.height)
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Host activity: \(live.clients.count) clients, \(live.queue.count) queued, \(live.workers.filter(\.isBusy).count) of \(live.workers.count) workers busy")
    }

    private func columnTitles(_ layout: GraphLayout) -> some View {
        ZStack(alignment: .topLeading) {
            ForEach(Array(["Clients", "Queue", "Workers", "Models"].enumerated()), id: \.offset) { i, title in
                Text(title.uppercased())
                    .font(.system(.caption2, design: .monospaced).weight(.semibold)).tracking(1.4)
                    .foregroundStyle(.secondary)
                    .position(x: i == 0 ? layout.x(0) - GraphLayout.clientSize.width / 2 + 60 : layout.x(i), y: 14)
            }
        }
        .accessibilityHidden(true)
    }

    // MARK: Canvas

    private func draw(in gc: inout GraphicsContext, layout: GraphLayout, time: Double, wall: Date) {
        let idle = Color.fsBlue.opacity(scheme == .dark ? 0.30 : 0.22)
        let t = wall.timeIntervalSinceReferenceDate
        let streaming = Set(live.streams.values)

        for client in layout.clients {
            guard let c = layout.curve(from: .client(client.id), to: .queue) else { continue }
            let active = streaming.contains(client.id)
            if active {
                stroke(&gc, c, color: Color.fsRed.opacity(0.18), width: 9, blur: 4)
                stroke(&gc, c, color: Color.fsRed.opacity(0.85), width: 2.2, dash: reduceMotion ? nil : [4, 8], phase: -t * 30)
            } else {
                stroke(&gc, c, color: idle, width: 1.2)
            }
        }
        for worker in layout.workers {
            guard let c = layout.curve(from: .queue, to: .worker(worker.id)) else { continue }
            let busy = worker.isBusy
            let audio = busy && worker.jobClient.map { streaming.contains($0) } == true
            if busy {
                stroke(&gc, c, color: Color.fsBlue.opacity(0.16), width: 8, blur: 4)
                stroke(&gc, c, color: (audio ? Color.fsRed : Color.fsBlue).opacity(0.75), width: 2,
                       dash: audio && !reduceMotion ? [4, 8] : nil, phase: -t * 30)
            } else {
                stroke(&gc, c, color: idle, width: 1.2)
            }
            if !worker.model.isEmpty, let m = layout.curve(from: .worker(worker.id), to: .model(worker.model)) {
                stroke(&gc, m, color: busy ? Color.fsRed.opacity(0.7) : idle, width: busy ? 2.2 : 1.2)
            }
        }

        // Pulses: a halo that grows out of the node's own shape and fades.
        for (node, pulse) in animator.pulses {
            let age = time - pulse.time
            guard age >= 0, age < 1.2, let p = layout.center(node) else { continue }
            let f = age / 1.2
            let (size, radius): (CGSize, CGFloat) = switch node {
            case .client: (CGSize(width: 50, height: 50), 25)
            case .queue: (GraphLayout.queueSize, 28)
            case .worker: (GraphLayout.workerSize, 22)
            case .model: (GraphLayout.modelSize, 24)
            }
            let origin: CGPoint
            if case .client = node { origin = CGPoint(x: p.x - GraphLayout.clientSize.width / 2 + 25, y: p.y) } else { origin = p }
            let grow = CGFloat(reduceMotion ? 0.3 : f) * 12
            let rect = CGRect(x: origin.x - size.width / 2 - grow, y: origin.y - size.height / 2 - grow,
                              width: size.width + grow * 2, height: size.height + grow * 2)
            gc.stroke(Path(roundedRect: rect, cornerRadius: radius + grow), with: .color(color(for: pulse.kind).opacity((1 - f) * 0.6)), lineWidth: 2)
        }

        // Comets.
        guard !reduceMotion else { return }
        for particle in animator.particles {
            let raw = (time - particle.start) / particle.duration
            guard raw >= 0, raw <= 1, let c = layout.curve(from: particle.from, to: particle.to) else { continue }
            let color = color(for: particle.kind)
            for trail in stride(from: 7, through: 0, by: -1) {
                let p = ease(max(0, raw - Double(trail) * 0.022))
                let pt = bezier(c, p)
                let alpha = trail == 0 ? 1.0 : 0.55 - Double(trail) * 0.065
                let r: CGFloat = trail == 0 ? 4.5 : 3.6 - CGFloat(trail) * 0.35
                gc.fill(Path(ellipseIn: CGRect(x: pt.x - r, y: pt.y - r, width: r * 2, height: r * 2)), with: .color(color.opacity(alpha)))
            }
            let head = bezier(c, ease(raw))
            var glow = gc
            glow.addFilter(.blur(radius: 7))
            glow.fill(Path(ellipseIn: CGRect(x: head.x - 11, y: head.y - 11, width: 22, height: 22)), with: .color(color.opacity(0.55)))
        }
    }

    private func color(for kind: FlowAnimator.Kind) -> Color {
        switch kind {
        case .request: .fsRed
        case .result: .fsBlue
        case .failure: .fsError
        }
    }

    private func stroke(_ gc: inout GraphicsContext, _ c: (CGPoint, CGPoint, CGPoint, CGPoint), color: Color, width: CGFloat,
                        dash: [CGFloat]? = nil, phase: Double = 0, blur: CGFloat = 0) {
        var path = Path()
        path.move(to: c.0)
        path.addCurve(to: c.3, control1: c.1, control2: c.2)
        let style = StrokeStyle(lineWidth: width, lineCap: .round, dash: dash ?? [], dashPhase: CGFloat(phase))
        if blur > 0 {
            var soft = gc
            soft.addFilter(.blur(radius: blur))
            soft.stroke(path, with: .color(color), style: style)
        } else {
            gc.stroke(path, with: .color(color), style: style)
        }
    }

    private func bezier(_ c: (CGPoint, CGPoint, CGPoint, CGPoint), _ t: Double) -> CGPoint {
        let u = 1 - t
        let a = u * u * u, b = 3 * u * u * t, d = 3 * u * t * t, e = t * t * t
        return CGPoint(x: a * c.0.x + b * c.1.x + d * c.2.x + e * c.3.x, y: a * c.0.y + b * c.1.y + d * c.2.y + e * c.3.y)
    }

    /// Fast attack, long settle (the brand's motion rule), no overshoot.
    private func ease(_ t: Double) -> Double { 1 - pow(1 - t, 3) }
}

/// Faint ruled lines, the brand's copybook pattern, behind the constellation.
struct RuledBackground: View {
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        Canvas { gc, size in
            let spacing: CGFloat = 38
            var y: CGFloat = 30
            while y < size.height {
                var line = Path()
                line.move(to: CGPoint(x: 0, y: y))
                line.addLine(to: CGPoint(x: size.width, y: y))
                gc.stroke(line, with: .color(Color.fsRuling.opacity(scheme == .dark ? 0.35 : 0.55)), lineWidth: 1)
                y += spacing
            }
        }
        .accessibilityHidden(true)
    }
}

// MARK: - Nodes

struct ClientNodeView: View {
    var client: HostLiveState.ClientNode

    var body: some View {
        let speaking = client.activeStreams > 0
        HStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.system(size: 17, weight: .medium))
                .foregroundStyle(speaking ? Color.fsRedText : Color.primary.opacity(0.8))
                .frame(width: 48, height: 48)
                .glassEffect(.regular, in: .circle)
                .overlay {
                    if speaking {
                        Circle().strokeBorder(Color.fsRed, lineWidth: 2.5).padding(-3)
                            .shadow(color: Color.fsRed.opacity(0.45), radius: 6)
                    }
                }
            VStack(alignment: .leading, spacing: 1) {
                Text(client.displayName).font(.callout.weight(.semibold)).lineLimit(1)
                Text(speaking ? "speaking" : "\(client.completed) done")
                    .font(.caption).foregroundStyle(speaking ? Color.fsRedText : .secondary).lineLimit(1)
                    .transaction { $0.animation = nil }
            }
            Spacer(minLength: 0)
        }
        .frame(width: GraphLayout.clientSize.width, height: GraphLayout.clientSize.height)
        .animation(.smooth(duration: 0.3), value: speaking)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Client \(client.displayName)\(speaking ? ", speaking" : "")")
    }

    private var symbol: String {
        let id = client.id.lowercased()
        if id.contains("@") { return "person" }
        if id.contains("reception") || id.contains("station") || id.contains("desk") { return "desktopcomputer" }
        if id.contains("iphone") || id.contains("phone") { return "iphone" }
        return "laptopcomputer"
    }
}

struct QueueNodeView: View {
    var queue: [HostLiveState.QueueItem]
    var capacity: Int
    var streams: Int

    var body: some View {
        let cap = max(capacity, queue.count, 1)
        VStack(alignment: .leading, spacing: 6) {
            Text("Queue").font(.callout.weight(.medium)).foregroundStyle(.secondary)
            Text("\(queue.count)").font(.mvNumber(40, weight: .bold)).contentTransition(.numericText())
            HStack(spacing: 3) {
                ForEach(0..<min(cap, 12), id: \.self) { i in
                    Capsule().fill(i < queue.count ? Color.fsRed : Color.primary.opacity(0.13)).frame(width: 7, height: 12)
                }
            }
            Text("\(streams) live · \(queue.count) waiting").font(.caption).foregroundStyle(.secondary)
        }
        .padding(.horizontal, 18)
        .frame(width: GraphLayout.queueSize.width, height: GraphLayout.queueSize.height, alignment: .leading)
        .glassEffect(.regular.tint(Color.fsBlue.opacity(0.10)), in: .rect(cornerRadius: 26))
        .animation(.smooth(duration: 0.25), value: queue.count)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Queue: \(queue.count) of \(capacity), \(streams) live streams")
    }
}

struct WorkerNodeView: View {
    var worker: HostLiveState.WorkerNode
    var accelerated: Bool

    var body: some View {
        let busy = worker.isBusy
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Worker \(worker.id + 1)").font(.callout.weight(.semibold))
                Spacer()
                Text(accelerated ? "ANE" : "CPU").font(.system(.caption2, design: .monospaced).weight(.semibold)).foregroundStyle(.secondary)
            }
            TimelineView(.periodic(from: .now, by: busy ? 0.1 : 3600)) { ctx in
                let elapsed = worker.jobStartedAt.map { max(0, ctx.date.timeIntervalSince1970 * 1000 - $0) / 1000 } ?? 0
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 6) {
                        if busy { Circle().fill(Color.fsRed).frame(width: 7, height: 7) }
                        Text(detail(elapsed: elapsed)).font(.caption).foregroundStyle(stateColor).lineLimit(1)
                    }
                    GeometryReader { g in
                        ZStack(alignment: .leading) {
                            Capsule().fill(Color.primary.opacity(0.10))
                            if busy {
                                Capsule().fill(Color.fsRed).frame(width: g.size.width * min(1, max(0.06, elapsed / 12)))
                            }
                        }
                    }
                    .frame(height: 4)
                }
            }
        }
        .padding(.horizontal, 16)
        .frame(width: GraphLayout.workerSize.width, height: GraphLayout.workerSize.height)
        .glassEffect(.regular, in: .rect(cornerRadius: 22))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Worker \(worker.id + 1), \(detail(elapsed: 0))")
    }

    private var stateColor: Color {
        worker.state == "model-unavailable" ? .fsError : busy ? .primary : .secondary
    }

    private var busy: Bool { worker.isBusy }

    private func detail(elapsed: Double) -> String {
        switch worker.state {
        case "transcribing":
            let who = worker.jobClient.map { HostLiveState.displayName(forClient: $0) } ?? "Transcribing"
            return String(format: "%@ · %.1f s", who, elapsed)
        case "loading": return "Loading model…"
        case "model-unavailable": return "Model not installed"
        default: return "Idle · \(worker.completedJobs) done"
        }
    }
}

struct ModelNodeView: View {
    var model: HostLiveState.ModelNode
    var busy: Bool

    var body: some View {
        HStack(spacing: 12) {
            FairspokenMark(size: 26, monochrome: model.installed ? nil : .secondary)
                .frame(width: 38, height: 38)
                .background(Color.primary.opacity(0.06), in: .rect(cornerRadius: 10))
            VStack(alignment: .leading, spacing: 3) {
                Text(model.name).font(.callout.weight(.semibold)).lineLimit(1).minimumScaleFactor(0.85)
                if let pct = model.downloadPercentage {
                    ProgressView(value: pct / 100).tint(.fsBlue).frame(width: 130)
                    Text("\(model.downloadStage?.capitalized ?? "Downloading") \(Int(pct))%").font(.caption2).foregroundStyle(.secondary)
                } else {
                    Text(subtitle).font(.caption).foregroundStyle(model.downloadError != nil ? Color.fsError : .secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .frame(width: GraphLayout.modelSize.width, height: GraphLayout.modelSize.height)
        .glassEffect(busy ? .regular.tint(Color.fsRed.opacity(0.06)) : .regular, in: .rect(cornerRadius: 24))
        .opacity(model.installed ? 1 : 0.65)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Model \(model.name), \(subtitle)")
    }

    private var subtitle: String {
        if let error = model.downloadError { return error }
        if !model.installed { return "Not installed" }
        let workers = model.assignedWorkers.count
        return workers == 0 ? "Installed · not loaded" : "Speech · \(workers) worker\(workers == 1 ? "" : "s")"
    }
}
