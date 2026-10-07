import FairspokenUI
import SwiftUI

/// Live mic level as rounded bars, redrawn per frame only while `active`.
struct LevelBars: View {
    var meter: LevelMeter
    var active: Bool
    var bars = 24
    var color: Color = Crystal.live
    var minHeight: CGFloat = 3

    var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 60, paused: !active)) { _ in
            Canvas { ctx, size in
                let snap = meter.snapshot()
                let history = Array(snap.history.suffix(bars))
                let gap: CGFloat = 3
                let w = max(2, (size.width - gap * CGFloat(bars - 1)) / CGFloat(bars))
                for i in 0..<bars {
                    let v = i < history.count ? CGFloat(history[i]) : 0
                    let h = max(minHeight, v * size.height)
                    let rect = CGRect(x: CGFloat(i) * (w + gap), y: (size.height - h) / 2, width: w, height: h)
                    let fade = 0.35 + 0.65 * Double(i) / Double(max(1, bars - 1))
                    ctx.fill(Path(roundedRect: rect, cornerRadius: w / 2), with: .color(color.opacity(active ? fade : 0.25)))
                }
            }
        }
        .accessibilityLabel("Microphone level")
    }
}

