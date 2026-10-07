import FairspokenUI
import SwiftUI

/// Crystal layout constants for the client (design/crystal/README.md): 20 for cards, 12 for
/// rows and fields, one rhythm of spacing.
enum Layout {
    static let card: CGFloat = 20
    static let row: CGFloat = 12
    static let gap: CGFloat = 16
    static let pageH: CGFloat = 32
    static let pageTop: CGFloat = 24
}

/// A screen title: semibold, slightly tight, with at most one quiet line under it.
struct PageTitle<Trailing: View>: View {
    var title: String
    var subtitle: String?
    @ViewBuilder var trailing: Trailing

    init(_ title: String, subtitle: String? = nil, @ViewBuilder trailing: () -> Trailing = { EmptyView() }) {
        self.title = title
        self.subtitle = subtitle
        self.trailing = trailing()
    }

    var body: some View {
        HStack(alignment: .lastTextBaseline) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 28, weight: .semibold))
                    .tracking(-0.4)
                    .foregroundStyle(Crystal.ink)
                if let subtitle {
                    Text(subtitle).font(.callout).foregroundStyle(Crystal.ink2)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 12)
            trailing
        }
    }
}

extension View {
    /// The flat fill for things that sit inside a glass card (glass can't sit on glass).
    func well(cornerRadius: CGFloat = Layout.row) -> some View {
        background(Crystal.well, in: .rect(cornerRadius: cornerRadius))
    }

    /// A secondary button inside a glass card: a flat capsule, so it isn't glass on glass.
    func cardButton() -> some View {
        buttonStyle(.bordered).buttonBorderShape(.capsule)
    }

    /// A small, title-style card heading.
    func cardHeading() -> some View {
        font(.headline).foregroundStyle(Crystal.ink)
    }
}

extension ModelLibrary.EngineState {
    var tint: Color {
        if case .failed = self { return Crystal.error }
        return isReady ? Crystal.ok : isBusy ? Crystal.warn : Crystal.ink3
    }
}

extension HostStatus {
    /// "Connected", "Checking…", "Not reachable", "Not set".
    var stateLabel: String {
        switch state {
        case .notConfigured: "Not set"
        case .checking: "Checking…"
        case .connected: "Connected"
        case .failed: "Not reachable"
        }
    }

    var tint: Color {
        switch state {
        case .connected: Crystal.ok
        case .checking: Crystal.warn
        case .failed: Crystal.error
        case .notConfigured: Crystal.ink3
        }
    }
}
