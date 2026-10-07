package ie.fairspoken.mobile.dock

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.BasicText
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.unit.dp
import ie.fairspoken.mobile.FairspokenApp
import ie.fairspoken.mobile.core.DictationState
import ie.fairspoken.mobile.ui.Crystal
import ie.fairspoken.mobile.ui.CrystalColors
import ie.fairspoken.mobile.ui.CrystalTheme
import ie.fairspoken.mobile.ui.Elapsed
import ie.fairspoken.mobile.ui.Spinner
import ie.fairspoken.mobile.ui.Type
import ie.fairspoken.mobile.ui.checkGlyph
import ie.fairspoken.mobile.ui.micGlyph

/**
 * What the dock shows. The frosted capsule itself is the window's blurred
 * background; this draws the sheen and the content on top.
 */
@Composable
fun DockContent(app: FairspokenApp, dock: DockWindow) = CrystalTheme {
    val c = Crystal.colors
    val owns by dock.ownsSession.collectAsState()
    val dictation by app.dictation.state.collectAsState()
    val state = if (owns) dictation else DictationState.Idle
    val note = dock.message.value

    Row(
        Modifier.height(56.dp).sheen(c),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        when {
            note != null -> {
                Box(Modifier.size(56.dp), contentAlignment = Alignment.Center) {
                    Canvas(Modifier.size(24.dp)) {
                        if (note.ok) checkGlyph(c.ok) else micGlyph(c.error)
                    }
                }
                BasicText(
                    note.text,
                    style = Type.label.copy(color = if (note.ok) c.ink else c.error),
                    maxLines = 1,
                    modifier = Modifier.padding(end = 20.dp),
                )
            }
            state is DictationState.Listening -> {
                Box(Modifier.size(56.dp), contentAlignment = Alignment.Center) {
                    Canvas(Modifier.size(34.dp)) {
                        drawCircle(c.live)
                        val s = size.minDimension * 0.34f
                        drawRoundRect(
                            Color.White,
                            topLeft = Offset((size.width - s) / 2, (size.height - s) / 2),
                            size = Size(s, s),
                            cornerRadius = CornerRadius(s * 0.25f),
                        )
                    }
                }
                Waveform(dock.levels, c.ink)
                Spacer(Modifier.width(10.dp))
                Elapsed(state.startedAt)
                Spacer(Modifier.width(18.dp))
            }
            state is DictationState.Transcribing -> Box(Modifier.size(56.dp), contentAlignment = Alignment.Center) {
                Spinner(c.accent, Modifier.size(24.dp))
            }
            else -> Box(Modifier.size(56.dp), contentAlignment = Alignment.Center) {
                Canvas(Modifier.size(26.dp)) { micGlyph(c.ink) }
            }
        }
    }
}

@Composable
private fun Waveform(levels: List<Float>, ink: Color) {
    Canvas(Modifier.width(78.dp).height(28.dp)) {
        val gap = size.width / levels.size
        val bar = gap * 0.55f
        levels.forEachIndexed { i, level ->
            val h = (size.height * (0.12f + level * 0.88f)).coerceAtMost(size.height)
            drawRoundRect(
                ink.copy(alpha = 0.40f + level * 0.60f),
                topLeft = Offset(i * gap + (gap - bar) / 2, (size.height - h) / 2),
                size = Size(bar, h),
                cornerRadius = CornerRadius(bar / 2),
            )
        }
    }
}

/** A thin specular rim and top light, the "liquid" catch on the glass. */
private fun Modifier.sheen(c: CrystalColors): Modifier = drawWithContent {
    val r = CornerRadius(size.height / 2)
    val light = if (c.dark) 0.10f else 0.55f
    drawRoundRect(
        Brush.verticalGradient(listOf(Color.White.copy(alpha = light), Color.Transparent, Color.White.copy(alpha = light * 0.3f))),
        cornerRadius = r,
    )
    // A faint edge keeps the bubble visible over pages of its own colour.
    drawRoundRect(if (c.dark) Color.White.copy(alpha = 0.14f) else c.ink.copy(alpha = 0.12f), cornerRadius = r, style = Stroke(0.8.dp.toPx()))
    drawRoundRect(
        Brush.linearGradient(
            listOf(c.edge, Color.White.copy(alpha = 0.04f), c.edge.copy(alpha = c.edge.alpha * 0.4f)),
            start = Offset.Zero,
            end = Offset(size.width, size.height),
        ),
        cornerRadius = r,
        style = Stroke(1.2.dp.toPx()),
    )
    drawContent()
}
