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
        let locked = demo || !controller.runState.isRunning
        CrystalPage {
            PageHeader(title: "Models", subtitle: demo ? "Demo data. Switch to Live to manage this server's models." : nil)
            NoticeBanner()
            ForEach(models, id: \.id) { m in
                ModelCard(model: m, info: SpeechModelCatalog.model(id: m.id), download: stats.modelDownload?.model == m.id ? stats.modelDownload : nil,
                          actionsDisabled: locked,
                          // Screenshots show the link controls as they are when serving live.
                          linkActionsDisabled: locked && !controller.isPresenting,
                          onDownload: { controller.download(m.id) }, onDelete: { confirmDelete = m.id })
            }
            if models.isEmpty {
                SurfaceCard {
                    Text(controller.runState.isRunning ? "Loading…" : "Start serving to manage models.").foregroundStyle(Crystal.ink2)
                }
            }
            SectionTitle(text: "Workers")
                .help("Workers serving the same model share one copy of it in memory. A change applies at once; a worker finishes its current job first.")
            SurfaceCard(padding: 8) {
                VStack(spacing: 0) {
                    ForEach(stats.workers, id: \.index) { w in
                        if w.index != stats.workers.first?.index { Rectangle().fill(Crystal.hairline).frame(height: 1).padding(.horizontal, 12) }
                        HStack(spacing: 14) {
                            Text("Worker \(w.index + 1)").font(.body.weight(.medium)).foregroundStyle(Crystal.ink).frame(width: 90, alignment: .leading)
                            Picker("Model", selection: Binding(get: { w.assignedModel }, set: { controller.assign(model: $0, toWorker: w.index) })) {
                                ForEach(models, id: \.id) { m in
                                    Text(m.name + (m.installed ? "" : " (not installed)")).tag(m.id)
                                }
                            }
                            .labelsHidden()
                            .frame(width: 260)
                            .disabled(locked)
                            workerState(w)
                            Spacer()
                            Text("\(w.completedJobs) done").font(.fsData(.callout)).foregroundStyle(Crystal.ink3)
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 10)
                    }
                }
            }
        }
        .confirmationDialog("Delete this model from the Mac?", isPresented: Binding(get: { confirmDelete != nil }, set: { if !$0 { confirmDelete = nil } }),
                            presenting: confirmDelete) { id in
            Button("Delete", role: .destructive) { controller.delete(id) }
        } message: { _ in
            Text("Fairspoken on this Mac uses the same files. You can download it again.")
        }
    }

    @ViewBuilder private func workerState(_ w: HostStats.Worker) -> some View {
        switch w.state {
        case "transcribing": Text("Transcribing").font(.callout).foregroundStyle(Crystal.ink)
        case "loading": Text("Loading the model…").font(.callout).foregroundStyle(Crystal.ink2)
        case "model-unavailable":
            Label(w.lastError ?? "Model not installed", systemImage: "exclamationmark.triangle.fill").font(.callout).foregroundStyle(Crystal.error)
        default: Text(w.loadedModel == nil ? "Idle" : "Ready").font(.callout).foregroundStyle(Crystal.ink3)
        }
    }
}

private struct ModelCard: View {
    var model: HostStats.Model
    var info: SpeechModelInfo?
    var download: HostStats.ModelDownload?
    var actionsDisabled: Bool
    var linkActionsDisabled: Bool
    var onDownload: () -> Void
    var onDelete: () -> Void
    @Environment(ServerController.self) private var controller

