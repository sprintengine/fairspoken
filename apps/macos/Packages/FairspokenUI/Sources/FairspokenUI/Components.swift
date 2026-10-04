import MultiVoiceCore
import SwiftUI

/// A Liquid Glass panel. Content cards share one `GlassEffectContainer` per screen so
/// they sample the same backdrop (glass can't sample glass) and render efficiently.
public struct GlassCard<Content: View>: View {
    var cornerRadius: CGFloat = 24
    var padding: CGFloat = 20
    var tint: Color? = nil
    var fillHeight = false
    @ViewBuilder var content: Content

    public init(cornerRadius: CGFloat = 24, padding: CGFloat = 20, tint: Color? = nil, fillHeight: Bool = false, @ViewBuilder content: () -> Content) {
        self.cornerRadius = cornerRadius
        self.padding = padding
        self.tint = tint
        self.fillHeight = fillHeight
        self.content = content()
    }
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    public var body: some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, maxHeight: fillHeight ? .infinity : nil, alignment: .topLeading)
            .glassEffect(glass, in: .rect(cornerRadius: cornerRadius))
    }

    private var glass: Glass {
        var g: Glass = .regular
        if let tint { g = g.tint(tint) }
        return reduceTransparency ? .identity : g
    }
}

/// Small uppercase label above a value.
public struct Eyebrow: View {
    var text: String
    var symbol: String? = nil
    public init(text: String, symbol: String? = nil) { self.text = text; self.symbol = symbol }
    public var body: some View {
        HStack(spacing: 5) {
            if let symbol { Image(systemName: symbol).imageScale(.small) }
            Text(text.uppercased())
        }
        .font(.caption.weight(.semibold))
        .tracking(0.6)
        .foregroundStyle(.secondary)
    }
}

/// A keyboard key cap: `⌘` `⇧` `1`.
public struct Keycap: View {
    var label: String
    public init(label: String) { self.label = label }
    public var body: some View {
        Text(label)
            .font(.system(.callout, design: .rounded).weight(.semibold))
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .frame(minWidth: 26)
            .background(.quaternary.opacity(0.6), in: .rect(cornerRadius: 7))
            .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(.primary.opacity(0.12)))
            .accessibilityLabel(label)
    }
}

/// Splits a KeyboardShortcuts description like "⇧⌘1" into individual caps.
public struct ShortcutCaps: View {
    var description: String
    public init(description: String) { self.description = description }
    public var body: some View {
        HStack(spacing: 4) {
            ForEach(Array(Self.split(description).enumerated()), id: \.offset) { _, part in Keycap(label: part) }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Shortcut \(description)")
    }

    public static func split(_ s: String) -> [String] {
        let mods: Set<Character> = ["⌃", "⌥", "⇧", "⌘"]
        var parts: [String] = []
        var rest = ""
        for ch in s {
            if mods.contains(ch) && rest.isEmpty { parts.append(String(ch)) } else { rest.append(ch) }
        }
        if !rest.isEmpty { parts.append(rest) }
        return parts
    }
}

public struct StatusDot: View {
    var color: Color
    var pulsing = false
    public init(color: Color, pulsing: Bool = false) { self.color = color; self.pulsing = pulsing }
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var on = false

    public var body: some View {
        Circle()
            .fill(color)
            .frame(width: 8, height: 8)
            .overlay {
                if pulsing && !reduceMotion {
                    Circle().stroke(color.opacity(0.6), lineWidth: 2)
                        .scaleEffect(on ? 2.4 : 1)
                        .opacity(on ? 0 : 1)
                        .animation(.easeOut(duration: 1.2).repeatForever(autoreverses: false), value: on)
                        .onAppear { on = true }
                }
            }
            .accessibilityHidden(true)
    }
}

/// Capsule chip, glass by default.
public struct Chip: View {
    var text: String
    var symbol: String? = nil
    var tint: Color? = nil
    var dot: Color? = nil
    public init(text: String, symbol: String? = nil, tint: Color? = nil, dot: Color? = nil) {
        self.text = text; self.symbol = symbol; self.tint = tint; self.dot = dot
    }

