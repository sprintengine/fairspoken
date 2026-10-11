import FairspokenUI
import FairspokenCore
import SwiftUI

struct ModelsView: View {
    @Environment(AppModel.self) private var model
    @State private var pendingDelete: SpeechModelInfo?

    var body: some View {
        ScrollView {
            GlassEffectContainer(spacing: Layout.gap) {
                VStack(alignment: .leading, spacing: Layout.gap) {
                    PageTitle("Models", subtitle: "On this Mac's Neural Engine")
                        .padding(.bottom, 4)
                    // Three catalogue entries: equal columns, equal heights.
                    HStack(alignment: .top, spacing: Layout.gap) {
                        ForEach(SpeechModelCatalog.all) { info in
                            ModelCard(info: info, onDelete: { pendingDelete = info })
                                .frame(maxWidth: .infinity)
                        }
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    Text("Error rates: LibriSpeech test-clean / test-other. Speed: real-time factor.")
                        .font(.caption).foregroundStyle(Crystal.ink3)
                        .padding(.horizontal, 4)
                }
                .padding(.horizontal, Layout.pageH)
                .padding(.vertical, Layout.pageTop)
            }
        }
        .scrollEdgeEffectStyle(.soft, for: .top)
        .onAppear { model.models.refresh() }
        .confirmationDialog("Delete \(pendingDelete?.name ?? "")?", isPresented: Binding(get: { pendingDelete != nil }, set: { if !$0 { pendingDelete = nil } })) {
            Button("Delete", role: .destructive) {
                if let id = pendingDelete?.id { model.models.delete(id) }
                pendingDelete = nil
            }
        } message: {
            Text("You can download it again at any time.")
        }
    }
}

/// A silver tile with the model's symbol; the model in use gets the accent.
struct ModelGlyph: View {
    var info: SpeechModelInfo
    var size: CGFloat = 48
    var active = false

    var body: some View {
        Image(systemName: info.id == "parakeet-ultra" ? "waveform.badge.magnifyingglass" : "waveform")
            .font(.system(size: size * 0.42, weight: .semibold))
            .foregroundStyle(active ? Crystal.clientAccent : Crystal.ink2)
            .frame(width: size, height: size)
            .background(active ? Crystal.clientAccent.opacity(0.12) : Crystal.well, in: .rect(cornerRadius: Layout.row, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Layout.row, style: .continuous).strokeBorder(Crystal.hairline))
            .accessibilityHidden(true)
    }
}

struct ModelCard: View {
    var info: SpeechModelInfo
    var onDelete: () -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        let library = model.models
        let state = library.state(of: info.id)
        let active = library.activeModelID == info.id
        GlassCard(cornerRadius: Layout.card, padding: 20, fillHeight: true) {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .top, spacing: 12) {
                    ModelGlyph(info: info, active: active)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(info.name).cardHeading().lineLimit(2).fixedSize(horizontal: false, vertical: true)
                        Text(info.publisher).font(.caption).foregroundStyle(Crystal.ink2).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    if let badge = info.badge {
                        Tag(text: badge, color: Crystal.ink2).fixedSize()
                    }
                }
                Text(info.summary)
                    .font(.callout).foregroundStyle(Crystal.ink2)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(minHeight: 38, alignment: .top)

                Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 8) {
                    GridRow {
                        Text("Accuracy").foregroundStyle(Crystal.ink2)
                        DotMeter(value: info.accuracy, color: Crystal.clientAccent)
                        Text(info.werClean.map { String(format: "%.2f%% / %.2f%%", $0, info.werOther ?? 0) } ?? "English only")
                            .font(.fsData(.caption)).foregroundStyle(Crystal.ink2)
                    }
                    GridRow {
                        Text("Speed").foregroundStyle(Crystal.ink2)
                        DotMeter(value: info.speed, color: Crystal.clientAccent)
                        Text(info.realTimeFactor.map { String(format: "%.0f×", $0) } ?? "≈ v3")
                            .font(.fsData(.caption)).foregroundStyle(Crystal.ink2)
                    }
                }
                .font(.caption)

                HStack(spacing: 6) {
                    Tag(text: info.languages.hasPrefix("25") ? "25 languages" : "English", symbol: "globe", color: Crystal.ink2)
                        .help(info.languages)
                    Tag(text: Format.bytes(library.diskBytes[info.id] ?? info.approxBytes), symbol: "arrow.down.circle", color: Crystal.ink2)
                        .help("Size on disk")
                    Tag(text: Format.bytes(info.memoryBytes), symbol: "memorychip", color: Crystal.ink2)
                        .help("Neural Engine memory while loaded")
                }

