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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
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
import ie.fairspoken.mobile.ui.Glyph
import ie.fairspoken.mobile.ui.Ink
import ie.fairspoken.mobile.ui.Type
import ie.fairspoken.mobile.ui.checkGlyph
import ie.fairspoken.mobile.ui.micGlyph
import kotlinx.coroutines.delay

/**
 * What the dock shows. The frosted capsule itself is the window's blurred
 * background; this draws the sheen and the content on top.
 */
@Composable
fun DockContent(app: FairspokenApp, dock: DockWindow) {
    val owns by dock.ownsSession.collectAsState()
    val dictation by app.dictation.state.collectAsState()
    val state = if (owns) dictation else DictationState.Idle
    val note = dock.message.value

    Row(
        Modifier.height(56.dp).sheen(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        when {
            note != null -> {
                Box(Modifier.size(56.dp), contentAlignment = Alignment.Center) {
                    Canvas(Modifier.size(24.dp)) {
                        if (note.ok) checkGlyph(Ink.ok) else micGlyph(Ink.warn)
                    }
                }
                BasicText(note.text, style = Type.label, maxLines = 1, modifier = Modifier.padding(end = 20.dp))
            }
            state is DictationState.Listening -> {
                Box(Modifier.size(56.dp), contentAlignment = Alignment.Center) {
                    Canvas(Modifier.size(34.dp)) {
                        drawCircle(Ink.record)
                        val s = size.minDimension * 0.34f
                        drawRoundRect(
                            Color.White,
                            topLeft = Offset((size.width - s) / 2, (size.height - s) / 2),
                            size = Size(s, s),
                            cornerRadius = CornerRadius(s * 0.25f),
                        )
                    }
                }
                Waveform(dock.levels)
                Spacer(Modifier.width(10.dp))
                Elapsed(state.startedAt)
                Spacer(Modifier.width(18.dp))
            }
            state is DictationState.Transcribing -> Box(Modifier.size(56.dp), contentAlignment = Alignment.Center) {
                Glyph(state, Modifier.size(24.dp))
            }
            else -> Box(Modifier.size(56.dp), contentAlignment = Alignment.Center) {
                Canvas(Modifier.size(26.dp)) { micGlyph(Ink.primary) }
            }
        }
    }
}

@Composable
private fun Waveform(levels: List<Float>) {
    Canvas(Modifier.width(78.dp).height(28.dp)) {
        val gap = size.width / levels.size
        val bar = gap * 0.55f
        levels.forEachIndexed { i, level ->
            val h = (size.height * (0.12f + level * 0.88f)).coerceAtMost(size.height)
            drawRoundRect(
                Ink.primary.copy(alpha = 0.45f + level * 0.55f),
                topLeft = Offset(i * gap + (gap - bar) / 2, (size.height - h) / 2),
                size = Size(bar, h),
                cornerRadius = CornerRadius(bar / 2),
            )
        }
    }
}

@Composable
private fun Elapsed(startedAt: Long) {
    var now by remember { mutableLongStateOf(android.os.SystemClock.elapsedRealtime()) }
    LaunchedEffect(startedAt) {
        while (true) {
            now = android.os.SystemClock.elapsedRealtime()
            delay(250)
        }
    }
    val seconds = ((now - startedAt) / 1000).coerceAtLeast(0)
    BasicText(
        "%d:%02d".format(seconds / 60, seconds % 60),
        style = Type.label.copy(fontFeatureSettings = "tnum"),
    )
}

/** A thin specular rim and top light, the "liquid" catch on the glass. */
private fun Modifier.sheen(): Modifier = drawWithContent {
    val r = CornerRadius(size.height / 2)
    drawRoundRect(
        Brush.verticalGradient(listOf(Color.White.copy(alpha = 0.55f), Color.Transparent, Color.White.copy(alpha = 0.18f))),
        cornerRadius = r,
    )
    // A faint graphite edge keeps the bubble visible over white pages.
    drawRoundRect(Ink.primary.copy(alpha = 0.14f), cornerRadius = r, style = Stroke(0.8.dp.toPx()))
    drawRoundRect(
        Brush.linearGradient(
            listOf(Color.White.copy(alpha = 0.55f), Color.White.copy(alpha = 0.06f), Color.White.copy(alpha = 0.30f)),
            start = Offset.Zero,
            end = Offset(size.width, size.height),
        ),
        cornerRadius = r,
        style = Stroke(1.2.dp.toPx()),
    )
    drawContent()
}
