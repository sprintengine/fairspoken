import FairspokenUI
import MultiVoiceCore
import SwiftUI

struct ModelsView: View {
    @Environment(AppModel.self) private var model
    @State private var pendingDelete: SpeechModelInfo?

    var body: some View {
        ScrollView {
            GlassEffectContainer(spacing: 4) {
                VStack(alignment: .leading, spacing: 22) {
                    header
                    ActiveModelBanner()
                    Text("Speech models").font(.title3.weight(.semibold))
                    // Three catalogue entries: equal columns, equal heights.
                    HStack(alignment: .top, spacing: 18) {
                        ForEach(SpeechModelCatalog.all) { info in
                            ModelCard(info: info, onDelete: { pendingDelete = info })
                                .frame(maxWidth: .infinity)
                        }
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    footnote
                }
                .padding(.horizontal, 28)
                .padding(.vertical, 22)
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
            Text("You can download it again at any time. The first load after a download includes a one-time Neural Engine optimisation (about 15 s).")
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Models").font(.system(size: 30, weight: .bold, design: .rounded))
            Text("Speech recognition runs on the Apple Neural Engine. Nothing you say leaves this Mac.")
                .font(.title3).foregroundStyle(.secondary)
        }
    }

    private var footnote: some View {
        Label("Word error rates are FluidAudio's LibriSpeech test-clean / test-other figures; speed is real-time factor on the Neural Engine. AI polish models arrive in a later update.",
              systemImage: "info.circle")
            .font(.caption).foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// The model in use, with live engine state.
struct ActiveModelBanner: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let info = model.models.activeModel
        let state = model.models.engineState
        GlassCard(cornerRadius: 28, padding: 22, tint: Color.mvTeal.opacity(0.10)) {
            HStack(spacing: 18) {
                ModelGlyph(info: info, size: 64)
                VStack(alignment: .leading, spacing: 6) {
                    Eyebrow(text: "In use", symbol: "checkmark.seal")
                    Text(info.name).font(.title2.weight(.semibold))
                    HStack(spacing: 8) {
                        Tag(text: "Neural Engine", symbol: "brain", color: .mvTeal)
                        Tag(text: info.languages, symbol: "globe")
                        Tag(text: "≈ \(Format.bytes(info.memoryBytes)) memory", symbol: "memorychip")
                    }
                }
                Spacer()
                VStack(alignment: .trailing, spacing: 6) {
                    HStack(spacing: 8) {
                        StatusDot(color: state.isReady ? .mvGreen : state.isBusy ? .mvAmber : .secondary, pulsing: state.isBusy)
                        Text(state.label).font(.callout.weight(.medium))
                    }
                    if let seconds = model.models.lastLoadSeconds {
                        Text(String(format: "Loaded and warmed in %.1f s", seconds)).font(.caption).foregroundStyle(.secondary)
                    }
                    if case .downloading(let f) = state {
                        ProgressView(value: f).frame(width: 160).tint(.mvTeal)
                    }
                }
            }
        }
    }
}

struct ModelGlyph: View {
    var info: SpeechModelInfo
    var size: CGFloat = 52

    var body: some View {
        let colors: [Color] = info.id == "parakeet-ultra" ? [.mvIndigo, .mvTeal] : info.id.hasSuffix("v2") ? [.gray, .mvTeal] : [.mvTeal, .mvIndigo]
        ZStack {
            RoundedRectangle(cornerRadius: size * 0.3, style: .continuous)
                .fill(LinearGradient(colors: colors.map { $0.opacity(0.9) }, startPoint: .topLeading, endPoint: .bottomTrailing))
            Image(systemName: info.id == "parakeet-ultra" ? "waveform.badge.magnifyingglass" : "waveform")
                .font(.system(size: size * 0.42, weight: .semibold))
                .foregroundStyle(.white)
        }
        .frame(width: size, height: size)
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
        GlassCard(cornerRadius: 26, padding: 20, tint: active ? Color.mvTeal.opacity(0.10) : nil, fillHeight: true) {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .top, spacing: 14) {
                    ModelGlyph(info: info)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(info.name).font(.headline).lineLimit(2).fixedSize(horizontal: false, vertical: true)
                        Text(info.publisher).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    if let badge = info.badge {
                        Tag(text: badge, color: badge == "Most accurate" ? .mvIndigo : .mvTeal)
                            .fixedSize()
                    }
                }
                Text(info.summary)
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(minHeight: 38, alignment: .top)

                Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 8) {
                    GridRow {
                        Text("Accuracy").font(.caption).foregroundStyle(.secondary)
                        DotMeter(value: info.accuracy, color: .mvTeal)
                        Text(info.werClean.map { String(format: "%.2f%% / %.2f%% WER", $0, info.werOther ?? 0) } ?? "English benchmark")
                            .font(.caption).foregroundStyle(.secondary).monospacedDigit()
                    }
                    GridRow {
                        Text("Speed").font(.caption).foregroundStyle(.secondary)
                        DotMeter(value: info.speed, color: .mvIndigo)
                        Text(info.realTimeFactor.map { String(format: "%.0f× real time", $0) } ?? "≈ v3")
                            .font(.caption).foregroundStyle(.secondary).monospacedDigit()
                    }
                }

                HStack(spacing: 8) {
                    Tag(text: info.languages.hasPrefix("25") ? "25 languages" : "English", symbol: "globe", color: .mvTeal)
                        .help(info.languages)
                    Tag(text: Format.bytes(library.diskBytes[info.id] ?? info.approxBytes), symbol: "arrow.down.circle")
                        .help("Size on disk")
                    Tag(text: Format.bytes(info.memoryBytes), symbol: "memorychip")
                        .help("Neural Engine memory while loaded")
                }

                Spacer(minLength: 0)
                Divider().opacity(0.5)
                actions(state: state, active: active)
            }
        }
    }

    @ViewBuilder private func actions(state: ModelLibrary.ItemState, active: Bool) -> some View {
        HStack(spacing: 10) {
            switch state {
            case .available:
                Label("Not downloaded", systemImage: "icloud.and.arrow.down").font(.callout).foregroundStyle(.secondary).lineLimit(1)
                Spacer()
                Button("Download", systemImage: "arrow.down") { model.models.download(info.id) }
                    .buttonStyle(.glass)
            case .downloading(let fraction):
                ProgressView(value: fraction) {
                    Text("Downloading \(Int(fraction * 100))%").font(.caption)
                }
                .tint(.mvTeal)
            case .compiling:
                ProgressView().controlSize(.small)
                Text("Optimising for the Neural Engine…").font(.callout).foregroundStyle(.secondary)
                Spacer()
            case .installed:
                if active {
                    Label("In use", systemImage: "checkmark.circle.fill").font(.callout.weight(.semibold)).foregroundStyle(Color.mvGreen)
                    Spacer()
                } else {
                    Label("Installed", systemImage: "internaldrive").font(.callout).foregroundStyle(.secondary)
                    Spacer()
                    Button(role: .destructive, action: onDelete) { Image(systemName: "trash") }
                        .buttonStyle(.glass)
                        .help("Delete model files")
                        .accessibilityLabel("Delete \(info.name)")
                    Button("Use") {
                        model.settings.update { $0.model = info.id }
                        model.models.prepare(info.id)
                    }
                    .buttonStyle(.glassProminent)
                    .tint(.mvTeal)
                }
            }
        }
        .frame(minHeight: 30)
    }
}
