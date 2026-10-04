import MultiVoiceCore
import FairspokenHost
import Foundation
import IOKit.pwr_mgt

/// Keeps the Mac from idle-sleeping while the server is serving (the display may still sleep).
/// The assertion is visible in `pmset -g assertions` under the app's name.
nonisolated final class SleepGuard: @unchecked Sendable {
    private var assertion: IOPMAssertionID = 0
    private let lock = NSLock()

    var isHeld: Bool { lock.withLock { assertion != 0 } }

    func hold(reason: String) {
        lock.withLock {
            guard assertion == 0 else { return }
            var id: IOPMAssertionID = 0
            let result = IOPMAssertionCreateWithName(kIOPMAssertionTypePreventUserIdleSystemSleep as CFString,
                                                     IOPMAssertionLevel(kIOPMAssertionLevelOn), reason as CFString, &id)
            if result == kIOReturnSuccess { assertion = id }
        }
    }

    func release() {
        lock.withLock {
            guard assertion != 0 else { return }
            IOPMAssertionRelease(assertion)
            assertion = 0
        }
    }

    deinit { release() }
}
