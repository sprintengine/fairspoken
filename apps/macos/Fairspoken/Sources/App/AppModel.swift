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
    let hostFinder: HostFinder
    let updates: UpdateController
    @ObservationIgnored let hotkeys: HotkeyController

    @ObservationIgnored var openDashboard: ((Section?) -> Void)?
    @ObservationIgnored var openOnboarding: (() -> Void)?

    init() {
        settings = SettingsStore()
        // Before anything can download.
        ParakeetModels.setLinks(settings.settings.modelLinks)
        history = HistoryStore()
        permissions = Permissions()
        models = ModelLibrary()
        dictation = DictationController(settings: settings, models: models, history: history, permissions: permissions)
        hostStatus = HostStatus(settings: settings)
        hostFinder = HostFinder(settings: settings, hostStatus: hostStatus)
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

    /// The model card whose "Download from Link…" field is open, and what it holds.
    var linkEditorModel: String?
    var linkDraft = ""

    func openLinkEditor(for id: String) {
        linkDraft = settings.settings.modelLinks[id] ?? ""
        linkEditorModel = id
    }

    /// Saves `text` as `id`'s download link and downloads the model from it (the model in use
    /// that failed to load is loaded too). Returns why `text` can't be a link, or nil.
    func downloadFromLink(_ id: String, _ text: String) -> String? {
        let value: String
        do { value = try ModelLink.normalize(text) } catch { return error.localizedDescription }
        settings.update { $0.modelLinks[id] = value }
        ParakeetModels.setLinks(settings.settings.modelLinks)
        linkEditorModel = nil
        models.clearDownloadError(id)
        if id == models.activeModelID, case .failed = models.engineState, settings.settings.transcriptionLocation == .local {
            models.prepare(id)
        } else {
            models.download(id)
        }
        return nil
    }

    /// Back to the standard download for `id`.
    func removeModelLink(_ id: String) {
        settings.update { $0.modelLinks[id] = nil }
        ParakeetModels.setLinks(settings.settings.modelLinks)
        models.clearDownloadError(id)
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
