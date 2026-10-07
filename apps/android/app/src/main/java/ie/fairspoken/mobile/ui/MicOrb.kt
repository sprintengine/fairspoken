package ie.fairspoken.mobile.ui

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.kyant.backdrop.Backdrop
import com.kyant.shapes.Capsule
import ie.fairspoken.mobile.core.DictationState

/** The big glass microphone: idle, listening (glows with your voice), transcribing, done. */
@Composable
fun MicOrb(
    state: DictationState,
    level: Float,
    backdrop: Backdrop,
    size: Dp,
    modifier: Modifier = Modifier,
    onPress: () -> Unit,
) {
    val listening = state is DictationState.Listening
    val glow by animateFloatAsState(
        if (listening) 0.35f + level * 0.65f else 0f,
        spring(dampingRatio = 0.7f, stiffness = 380f),
        label = "glow",
    )
    val label = when (state) {
        is DictationState.Listening -> "Stop dictation"
        is DictationState.Transcribing -> "Transcribing"
        else -> "Start dictation"
    }
    Box(
        modifier
            .size(size)
            .drawBehind {
                if (glow > 0f) {
                    val radius = this.size.minDimension * (0.55f + glow * 0.35f)
                    drawCircle(
                        Brush.radialGradient(
                            listOf(Ink.record.copy(alpha = 0.40f * glow), Color.Transparent),
                            center,
                            radius,
                        ),
                        radius,
                    )
                }
            }
            .liquidGlass(
                backdrop,
                Capsule(),
                tint = if (listening) Ink.record.copy(alpha = 0.14f + glow * 0.18f) else Color.White.copy(alpha = 0.45f),
                blurRadius = 10.dp,
                refraction = size * 0.18f,
            )
            .clickable(remember { MutableInteractionSource() }, null, role = Role.Button, onClick = onPress)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Glyph(state, Modifier.size(size * 0.42f))
    }
}

@Composable
fun Glyph(state: DictationState, modifier: Modifier) {
    when (state) {
        is DictationState.Transcribing -> {
            val turn by rememberInfiniteTransition(label = "spin").animateFloat(
                0f, 360f, infiniteRepeatable(tween(900, easing = LinearEasing), RepeatMode.Restart), label = "turn",
            )
            Canvas(modifier) { spinner(Ink.accent, turn) }
        }
        is DictationState.Listening -> Canvas(modifier) { stopGlyph(Ink.primary) }
        is DictationState.Done -> Canvas(modifier) { checkGlyph(Ink.ok) }
        is DictationState.Failed -> Canvas(modifier.fillMaxSize()) { micGlyph(Ink.warn) }
        DictationState.Idle -> Canvas(modifier) { micGlyph(Ink.primary) }
    }
}
