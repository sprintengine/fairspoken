import FairspokenHost
import FairspokenUI
import FairspokenCore
import SwiftUI

/// Installed and available models, downloads, and which model each worker serves.
struct ModelsView: View {
    @Environment(ServerController.self) private var controller
    @State private var confirmDelete: String?

    var body: some View {
        let stats = controller.stats
        let models = stats.models ?? []
        let demo = controller.source == .demo
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                PageHeader(title: "Models", subtitle: "Parakeet on the Apple Neural Engine · models live in ~/Library/Application Support/FluidAudio/Models")
                NoticeBanner()
                if demo {
                    Label("Demo data. Switch to Live to manage this server's models.", systemImage: "info.circle")
                        .font(.callout).foregroundStyle(.secondary)
                }
                ForEach(models, id: \.id) { m in
                    ModelCard(model: m, info: SpeechModelCatalog.model(id: m.id), download: stats.modelDownload?.model == m.id ? stats.modelDownload : nil,
                              actionsDisabled: demo || !controller.runState.isRunning,
                              onDownload: { controller.download(m.id) }, onDelete: { confirmDelete = m.id })
                }
                if models.isEmpty {
                    SurfaceCard { Text(controller.runState.isRunning ? "Loading…" : "Start the server to manage models.").foregroundStyle(.secondary) }
                }
                Text("Workers").font(.title3.weight(.semibold)).padding(.top, 6)
                SurfaceCard {
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(stats.workers, id: \.index) { w in
                            HStack(spacing: 14) {
                                Text("Worker \(w.index + 1)").font(.body.weight(.semibold)).frame(width: 90, alignment: .leading)
                                Picker("Model", selection: Binding(get: { w.assignedModel }, set: { controller.assign(model: $0, toWorker: w.index) })) {
                                    ForEach(models, id: \.id) { m in
                                        Text(m.name + (m.installed ? "" : " (not installed)")).tag(m.id)
                                    }
                                }
                                .labelsHidden()
                                .frame(width: 280)
                                .disabled(demo || !controller.runState.isRunning)
                                Text(workerState(w)).font(.callout).foregroundStyle(w.state == "model-unavailable" ? Color.fsError : .secondary)
                                Spacer()
                                Text("\(w.completedJobs) done").font(.fsData(.callout)).foregroundStyle(.secondary)
                            }
                            if w.index != stats.workers.last?.index { Divider() }
                        }
                        Text("Workers serving the same model share one copy of it in memory. A change applies at once; a worker finishes its current job first.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            .padding(.horizontal, 28)
            .padding(.vertical, 22)
        }
        .confirmationDialog("Delete this model from the Mac?", isPresented: Binding(get: { confirmDelete != nil }, set: { if !$0 { confirmDelete = nil } }),
                            presenting: confirmDelete) { id in
            Button("Delete", role: .destructive) { controller.delete(id) }
        } message: { _ in
            Text("Its files are removed from FluidAudio's model folder, which Fairspoken on this Mac also uses. You can download it again.")
        }
    }

    private func workerState(_ w: HostStats.Worker) -> String {
        switch w.state {
        case "transcribing": "Transcribing"
        case "loading": "Loading the model…"
        case "model-unavailable": w.lastError ?? "Model not installed"
        default: w.loadedModel == nil ? "Idle" : "Ready"
        }
    }
}

private struct ModelCard: View {
    var model: HostStats.Model
    var info: SpeechModelInfo?
    var download: HostStats.ModelDownload?
    var actionsDisabled: Bool
    var onDownload: () -> Void
    var onDelete: () -> Void

    var body: some View {
        let downloading = download.map { $0.stage != "ready" && $0.stage != "error" } ?? false
        SurfaceCard(padding: 20) {
            HStack(alignment: .top, spacing: 18) {
                FairspokenMark(size: 34, monochrome: model.installed ? nil : .secondary)
                    .frame(width: 54, height: 54)
                    .background(Color.primary.opacity(0.05), in: .rect(cornerRadius: 14))
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 8) {
                        Text(model.name).font(.title3.weight(.semibold))
                        if let badge = info?.badge { Tag(text: badge, color: .fsBlue) }
                    }
                    Text("\(model.publisher) · \(info?.languages ?? "") · \(Format.bytes(Int64(model.sizeBytes)))\(model.installed ? " on disk" : " download")")
                        .font(.callout).foregroundStyle(.secondary)
                    if let summary = info?.summary { Text(summary).font(.callout) }
                    if let info, let wer = info.werClean {
                        Text(String(format: "LibriSpeech word error rate %.2f%% clean, %.2f%% other · %.0f× faster than real time", wer, info.werOther ?? 0, info.realTimeFactor ?? 0))
                            .font(.fsData(.caption)).foregroundStyle(.secondary)
                    }
                    if downloading, let d = download {
                        HStack(spacing: 10) {
                            ProgressView(value: d.percentage / 100).tint(.fsBlue).frame(width: 260)
                            Text(d.stage == "validating" ? "Optimising for this Mac…" : "\(d.stage.capitalized) \(Int(d.percentage))%")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    } else if let error = download?.error {
                        Label(error, systemImage: "exclamationmark.triangle.fill").font(.caption).foregroundStyle(Color.fsError)
                    }
                }
                Spacer()
                VStack(alignment: .trailing, spacing: 10) {
                    if model.installed {
                        Label(model.assignedWorkers.isEmpty ? "Installed" : "Serving on \(workerList)", systemImage: "checkmark.circle.fill")
                            .font(.callout.weight(.medium)).foregroundStyle(Color.fsSuccess)
                        Button("Delete…", role: .destructive, action: onDelete)
                            .buttonStyle(.glass)
                            .disabled(actionsDisabled || !model.assignedWorkers.isEmpty)
                            .help(model.assignedWorkers.isEmpty ? "Remove the model's files" : "Assign its workers another model first")
                    } else {
                        Button(downloading ? "Downloading…" : "Download", systemImage: "arrow.down.circle", action: onDownload)
                            .buttonStyle(.glassProminent).tint(.fsBlue)
                            .disabled(actionsDisabled || downloading)
                        if model.assignedWorkers.isEmpty == false {
                            Text("Assigned but missing").font(.caption).foregroundStyle(Color.fsError)
                        }
                    }
                }
            }
        }
    }

    private var workerList: String {
        let names = model.assignedWorkers.map { "\($0 + 1)" }
        return names.count == 1 ? "worker \(names[0])" : "workers \(names.joined(separator: ", "))"
    }
}
