import AppKit
import FairspokenCore
import SwiftUI

/// The model source control both apps show: a field (empty means Hugging Face), Choose Folder…
/// and Test, with the outcome inline underneath. `commit` runs on Return, when the field loses
/// focus, after a folder is chosen and before a test, and returns a problem to show (or nil);
/// `test` checks the source can serve the models in use, without downloading them.
public struct ModelSourceField: View {
    @Binding var text: String
    var commit: (String) -> String?
    var test: (String) async -> ModelSourceCheck
    @State private var result: ModelSourceCheck?
    @State private var testing = false
    @FocusState private var focused: Bool

    public init(text: Binding<String>, commit: @escaping (String) -> String?, test: @escaping (String) async -> ModelSourceCheck) {
        _text = text
        self.commit = commit
        self.test = test
    }

    public var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            HStack(spacing: 8) {
                TextField("Model source", text: $text, prompt: Text("Hugging Face"))
                    .labelsHidden()
                    .font(.fsData(.body))
                    .frame(minWidth: 200)
                    .focused($focused)
                    .onSubmit { _ = apply() }
                Button("Choose Folder…", action: chooseFolder)
                Button(testing ? "Testing…" : "Test", action: runTest)
                    .disabled(testing)
            }
            if let result {
                Label(result.message, systemImage: result.ok ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(result.ok ? Crystal.ok : Crystal.error)
                    .multilineTextAlignment(.leading)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .onChange(of: focused) { _, now in if !now { _ = apply() } }
    }

    /// Commits the text; shows the problem, if any. True when it was accepted.
    private func apply() -> Bool {
        if let problem = commit(text) {
            result = ModelSourceCheck(ok: false, message: problem)
            return false
        }
        if result?.ok == false { result = nil }
        return true
    }

    private func runTest() {
        guard apply() else { return }
        testing = true
        result = nil
        let value = text
        Task {
            let outcome = await test(value)
            testing = false
            result = outcome
        }
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = false
        panel.prompt = "Choose"
        panel.message = "Choose the folder that holds the models, each as owner/repo-name (FluidInference/parakeet-tdt-0.6b-v3-coreml)."
        if case .folder(let current)? = try? ModelSource.parse(text) { panel.directoryURL = current }
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            var path = url.path(percentEncoded: false)
            while path.count > 1, path.hasSuffix("/") { path.removeLast() }
            text = path
            result = nil
            _ = apply()
        }
    }
}