    var body: some View {
        let downloading = download.map { $0.stage != "ready" && $0.stage != "error" } ?? false
        let link = controller.effectiveModelLinks[model.id]
        let linkFromEnvironment = controller.linksFromEnvironment[model.id] != nil
        let editing = controller.linkEditorModel == model.id && !model.installed
        SurfaceCard {
            HStack(alignment: .top, spacing: 16) {
                FairspokenMark(size: 30, monochrome: model.installed ? Crystal.ink2 : Crystal.ink3)
                    .frame(width: 48, height: 48)
                    .crystalWell()
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 8) {
                        Text(model.name).font(.title3.weight(.semibold)).foregroundStyle(Crystal.ink)
                        if let badge = info?.badge { Tag(text: badge, color: Crystal.ink2) }
                    }
                    Text(facts).font(.callout).foregroundStyle(Crystal.ink2)
                    if let summary = info?.summary { Text(summary).font(.callout).foregroundStyle(Crystal.ink) }
                    if downloading, let d = download {
                        HStack(spacing: 10) {
                            ProgressView(value: d.percentage / 100).tint(Crystal.serverAccent).frame(width: 240)
                            // From a link, validating is unpacking and checking (it compiles on first load).
                            Text(d.stage == "validating" ? (link != nil ? "Unpacking…" : "Optimising for this Mac…") : "\(d.stage.capitalized) \(Int(d.percentage))%")
                                .font(.caption).foregroundStyle(Crystal.ink2)
                        }
                    } else if let error = download?.error {
                        ErrorLabel(text: error).font(.caption)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                    if editing {
                        ModelLinkEditor(text: Bindable(controller).linkDraft, tint: Crystal.serverAccent,
                                        submit: { controller.downloadFromLink(model.id, $0) },
                                        cancel: { controller.linkEditorModel = nil })
                            .frame(maxWidth: 560)
                            .padding(.top, 6)
                    } else if let link {
                        HStack(alignment: .firstTextBaseline, spacing: 14) {
                            ModelLinkSummary(link: link, note: linkFromEnvironment
                                ? "\(HostEnvironment.modelLinksVariable) sets this model's link for this run." : nil)
                            if !linkFromEnvironment {
                                Button("Use Standard Download") { controller.removeModelLink(model.id) }
                                    .buttonStyle(.link).font(.caption)
                                    .disabled(linkActionsDisabled || downloading)
                                    .help("Download this model the usual way instead of from the link")
                            }
                        }
                        .padding(.top, 4)
                    }
                }
                Spacer()
                VStack(alignment: .trailing, spacing: 10) {
                    if model.installed {
                        Label(model.assignedWorkers.isEmpty ? "Installed" : "Serving on \(workerList)", systemImage: "checkmark.circle.fill")
                            .font(.callout.weight(.medium)).foregroundStyle(model.assignedWorkers.isEmpty ? Crystal.ink2 : Crystal.ok)
                        Button("Delete…", role: .destructive, action: onDelete)
                            .buttonStyle(.bordered).buttonBorderShape(.capsule)
                            .disabled(actionsDisabled || !model.assignedWorkers.isEmpty)
                            .help(model.assignedWorkers.isEmpty ? "Remove the model's files" : "Assign its workers another model first")
                    } else {
                        Button(downloading ? "Downloading…" : "Download", systemImage: "arrow.down.circle", action: onDownload)
                            .buttonStyle(.borderedProminent).buttonBorderShape(.capsule).tint(Crystal.serverAccent)
                            .disabled(actionsDisabled || downloading)
                        Button("Download from Link…") { controller.openLinkEditor(for: model.id) }
                            .buttonStyle(.bordered).buttonBorderShape(.capsule)
                            .disabled(linkActionsDisabled || downloading || editing || linkFromEnvironment)
                            .help(linkFromEnvironment ? "\(HostEnvironment.modelLinksVariable) sets this model's link for this run."
                                : "Download a .zip of this model from a link you give, for example your organisation's server")
                        if !model.assignedWorkers.isEmpty {
                            ErrorLabel(text: "Assigned but missing").font(.caption)
                        }
                    }
                }
            }
        }
    }

    /// "NVIDIA · 25 European languages · 482 MB · WER 2.27%", the accuracy on hover.
    private var facts: String {
        var parts = [model.publisher]
        if let languages = info?.languages { parts.append(languages) }
        parts.append(Format.bytes(Int64(model.sizeBytes)))
        if let info, let wer = info.werClean, let rtf = info.realTimeFactor {
            parts.append(String(format: "%.2f%% WER · %.0f× real time", wer, rtf))
        }
        return parts.joined(separator: " · ")
    }

    private var workerList: String {
        let names = model.assignedWorkers.map { "\($0 + 1)" }
        return names.count == 1 ? "worker \(names[0])" : "workers \(names.joined(separator: ", "))"
    }
}
