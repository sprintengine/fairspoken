package ie.fairspoken.mobile.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.spring
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.kyant.backdrop.Backdrop
import com.kyant.shapes.Capsule
import ie.fairspoken.mobile.core.DictationState
import kotlinx.coroutines.flow.collectLatest

/**
 * The big glass microphone, the one primary control wherever it appears, so
 * it alone carries the accent tint. Listening turns it `live` red and it
 * breathes with your voice. [level] is read only while drawing, so the glow
 * animates without recomposing.
 */
@Composable
fun MicOrb(
    state: DictationState,
    level: () -> Float,
    backdrop: Backdrop,
    size: Dp,
    modifier: Modifier = Modifier,
    onPress: () -> Unit,
) {
    val c = Crystal.colors
    val listening = state is DictationState.Listening
    val currentLevel by rememberUpdatedState(level)
    val glow = remember { Animatable(0f) }
    LaunchedEffect(listening) {
        val spec = spring<Float>(dampingRatio = 0.7f, stiffness = 380f)
        if (listening) {
            snapshotFlow { 0.3f + currentLevel() * 0.7f }.collectLatest { glow.animateTo(it, spec) }
        } else {
            glow.animateTo(0f, spec)
        }
    }
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    val scale by animateFloatAsState(if (pressed) 0.95f else 1f, spring(dampingRatio = 0.6f, stiffness = 600f), label = "press")
    val label = when (state) {
        is DictationState.Listening -> "Stop dictation"
        is DictationState.Transcribing -> "Transcribing"
        else -> "Start dictation"
    }
    val idleTint = c.accent.copy(alpha = if (c.dark) 0.14f else 0.10f)
    val ring = if (listening) c.live else c.accent
    val shape = remember { Capsule() }
    Box(
        modifier
            .size(size)
            .graphicsLayer {
                scaleX = scale
                scaleY = scale
            }
            .drawBehind {
                val g = glow.value
                if (g > 0f) {
                    val radius = this.size.minDimension * (0.56f + g * 0.30f)
                    drawCircle(
                        Brush.radialGradient(listOf(c.live.copy(alpha = 0.32f * g), Color.Transparent), center, radius),
                        radius,
                    )
                }
            }
            .glass(
                backdrop,
                shape,
                fill = c.surfaceStrong,
                tint = { if (listening) c.live.copy(alpha = 0.10f + glow.value * 0.14f) else idleTint },
                blurRadius = 12.dp,
                refraction = size * 0.16f,
            )
            .drawBehind {
                // A fine accent ring just inside the rim: "ready", or red while live.
                val inset = 5.dp.toPx()
                drawCircle(
                    ring.copy(alpha = if (listening) 0.55f else 0.32f),
                    radius = this.size.minDimension / 2 - inset,
                    style = Stroke(1.2.dp.toPx()),
                )
            }
            .clickable(interaction, null, role = Role.Button, onClick = onPress)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        OrbGlyph(state, Modifier.size(size * 0.36f))
    }
}

@Composable
fun OrbGlyph(state: DictationState, modifier: Modifier) {
    val c = Crystal.colors
    when (state) {
        is DictationState.Transcribing -> Spinner(c.accent, modifier)
        is DictationState.Listening -> Canvas(modifier) { stopGlyph(c.live) }
        is DictationState.Done -> Canvas(modifier) { checkGlyph(c.ok) }
        is DictationState.Failed -> Canvas(modifier) { micGlyph(c.ink2) }
        DictationState.Idle -> Canvas(modifier) { micGlyph(c.ink) }
    }
}