                Spacer(minLength: 0)
                Divider().overlay(Crystal.hairline)
                if model.linkEditorModel == info.id, state == .available {
                    ModelLinkEditor(text: Bindable(model).linkDraft, tint: Crystal.clientAccent,
                                    submit: { model.downloadFromLink(info.id, $0) },
                                    cancel: { model.linkEditorModel = nil })
                } else {
                    actions(state: state, active: active)
                    linkRow(state: state)
                }
            }
        }
        .overlay {
            if active {
                RoundedRectangle(cornerRadius: Layout.card).strokeBorder(Crystal.clientAccent.opacity(0.45), lineWidth: 1)
                    .allowsHitTesting(false)
            }
        }
    }

    @ViewBuilder private func actions(state: ModelLibrary.ItemState, active: Bool) -> some View {
        HStack(spacing: 10) {
            switch state {
            case .available:
                if let error = model.models.downloadErrors[info.id] {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .font(.caption).foregroundStyle(Crystal.error)
                        .lineLimit(4).fixedSize(horizontal: false, vertical: true)
                        .help(error)
                        .textSelection(.enabled)
                } else {
                    Text("Not downloaded").font(.callout).foregroundStyle(Crystal.ink3).lineLimit(1)
                }
                Spacer()
                Button("Download", systemImage: "arrow.down") { model.models.download(info.id) }
                    .cardButton()
            case .downloading(let fraction):
                ProgressView(value: fraction) {
                    Text("Downloading \(Int(fraction * 100))%").font(.caption).foregroundStyle(Crystal.ink2)
                }
                .tint(Crystal.clientAccent)
            case .unpacking:
                ProgressView().controlSize(.small)
                Text("Unpacking…").font(.callout).foregroundStyle(Crystal.ink2)
                Spacer()
            case .compiling:
                ProgressView().controlSize(.small)
                Text("Optimising for this Mac…").font(.callout).foregroundStyle(Crystal.ink2)
                Spacer()
            case .installed:
                if active {
                    activeStatus
                } else {
                    Text("Installed").font(.callout).foregroundStyle(Crystal.ink3)
                    Spacer()
                    Button(role: .destructive, action: onDelete) { Image(systemName: "trash") }
                        .cardButton()
                        .help("Delete model files")
                        .accessibilityLabel("Delete \(info.name)")
                    Button("Use") {
                        model.settings.update { $0.model = info.id }
                        model.models.prepare(info.id)
                    }
                    .cardButton()
                }
            }
        }
        .frame(minHeight: 30)
    }

    /// The saved download link, if any, and the link actions while the model isn't downloaded.
    @ViewBuilder private func linkRow(state: ModelLibrary.ItemState) -> some View {
        let saved = model.settings.settings.modelLinks[info.id]
        if saved != nil || state == .available {
            VStack(alignment: .leading, spacing: 6) {
                if let saved { ModelLinkSummary(link: saved) }
                HStack(spacing: 14) {
                    if state == .available {
                        Button("Download from Link…") { model.openLinkEditor(for: info.id) }
                    }
                    if saved != nil {
                        Button("Use Standard Download") { model.removeModelLink(info.id) }
                            .help("Download this model the usual way instead of from the link")
                    }
                }
                .buttonStyle(.link).font(.caption)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// The live engine state, on the card of the model in use.
    @ViewBuilder private var activeStatus: some View {
        let engine = model.models.engineState
        Label("In use", systemImage: "checkmark.circle.fill")
            .font(.callout.weight(.semibold)).foregroundStyle(Crystal.ok)
        Spacer()
        if case .downloading(let f) = engine {
            ProgressView(value: f).frame(width: 100).tint(Crystal.clientAccent)
        } else if engine.isReady, let seconds = model.models.lastLoadSeconds {
            Text(String(format: "Loaded in %.1f s", seconds)).font(.caption).foregroundStyle(Crystal.ink3).monospacedDigit()
        } else if !engine.isReady {
            HStack(spacing: 6) {
                StatusDot(color: engine.tint, pulsing: engine.isBusy)
                Text(engine.label).font(.caption).foregroundStyle(Crystal.ink2).lineLimit(1)
            }
        }
    }
}
