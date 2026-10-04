import AppKit

// AppKit lifecycle (plan §2.1): an NSStatusItem-owning delegate with SwiftUI content in
// NSPanel / NSWindow hosts, so activation policy can switch between accessory (menu bar
// only) and regular (dashboard open, appears in ⌘Tab).
MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    withExtendedLifetime(delegate) {
        app.run()
    }
}
