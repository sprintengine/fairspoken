import AppKit
import FairspokenCore
import Foundation

/// The port the server wanted, and who holds it, when a start failed with "address in use".
struct PortConflict: Equatable {
    var port: Int
    /// The app or program listening there ("Fairspoken", "node"), if `lsof` could tell.
    var owner: String?

    var message: String {
        let holder = owner.map { "\($0) is already using port \(port)" } ?? "Another app is already using port \(port)"
        return "Can't start: \(holder). Quit it, or choose another port."
    }
}

/// Names the process listening on a TCP port (best effort, via `lsof`).
enum PortOwner {
    static func name(listeningOn port: Int, runner: any CommandRunning = ProcessCommandRunner()) async -> String? {
        guard let output = try? await runner.run("/usr/sbin/lsof", arguments: ["-nP", "+c", "0", "-iTCP:\(port)", "-sTCP:LISTEN", "-Fpc"],
                                                 environment: [:], timeout: .seconds(3)) else { return nil }
        let owners = parse(String(decoding: output.stdout, as: UTF8.self))
        let me = ProcessInfo.processInfo.processIdentifier
        guard let owner = owners.first(where: { $0.pid != me }) ?? owners.first else { return nil }
        // An app's own name ("Fairspoken Server") reads better than its executable's.
        return NSRunningApplication(processIdentifier: owner.pid)?.localizedName ?? owner.command
    }

    /// `lsof -F pc` output: a `p<pid>` line, then a `c<command>` line, per process.
    static func parse(_ output: String) -> [(pid: pid_t, command: String)] {
        var result: [(pid: pid_t, command: String)] = []
        var pid: pid_t?
        for line in output.split(separator: "\n") {
            switch line.first {
            case "p": pid = pid_t(line.dropFirst())
            case "c": if let p = pid { result.append((p, String(line.dropFirst()))); pid = nil }
            default: break
            }
        }
        return result
    }
}
