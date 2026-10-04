import MultiVoiceCore
import FairspokenHost
import AppKit

// `--headless` serves with no UI at all (LaunchAgent / LaunchDaemon); otherwise an AppKit app
// with a menu bar item and a window, like the client.
if ServerInfo.isHeadless {
    Task.detached {
        let code = await HeadlessServer.run()
        exit(code)
    }
    dispatchMain()
} else {
    MainActor.assumeIsolated {
        let app = NSApplication.shared
        let delegate = ServerAppDelegate()
        app.delegate = delegate
        withExtendedLifetime(delegate) {
            app.run()
        }
    }
}
