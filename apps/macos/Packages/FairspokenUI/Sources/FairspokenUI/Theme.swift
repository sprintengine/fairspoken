import AppKit
import SwiftUI

/// Brand tokens for the dictation client (teal accent, indigo for host things, coral for a
/// live microphone). Fairspoken Server uses the brand book's palette directly (`Color.fs*`).
public enum Brand {
    public static var accent: Color { .mvTeal }
    public static var secondary: Color { .mvIndigo }
    public static var live: Color { .mvCoral }
}

extension Color {
    // Client palette.
    public static let mvTeal = Color(light: NSColor(srgbRed: 0.04, green: 0.49, blue: 0.50, alpha: 1),
                                     dark: NSColor(srgbRed: 0.27, green: 0.82, blue: 0.79, alpha: 1))
    public static let mvIndigo = Color(light: NSColor(srgbRed: 0.33, green: 0.32, blue: 0.84, alpha: 1),
                                       dark: NSColor(srgbRed: 0.58, green: 0.57, blue: 1.0, alpha: 1))
    public static let mvCoral = Color(light: NSColor(srgbRed: 0.93, green: 0.30, blue: 0.24, alpha: 1),
                                      dark: NSColor(srgbRed: 1.0, green: 0.42, blue: 0.36, alpha: 1))
    public static let mvAmber = Color(light: NSColor(srgbRed: 0.85, green: 0.55, blue: 0.05, alpha: 1),
                                      dark: NSColor(srgbRed: 1.0, green: 0.74, blue: 0.30, alpha: 1))
    public static let mvGreen = Color(light: NSColor(srgbRed: 0.13, green: 0.60, blue: 0.33, alpha: 1),
                                      dark: NSColor(srgbRed: 0.35, green: 0.85, blue: 0.52, alpha: 1))

    // Brand book palette (design/brand/README.md §3).
    /// Copybook Blue: primary actions, links, the fair line.
    public static let fsBlue = Color(hex: 0x2C4BD0, dark: 0x93A8FF)
    /// Margin Red: the signal only (live microphone, audio in flight, the caret). Never errors.
    public static let fsRed = Color(hex: 0xE5512B, dark: 0xFF7148)
    /// Margin Red for text.
    public static let fsRedText = Color(hex: 0xB23B16, dark: 0xFF7148)
    public static let fsGorse = Color(hex: 0xF4BE4F, dark: 0xF4BE4F)
    public static let fsGorseText = Color(hex: 0x8F5A00, dark: 0xF4BE4F)
    public static let fsSuccess = Color(hex: 0x1D7347, dark: 0x5ED39A)
    public static let fsError = Color(hex: 0xB21F3B, dark: 0xFF7A8F)
    public static let fsRuling = Color(hex: 0xC5D1EC, dark: 0x2A3346)
    /// Limestone (light) / Night (dark) page background.
    public static let fsPage = Color(hex: 0xF3F1EA, dark: 0x0A0D12)
    /// Paper (light) / Night 2 (dark) raised surface.
    public static let fsSurface = Color(hex: 0xFBFAF6, dark: 0x141922)
    public static let fsInk = Color(hex: 0x0E1318, dark: 0xECEEF1)
    public static let fsSlate = Color(hex: 0x5E6873, dark: 0x8B95A2)

    public init(light: NSColor, dark: NSColor) {
        self.init(nsColor: NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .vibrantDark, .accessibilityHighContrastDarkAqua]) != nil ? dark : light
        })
    }

    public init(hex: UInt32, dark: UInt32) {
        func ns(_ v: UInt32) -> NSColor {
            NSColor(srgbRed: CGFloat((v >> 16) & 0xFF) / 255, green: CGFloat((v >> 8) & 0xFF) / 255, blue: CGFloat(v & 0xFF) / 255, alpha: 1)
        }
        self.init(light: ns(hex), dark: ns(dark))
    }
}

extension Font {
    /// Big rounded numerals for stats.
    public static func mvNumber(_ size: CGFloat, weight: Font.Weight = .semibold) -> Font {
        .system(size: size, weight: weight, design: .rounded).monospacedDigit()
    }

    /// Measured data (timings, addresses): the brand sets data in mono.
    public static func fsData(_ style: Font.TextStyle = .callout, weight: Font.Weight = .regular) -> Font {
        .system(style, design: .monospaced).weight(weight)
    }
}

/// Soft, static colour field behind a window so Liquid Glass has something to refract.
/// Drawn once (no animation) to keep idle CPU at zero.
public struct AuroraBackground: View {
    @Environment(\.colorScheme) private var scheme
    public var accent: Color
    public var secondary: Color
    public var base: Color?

    public init(accent: Color = .mvTeal, secondary: Color = .mvIndigo, base: Color? = nil) {
        self.accent = accent
        self.secondary = secondary
        self.base = base
    }

    public var body: some View {
        GeometryReader { geo in
            let w = geo.size.width, h = geo.size.height
            ZStack {
                base ?? (scheme == .dark ? Color(white: 0.075) : Color(white: 0.965))
                Circle().fill(accent.opacity(scheme == .dark ? 0.30 : 0.22))
                    .frame(width: w * 0.75, height: w * 0.75)
                    .position(x: w * 0.18, y: h * 0.05)
                    .blur(radius: 90)
                Circle().fill(secondary.opacity(scheme == .dark ? 0.26 : 0.16))
                    .frame(width: w * 0.6, height: w * 0.6)
                    .position(x: w * 0.95, y: h * 0.35)
                    .blur(radius: 100)
                Circle().fill(accent.opacity(scheme == .dark ? 0.16 : 0.10))
                    .frame(width: w * 0.7, height: w * 0.5)
                    .position(x: w * 0.55, y: h * 1.05)
                    .blur(radius: 110)
            }
            .drawingGroup()
        }
        .ignoresSafeArea()
        .accessibilityHidden(true)
    }
}
