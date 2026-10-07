import FairspokenUI
import KeyboardShortcuts
import FairspokenCore
import SwiftUI

/// First-run guide: welcome → permissions (live status) → shortcut → model ready + try it.
struct OnboardingView: View {
    enum Step: Int, CaseIterable { case welcome, permissions, shortcut, ready }

    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var step: Step
    var onFinish: () -> Void

    init(initialStep: Step = .welcome, onFinish: @escaping () -> Void) {
        _step = State(initialValue: initialStep)
        self.onFinish = onFinish
    }

    var body: some View {
        ZStack {
            CrystalBackground(accent: Crystal.clientAccent)
            VStack(spacing: 0) {
                progress
                    .padding(.top, 34)
                GlassEffectContainer(spacing: Layout.gap) {
                    Group {
                        switch step {
                        case .welcome: welcome
                        case .permissions: permissions
                        case .shortcut: shortcut
                        case .ready: ready
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .padding(.horizontal, 44)
                    .transition(reduceMotion ? .opacity : .asymmetric(insertion: .move(edge: .trailing).combined(with: .opacity),
                                                                      removal: .move(edge: .leading).combined(with: .opacity)))
                }
                footer
                    .padding(.horizontal, 32)
                    .padding(.bottom, 26)
            }
        }
        .frame(width: 720, height: 560)
        .onAppear { model.permissions.beginPolling() }
        .onDisappear { model.permissions.endPolling() }
    }

    private var progress: some View {
        HStack(spacing: 8) {
            ForEach(Step.allCases, id: \.rawValue) { s in
                Capsule().fill(s.rawValue <= step.rawValue ? Crystal.clientAccent : Crystal.ink3.opacity(0.3))
                    .frame(width: s == step ? 28 : 8, height: 8)
            }
        }
        .animation(reduceMotion ? nil : .smooth, value: step)
        .accessibilityElement()
        .accessibilityLabel("Step \(step.rawValue + 1) of \(Step.allCases.count)")
    }

    private func stepTitle(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 28, weight: .semibold)).tracking(-0.4)
            .foregroundStyle(Crystal.ink)
            .accessibilityAddTraits(.isHeader)
    }

    private var welcome: some View {
        VStack(spacing: 16) {
            Spacer()
            BrandMark(size: 88)
                .padding(.bottom, 8)
            Text("Welcome to \(AppInfo.displayName)")
                .font(.system(size: 32, weight: .semibold)).tracking(-0.5)
                .foregroundStyle(Crystal.ink)
                .accessibilityAddTraits(.isHeader)
            Text("Speak anywhere. Your words appear where you type.")
                .font(.title3).foregroundStyle(Crystal.ink2).multilineTextAlignment(.center)
            HStack(spacing: 10) {
                Chip(text: "On-device", symbol: "lock")
                Chip(text: "≈ 60 ms to text", symbol: "bolt")
                Chip(text: "25 languages", symbol: "globe")
            }
            .padding(.top, 8)
            Spacer()
        }
    }

    private var permissions: some View {
        VStack(alignment: .leading, spacing: 18) {
            Spacer(minLength: 12)
            stepTitle("Permissions")
            GlassCard(cornerRadius: Layout.card, padding: 6) {
                VStack(spacing: 0) {
                    row(.microphone, optional: false)
                    divider
                    row(.accessibility, optional: true)
                        .help("Without it, dictations are copied for you to paste.")
                    if model.settings.settings.fnPushToTalk {
                        divider
                        row(.inputMonitoring, optional: true)
                    }
                }
            }
            Spacer(minLength: 12)
        }
    }

    private func row(_ kind: Permissions.Kind, optional: Bool) -> some View {
        PermissionRow(kind: kind, optional: optional)
            .padding(.horizontal, 12)
            .padding(.vertical, 12)
    }

    private var divider: some View {
        Divider().overlay(Crystal.hairline).padding(.leading, 54).padding(.trailing, 12)
    }

