import FairspokenSpeech
import FairspokenUpdates
import AppKit
import FairspokenCore
import Observation

/// Composition root shared by every window, the menu bar item and the pill.
@Observable
final class AppModel {
    enum Section: String, CaseIterable, Identifiable {
        case home, models, settings
        var id: String { rawValue }
        var title: String {
            switch self {
            case .home: "Home"
            case .models: "Models"
            case .settings: "Settings"
            }
        }
        var symbol: String {
            switch self {
            case .home: "waveform"
            case .models: "square.stack.3d.up"
            case .settings: "gearshape"
            }
        }
    }

    var section: Section = .home
    var settingsTab: SettingsView.Tab = .general

    let settings: SettingsStore
    let history: HistoryStore
    let permissions: Permissions
    let models: ModelLibrary
    let dictation: DictationController
    let hostStatus: HostStatus
    let updates: UpdateController
    @ObservationIgnored let hotkeys: HotkeyController

    @ObservationIgnored var openDashboard: ((Section?) -> Void)?
    @ObservationIgnored var openOnboarding: (() -> Void)?

    init() {
        settings = SettingsStore()
        history = HistoryStore()
        permissions = Permissions()
        models = ModelLibrary()
        dictation = DictationController(settings: settings, models: models, history: history, permissions: permissions)
        hostStatus = HostStatus(settings: settings)
        var updateConfig = UpdateController.Configuration(appName: AppInfo.displayName, version: AppInfo.version, build: AppInfo.build)
        #if DEBUG
        updateConfig.feedOverride = UpdateController.Configuration.debugFeedOverride()
        #endif
        updates = UpdateController(configuration: updateConfig)
        hotkeys = HotkeyController()
        dictation.hotkeys = hotkeys

        // Carry an imported Tauri shortcut over once (KeyboardShortcuts owns it afterwards).
        if settings.importedFromTauri { HotkeyController.adoptAccelerator(settings.settings.recordingShortcut) }

        hotkeys.onPress = { [weak self] in self?.dictation.hotkeyPressed() }
        hotkeys.onRelease = { [weak self] in self?.dictation.hotkeyReleased() }
        hotkeys.onCancel = { [weak self] in self?.dictation.cancel() }
        hotkeys.onFnPress = { [weak self] in self?.dictation.fnPressed() }
        hotkeys.onFnRelease = { [weak self] in self?.dictation.fnReleased() }
    }

    /// Launch-time work: warm the engine so the first dictation is instant, start Fn if enabled.
    func startServices() {
        if settings.settings.transcriptionLocation != .remoteHost || FluidAudioEngine.isInstalled(settings.settings.model) {
            models.prepare(settings.settings.model)
        }
        applyFnSetting()
        if settings.settings.transcriptionLocation == .remoteHost { hostStatus.refresh() }
        #if DEBUG
        // -FAIRSPOKEN_UPDATE_STATE available|checking|downloading|ready|error|idle shows that state
        // without Sparkle (README › Updates).
        if updates.simulateFromDefaults() { return }
        #endif
        updates.start()
    }

    func applyFnSetting() {
        if settings.settings.fnPushToTalk {
            permissions.refresh()
            hotkeys.setFnEnabled(true)
        } else {
            hotkeys.setFnEnabled(false)
        }
    }

    /// Keep `recordingShortcut` in settings.json meaningful for the shared schema.
    func mirrorShortcut() {
        if let accelerator = HotkeyController.currentAccelerator(), accelerator != settings.settings.recordingShortcut {
            settings.update { $0.recordingShortcut = accelerator }
        }
    }

    var needsOnboarding: Bool {
        !UserDefaults.standard.bool(forKey: "onboardingCompleted")
            || !permissions.microphone.isGranted
    }

    func completeOnboarding() {
        UserDefaults.standard.set(true, forKey: "onboardingCompleted")
    }

    /// Engine summary used across the UI.
    var engineSummary: (title: String, detail: String, placement: ComputePlacement) {
        if settings.settings.transcriptionLocation == .remoteHost {
            let host = URL(string: settings.settings.remoteUrl)?.host() ?? "your host"
            return ("Your transcription host", host, .remoteHost)
        }
        return (models.activeModel.name, "Apple Neural Engine · on this Mac", .neuralEngine)
    }
}