    public var body: some View {
        HStack(spacing: 6) {
            if let dot { StatusDot(color: dot) }
            if let symbol { Image(systemName: symbol).imageScale(.small).foregroundStyle(tint ?? .secondary) }
            Text(text).lineLimit(1)
        }
        .font(.callout.weight(.medium))
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .glassEffect(.regular, in: .capsule)
    }
}

/// Small flat tag used inside cards (no glass-on-glass).
public struct Tag: View {
    var text: String
    var symbol: String? = nil
    var color: Color = .secondary
    public init(text: String, symbol: String? = nil, color: Color = .secondary) { self.text = text; self.symbol = symbol; self.color = color }

    public var body: some View {
        HStack(spacing: 4) {
            if let symbol { Image(systemName: symbol).imageScale(.small) }
            Text(text).lineLimit(1)
        }
        .font(.caption.weight(.semibold))
        .foregroundStyle(color)
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(color.opacity(0.12), in: .capsule)
    }
}

/// Brand mark: placeholder until naming settles. Built from SF Symbols so it scales.
public struct BrandMark: View {
    var size: CGFloat = 28
    public init(size: CGFloat = 28) { self.size = size }
    public var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
                .fill(LinearGradient(colors: [Color.mvTeal, Color.mvTeal.mix(with: .mvIndigo, by: 0.55)],
                                     startPoint: .topLeading, endPoint: .bottomTrailing))
            Image(systemName: "waveform")
                .font(.system(size: size * 0.52, weight: .bold))
                .foregroundStyle(.white)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// 1…5 dot meter for model accuracy/speed hints.
public struct DotMeter: View {
    var value: Int
    var color: Color
    public init(value: Int, color: Color) { self.value = value; self.color = color }
    public var body: some View {
        HStack(spacing: 3) {
            ForEach(0..<5, id: \.self) { i in
                Capsule().fill(i < value ? color : Color.primary.opacity(0.12)).frame(width: 12, height: 5)
            }
        }
        .accessibilityElement()
        .accessibilityValue("\(value) of 5")
    }
}

extension ComputePlacement {
    public var tint: Color { self == .remoteHost ? .mvIndigo : .mvTeal }
}

public enum Format {
    public static func bytes(_ value: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: value, countStyle: .file)
    }

    public static func ms(_ value: Double?) -> String {
        guard let value else { return "—" }
        return value >= 1000 ? String(format: "%.1f s", value / 1000) : "\(Int(value.rounded())) ms"
    }

    public static func number(_ n: Int) -> String { n.formatted(.number) }

    public static func duration(_ seconds: Double) -> String {
        let s = Int(seconds)
        if s >= 3600 { return "\(s / 3600)h \(s % 3600 / 60)m" }
        if s >= 60 { return "\(s / 60)m \(s % 60)s" }
        return "\(s)s"
    }

    public static func relative(_ date: Date) -> String {
        let delta = Date().timeIntervalSince(date)
        if delta < 60 { return "now" }
        if Calendar.current.isDateInToday(date) { return date.formatted(date: .omitted, time: .shortened) }
        if Calendar.current.isDateInYesterday(date) { return "Yesterday" }
        return date.formatted(.dateTime.weekday(.abbreviated))
    }
}

/// The Fairspoken mark ("the Settle"): a spoken wave settling into one straight line,
/// stopped by the red caret. Paths from design/brand/logo/fairspoken-mark.svg (64-unit grid).
public struct FairspokenMark: View {
    var size: CGFloat
    var monochrome: Color?
    public init(size: CGFloat = 28, monochrome: Color? = nil) {
        self.size = size
        self.monochrome = monochrome
    }