    private var shortcut: some View {
        VStack(alignment: .leading, spacing: 18) {
            Spacer(minLength: 12)
            stepTitle("Your shortcut")
            GlassCard(cornerRadius: Layout.card, padding: 20) {
                VStack(alignment: .leading, spacing: 14) {
                    LabeledContent("Shortcut") { KeyboardShortcuts.Recorder(for: .dictation) { _ in model.mirrorShortcut() } }
                    Picker("Mode", selection: Binding(get: { model.settings.settings.recordingShortcutMode },
                                                      set: { v in model.settings.update { $0.recordingShortcutMode = v } })) {
                        Text("Press to start and stop").tag(RecordingShortcutMode.toggle)
                        Text("Hold to talk").tag(RecordingShortcutMode.pushToTalk)
                    }
                    .pickerStyle(.segmented)
                    .tint(Crystal.clientAccent)
                    Toggle("Also hold fn (🌐)", isOn: Binding(get: { model.settings.settings.fnPushToTalk }, set: { v in
                        model.settings.update { $0.fnPushToTalk = v }
                        if v && !model.permissions.inputMonitoring.isGranted { model.permissions.request(.inputMonitoring) }
                        model.applyFnSetting()
                    }))
                    .tint(Crystal.clientAccent)
                    if model.settings.settings.fnPushToTalk {
                        PermissionRow(kind: .inputMonitoring)
                        if Permissions.globeKeyAction != 0 {
                            Label("🌐 is set to “\(Permissions.globeKeyActionName)”. Set it to Do Nothing in Keyboard settings.",
                                  systemImage: "exclamationmark.triangle")
                                .font(.caption).foregroundStyle(Crystal.warn)
                        }
                    }
                }
            }
            Spacer(minLength: 12)
        }
    }

    private var ready: some View {
        HStack(spacing: 28) {
            VStack(alignment: .leading, spacing: 14) {
                stepTitle("Try it")
                engineStatus
                GlassCard(cornerRadius: Layout.card, padding: 16) {
                    Text(model.dictation.lastInAppResult?.text ?? "Say a sentence. It appears here.")
                        .font(.title3)
                        .foregroundStyle(model.dictation.lastInAppResult == nil ? Crystal.ink3 : Crystal.ink)
                        .frame(maxWidth: .infinity, minHeight: 70, alignment: .topLeading)
                        .textSelection(.enabled)
                }
            }
            MicButton(dictation: model.dictation, size: 104) { model.dictation.toggleFromUI() }
                .disabled(!model.models.engineState.isReady && model.settings.settings.transcriptionLocation == .local)
        }
    }

    @ViewBuilder private var engineStatus: some View {
        let state = model.models.engineState
        HStack(spacing: 8) {
            if state.isBusy { ProgressView().controlSize(.small) } else { StatusDot(color: state.tint) }
            Text("\(model.models.activeModel.shortName) · \(state.label)").font(.callout).foregroundStyle(Crystal.ink2)
            if case .downloading = state {
                Text("\(Format.bytes(model.models.activeModel.approxBytes)), once").font(.caption).foregroundStyle(Crystal.ink3)
            }
        }
        if case .downloading(let f) = state { ProgressView(value: f).tint(Crystal.clientAccent) }
    }

    private var footer: some View {
        HStack {
            if step != .welcome {
                Button("Back") { go(-1) }.buttonStyle(.glass).controlSize(.large)
            }
            Spacer()
            if step == .ready {
                Button("Start dictating") {
                    model.completeOnboarding()
                    onFinish()
                }
                .buttonStyle(.glassProminent).tint(Crystal.clientAccent).controlSize(.large)
                .keyboardShortcut(.defaultAction)
            } else {
                Button(step == .welcome ? "Get started" : "Continue") { go(1) }
                    .buttonStyle(.glassProminent).tint(Crystal.clientAccent).controlSize(.large)
                    .keyboardShortcut(.defaultAction)
            }
        }
    }

    private func go(_ delta: Int) {
        guard let next = Step(rawValue: step.rawValue + delta) else { return }
        if next == .ready { model.models.prepare(model.settings.settings.model) }
        withAnimation(reduceMotion ? .easeInOut(duration: 0.15) : .spring(response: 0.45, dampingFraction: 0.85)) { step = next }
    }
}
