import AppKit
import FairspokenCore
import SwiftUI
import UniformTypeIdentifiers

/// "Download from Link…" on a model card, both apps: a field for an archive link or path,
/// Choose File… and Download, with any problem inline. `submit` saves the link and starts the
/// download, returning why the text can't be a link (or nil).
public struct ModelLinkEditor: View {
    @Binding var text: String
    var tint: Color
    var submit: (String) -> String?
    var cancel: () -> Void
    @State private var problem: String?
    @FocusState private var focused: Bool

    public static let prompt = "https://… or a path to a .zip"
    public static let caption = "A link to a .zip of this model, for example from your organisation's server."

    public init(text: Binding<String>, tint: Color, submit: @escaping (String) -> String?, cancel: @escaping () -> Void) {
        _text = text
        self.tint = tint
        self.submit = submit
        self.cancel = cancel
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Download link", text: $text, prompt: Text(Self.prompt))
                .labelsHidden()
                .textFieldStyle(.roundedBorder)
                .font(.fsData(.callout))
                .focused($focused)
                .onSubmit(download)
                .onChange(of: text) { problem = nil }
            // One row when it fits; on a narrow card, Choose File… above Cancel and Download.
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 8) {
                    chooseButton
                    Spacer(minLength: 8)
                    cancelButton
                    downloadButton
                }
                VStack(alignment: .leading, spacing: 8) {
                    chooseButton
                    HStack(spacing: 8) {
                        Spacer(minLength: 0)
                        cancelButton
                        downloadButton
                    }
                }
            }
            if let problem {
                Label(problem, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption).foregroundStyle(Crystal.error)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            Text(Self.caption)
                .font(.caption).foregroundStyle(Crystal.ink3)
                .fixedSize(horizontal: false, vertical: true)
        }
        .onAppear { focused = true }
    }

    private var chooseButton: some View {
        Button("Choose File…", action: chooseFile)
            .buttonStyle(.bordered).buttonBorderShape(.capsule)
            .fixedSize()
    }

    private var cancelButton: some View {
        Button("Cancel", action: cancel)
            .buttonStyle(.bordered).buttonBorderShape(.capsule)
            .fixedSize()
    }

    private var downloadButton: some View {
        Button("Download", action: download)
            .buttonStyle(.borderedProminent).buttonBorderShape(.capsule)
            .tint(tint)
            .fixedSize()
            .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }

    private func download() {
        problem = submit(text)
    }

    private func chooseFile() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.zip, .gzip, .archive]
        panel.prompt = "Choose"
        panel.message = "Choose a .zip (or .tar.gz) of the model."
        if let link = try? ModelLink.parse(text), link.isFile { panel.directoryURL = link.url.deletingLastPathComponent() }
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            text = url.path(percentEncoded: false)
            problem = nil
        }
    }
}

/// A saved link on a model card: where the model downloads from (host and path, never a query
/// string; the full address without its query on hover), and an optional note under it.
public struct ModelLinkSummary: View {
    var link: String
    var note: String?

    public init(link: String, note: String? = nil) {
        self.link = link
        self.note = note
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Label {
                Text("Downloads from \(ModelLink.shortDisplay(link))")
                    .lineLimit(2).truncationMode(.middle)
                    .fixedSize(horizontal: false, vertical: true)
            } icon: {
                Image(systemName: "link")
            }
            .font(.caption).foregroundStyle(Crystal.ink2)
            .help(ModelLink.display(link))
            if let note {
                Text(note).font(.caption).foregroundStyle(Crystal.warn)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}