    static let wave = "M7 32 C7.11 31.29 7.46 29.08 7.69 27.77 C7.92 26.46 8.15 25.2 8.38 24.13 C8.6 23.07 8.83 22.11 9.06 21.37 C9.29 20.63 9.52 20.05 9.75 19.68 C9.98 19.3 10.21 19.13 10.44 19.13 C10.67 19.14 10.9 19.35 11.13 19.71 C11.35 20.08 11.58 20.64 11.81 21.31 C12.04 21.97 12.27 22.82 12.5 23.71 C12.73 24.6 12.96 25.63 13.19 26.66 C13.42 27.68 13.65 28.8 13.88 29.87 C14.1 30.93 14.33 32.03 14.56 33.03 C14.79 34.04 15.02 35.03 15.25 35.89 C15.48 36.76 15.71 37.56 15.94 38.21 C16.17 38.87 16.4 39.42 16.63 39.83 C16.85 40.24 17.08 40.51 17.31 40.66 C17.54 40.8 17.77 40.79 18 40.68 C18.23 40.56 18.46 40.3 18.69 39.95 C18.92 39.61 19.15 39.14 19.38 38.61 C19.6 38.09 19.83 37.46 20.06 36.82 C20.29 36.18 20.52 35.46 20.75 34.77 C20.98 34.08 21.21 33.35 21.44 32.68 C21.67 32 21.9 31.32 22.13 30.72 C22.35 30.12 22.58 29.55 22.81 29.07 C23.04 28.59 23.27 28.17 23.5 27.84 C23.73 27.51 23.96 27.26 24.19 27.1 C24.42 26.94 24.65 26.87 24.88 26.87 C25.1 26.87 25.33 26.96 25.56 27.11 C25.79 27.26 26.02 27.49 26.25 27.75 C26.48 28.01 26.71 28.34 26.94 28.68 C27.17 29.02 27.4 29.41 27.63 29.78 C27.85 30.16 28.08 30.56 28.31 30.93 C28.54 31.3 28.77 31.67 29 32 C29.23 32.33 29.46 32.64 29.69 32.91 C29.92 33.17 30.15 33.4 30.38 33.58 C30.6 33.76 30.83 33.9 31.06 33.99 C31.29 34.08 31.52 34.12 31.75 34.13 C31.98 34.14 32.21 34.1 32.44 34.04 C32.67 33.98 32.9 33.88 33.13 33.77 C33.35 33.66 33.58 33.52 33.81 33.38 C34.04 33.24 34.27 33.09 34.5 32.94 C34.73 32.8 34.96 32.65 35.19 32.53 C35.42 32.4 35.65 32.28 35.88 32.18 C36.1 32.08 36.33 31.99 36.56 31.93 C36.79 31.87 37.02 31.83 37.25 31.8 C37.48 31.77 37.71 31.77 37.94 31.77 C38.17 31.78 38.4 31.8 38.63 31.83 C38.85 31.85 39.08 31.89 39.31 31.92 C39.54 31.95 39.89 31.99 40 32 L45 32"

    /// Parses the M / C / L commands of the mark's SVG path.
    static func path(_ d: String, scale: CGFloat) -> Path {
        var path = Path()
        let tokens = d.replacingOccurrences(of: "M", with: " M ").replacingOccurrences(of: "C", with: " C ")
            .replacingOccurrences(of: "L", with: " L ").split(separator: " ").map(String.init)
        var i = 0
        var command = ""
        func number() -> CGFloat { defer { i += 1 }; return CGFloat(Double(tokens[i]) ?? 0) * scale }
        while i < tokens.count {
            if ["M", "C", "L"].contains(tokens[i]) { command = tokens[i]; i += 1; continue }
            switch command {
            case "M": path.move(to: CGPoint(x: number(), y: number()))
            case "L": path.addLine(to: CGPoint(x: number(), y: number()))
            case "C":
                let c1 = CGPoint(x: number(), y: number()), c2 = CGPoint(x: number(), y: number()), p = CGPoint(x: number(), y: number())
                path.addCurve(to: p, control1: c1, control2: c2)
            default: i += 1
            }
        }
        return path
    }

    public var body: some View {
        Canvas { ctx, canvas in
            let s = canvas.width / 64
            let style = StrokeStyle(lineWidth: 4.5 * s, lineCap: .round, lineJoin: .round)
            ctx.stroke(Self.path(Self.wave, scale: s), with: .color(monochrome ?? .fsBlue), style: style)
            var caret = Path()
            caret.move(to: CGPoint(x: 53 * s, y: 20 * s))
            caret.addLine(to: CGPoint(x: 53 * s, y: 44 * s))
            ctx.stroke(caret, with: .color(monochrome ?? .fsRed), style: style)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}
