import FairspokenUI
import KeyboardShortcuts
import FairspokenCore
import SwiftUI

/// First-run guide: welcome → permissions (live status) → shortcut → model ready + try it.
struct OnboardingView: View {
    enum Step: Int, CaseIterable { case welcome, permissions, shortcut, ready }

    @Environment(AppModel.self) private var model
    @State private var step: Step
    var onFinish: () -> Void

    init(initialStep: Step = .welcome, onFinish: @escaping () -> Void) {
        _step = State(initialValue: initialStep)
        self.onFinish = onFinish
    }

    var body: some View {
        ZStack {
            AuroraBackground()
            VStack(spacing: 0) {
                progress
                    .padding(.top, 34)
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
                .transition(.asymmetric(insertion: .move(edge: .trailing).combined(with: .opacity),
                                        removal: .move(edge: .leading).combined(with: .opacity)))
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
                Capsule().fill(s.rawValue <= step.rawValue ? Color.mvTeal : Color.primary.opacity(0.15))
                    .frame(width: s == step ? 28 : 8, height: 8)
            }
        }
        .animation(.smooth, value: step)
        .accessibilityLabel("Step \(step.rawValue + 1) of \(Step.allCases.count)")
    }

    private var welcome: some View {
        VStack(spacing: 18) {
            Spacer()
            BrandMark(size: 92)
                .shadow(color: .mvTeal.opacity(0.35), radius: 24, y: 10)
            Text("Welcome to \(AppInfo.displayName)").font(.system(size: 34, weight: .bold, design: .rounded))
            Text("Speak anywhere on your Mac and your words appear where you're typing. Speech is recognised on the Apple Neural Engine.")
                .font(.title3).foregroundStyle(.secondary).multilineTextAlignment(.center)
                .frame(maxWidth: 520)
            HStack(spacing: 10) {
                Chip(text: "On-device", symbol: "lock.fill", tint: .mvTeal)
                Chip(text: "≈ 60 ms to text", symbol: "bolt.fill", tint: .mvTeal)
                Chip(text: "25 languages", symbol: "globe", tint: .mvTeal)
            }
            .padding(.top, 6)
            Spacer()
        }
    }

    private var permissions: some View {
        VStack(alignment: .leading, spacing: 16) {
            Spacer(minLength: 12)
            Text("A few permissions").font(.system(size: 28, weight: .bold, design: .rounded))
            Text("macOS asks you to approve each one. They update here as soon as you allow them in System Settings.")
                .foregroundStyle(.secondary)
            GlassEffectContainer(spacing: 4) {
                VStack(spacing: 10) {
                    card(.microphone, required: true)
                    card(.accessibility, required: false)
                    if model.settings.settings.fnPushToTalk { card(.inputMonitoring, required: false) }
                }
            }
            Spacer(minLength: 12)
        }
    }

    private func card(_ kind: Permissions.Kind, required: Bool) -> some View {
        GlassCard(cornerRadius: 20, padding: 16) {
            VStack(alignment: .leading, spacing: 6) {
                PermissionRow(kind: kind)
                if !required && !model.permissions.status(kind).isGranted {
                    Text(kind == .accessibility
                         ? "Without it, dictations are copied to the clipboard and you paste them yourself."
                         : "Without it, use the keyboard shortcut instead of fn.")
                        .font(.caption).foregroundStyle(.tertiary).padding(.leading, 42)
                }
            }
        }
    }

    private var shortcut: some View {
        VStack(alignment: .leading, spacing: 18) {
            Spacer(minLength: 12)
            Text("Your dictation shortcut").font(.system(size: 28, weight: .bold, design: .rounded))
            GlassCard(cornerRadius: 22, padding: 20) {
                VStack(alignment: .leading, spacing: 14) {
                    LabeledContent("Shortcut") { KeyboardShortcuts.Recorder(for: .dictation) { _ in model.mirrorShortcut() } }
                    Picker("Mode", selection: Binding(get: { model.settings.settings.recordingShortcutMode },
                                                      set: { v in model.settings.update { $0.recordingShortcutMode = v } })) {
                        Text("Press to start and stop").tag(RecordingShortcutMode.toggle)
                        Text("Hold to talk").tag(RecordingShortcutMode.pushToTalk)
                    }
                    .pickerStyle(.segmented)
                    Toggle("Also hold fn (🌐) to dictate", isOn: Binding(get: { model.settings.settings.fnPushToTalk }, set: { v in
                        model.settings.update { $0.fnPushToTalk = v }
                        if v && !model.permissions.inputMonitoring.isGranted { model.permissions.request(.inputMonitoring) }
                        model.applyFnSetting()
                    }))
                    if model.settings.settings.fnPushToTalk {
                        PermissionRow(kind: .inputMonitoring)
                        if Permissions.globeKeyAction != 0 {
                            Text("Tip: set System Settings › Keyboard › “Press 🌐 key to” › Do Nothing (it's currently “\(Permissions.globeKeyActionName)”).")
                                .font(.caption).foregroundStyle(Color.mvAmber)
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
                Text("Try it").font(.system(size: 28, weight: .bold, design: .rounded))
                engineStatus
                Text("Click the mic or use your shortcut and say a sentence. The text appears here — in other apps it's typed where your cursor is.")
                    .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                GlassCard(cornerRadius: 18, padding: 14) {
                    Text(model.dictation.lastInAppResult?.text ?? "Your words will appear here.")
                        .font(.title3)
                        .foregroundStyle(model.dictation.lastInAppResult == nil ? .tertiary : .primary)
                        .frame(maxWidth: .infinity, minHeight: 70, alignment: .topLeading)
                        .textSelection(.enabled)
                }
            }
            MicButton(dictation: model.dictation, size: 110) { model.dictation.toggleFromUI() }
                .disabled(!model.models.engineState.isReady && model.settings.settings.transcriptionLocation == .local)
        }
    }

    @ViewBuilder private var engineStatus: some View {
        let state = model.models.engineState
        HStack(spacing: 10) {
            if state.isBusy { ProgressView().controlSize(.small) } else {
                Image(systemName: state.isReady ? "checkmark.circle.fill" : "exclamationmark.circle")
                    .foregroundStyle(state.isReady ? Color.mvGreen : Color.mvAmber)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text("\(model.models.activeModel.name): \(state.label)").font(.callout.weight(.medium))
                if case .compiling = state {
                    Text("One-time optimisation for this Mac's Neural Engine — about 15 seconds.").font(.caption).foregroundStyle(.secondary)
                } else if case .downloading = state {
                    Text("Downloading \(Format.bytes(model.models.activeModel.approxBytes)) once; later launches take under a second.").font(.caption).foregroundStyle(.secondary)
                }
            }
        }
        if case .downloading(let f) = state { ProgressView(value: f).tint(.mvTeal) }
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
                .buttonStyle(.glassProminent).tint(.mvTeal).controlSize(.large)
                .keyboardShortcut(.defaultAction)
            } else {
                Button(step == .welcome ? "Get started" : "Continue") { go(1) }
                    .buttonStyle(.glassProminent).tint(.mvTeal).controlSize(.large)
                    .keyboardShortcut(.defaultAction)
            }
        }
    }

    private func go(_ delta: Int) {
        guard let next = Step(rawValue: step.rawValue + delta) else { return }
        if next == .ready { model.models.prepare(model.settings.settings.model) }
        withAnimation(.spring(response: 0.45, dampingFraction: 0.85)) { step = next }
    }
}
