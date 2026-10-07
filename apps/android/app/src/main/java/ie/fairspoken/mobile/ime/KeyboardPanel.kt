package ie.fairspoken.mobile.ime

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.BasicText
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kyant.backdrop.Backdrop
import com.kyant.backdrop.backdrops.layerBackdrop
import com.kyant.backdrop.backdrops.rememberLayerBackdrop
import com.kyant.shapes.Capsule
import ie.fairspoken.mobile.FairspokenApp
import ie.fairspoken.mobile.core.DictationState
import ie.fairspoken.mobile.ui.Aurora
import ie.fairspoken.mobile.ui.Ink
import ie.fairspoken.mobile.ui.MicOrb
import ie.fairspoken.mobile.ui.Type
import ie.fairspoken.mobile.ui.liquidGlass

class KeyboardPanelState {
    /** The last transcript committed, while it can still be undone. */
    var lastInsert by mutableStateOf<String?>(null)
}

@Composable
fun KeyboardPanel(
    app: FairspokenApp,
    panel: KeyboardPanelState,
    onMic: () -> Unit,
    onSwitch: () -> Unit,
    onPicker: () -> Unit,
    onBackspace: () -> Unit,
    onDeleteWord: () -> Unit,
    onSpace: () -> Unit,
    onEnter: () -> Unit,
    onUndo: () -> Unit,
) {
    val state by app.dictation.state.collectAsState()
    val level by app.dictation.level.collectAsState()
    val hosts by app.store.hosts.collectAsState()
    val activeUrl by app.store.activeUrl.collectAsState()
    val hostName = (hosts.firstOrNull { it.url == activeUrl } ?: hosts.firstOrNull())?.name
    val backdrop = rememberLayerBackdrop()
    val haptics = LocalHapticFeedback.current

    Box(Modifier.fillMaxWidth()) {
        Aurora(Modifier.matchParentSize().layerBackdrop(backdrop), shade = 0.04f)
        Column(
            Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 12.dp, vertical = 10.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            BasicText(
                when (val s = state) {
                    is DictationState.Listening -> "Listening · tap to insert"
                    DictationState.Transcribing -> "Transcribing on ${hostName ?: "your host"}"
                    is DictationState.Done -> s.text
                    is DictationState.Failed -> s.message
                    DictationState.Idle ->
                        if (hostName == null) "Open Fairspoken to pair a host" else "Tap to speak · $hostName"
                },
                style = Type.caption.copy(
                    textAlign = TextAlign.Center,
                    color = if (state is DictationState.Failed) Ink.warn else Ink.secondary,
                ),
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.fillMaxWidth().height(36.dp),
            )
            MicOrb(state, level, backdrop, 96.dp) {
                haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                onMic()
            }
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Key(backdrop, "Switch keyboard", Modifier.weight(1f), onLongClick = onPicker, onClick = onSwitch) { keyboardGlyph(it) }
                if (panel.lastInsert != null) {
                    Key(backdrop, "Undo", Modifier.weight(1.4f), onClick = onUndo) { undoGlyph(it) }
                }
                Key(backdrop, "Space", Modifier.weight(2.4f), onClick = onSpace) { spaceGlyph(it) }
                Key(backdrop, "Delete", Modifier.weight(1f), onLongClick = onDeleteWord, onClick = onBackspace) { backspaceGlyph(it) }
                Key(backdrop, "Enter", Modifier.weight(1f), onClick = onEnter) { enterGlyph(it) }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun Key(
    backdrop: Backdrop,
    label: String,
    modifier: Modifier,
    onLongClick: (() -> Unit)? = null,
    onClick: () -> Unit,
    glyph: DrawScope.(Color) -> Unit,
) {
    val haptics = LocalHapticFeedback.current
    Box(
        modifier
            .height(48.dp)
            .liquidGlass(backdrop, Capsule(), tint = Color.White.copy(alpha = 0.50f), blurRadius = 6.dp, refraction = 8.dp)
            .combinedClickable(
                interactionSource = remember { MutableInteractionSource() },
                indication = null,
                role = Role.Button,
                onLongClick = onLongClick,
                onClick = {
                    haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                    onClick()
                },
            )
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Canvas(Modifier.size(22.dp)) { glyph(Ink.primary) }
    }
}

private fun DrawScope.line(color: Color, vararg xy: Float) {
    val points = xy.toList().chunked(2) { (x, y) -> Offset(size.width * x, size.height * y) }
    val path = Path().apply {
        moveTo(points[0].x, points[0].y)
        points.drop(1).forEach { lineTo(it.x, it.y) }
    }
    drawPath(path, color, style = Stroke(size.minDimension * 0.09f, cap = StrokeCap.Round, join = StrokeJoin.Round))
}

private fun DrawScope.keyboardGlyph(c: Color) {
    line(c, 0.08f, 0.25f, 0.92f, 0.25f, 0.92f, 0.75f, 0.08f, 0.75f, 0.08f, 0.25f)
    line(c, 0.3f, 0.6f, 0.7f, 0.6f)
    listOf(0.25f, 0.42f, 0.58f, 0.75f).forEach { x -> drawCircle(c, size.minDimension * 0.045f, Offset(size.width * x, size.height * 0.42f)) }
}

private fun DrawScope.spaceGlyph(c: Color) = line(c, 0.1f, 0.5f, 0.1f, 0.68f, 0.9f, 0.68f, 0.9f, 0.5f)

private fun DrawScope.backspaceGlyph(c: Color) {
    line(c, 0.32f, 0.25f, 0.92f, 0.25f, 0.92f, 0.75f, 0.32f, 0.75f, 0.06f, 0.5f, 0.32f, 0.25f)
    line(c, 0.48f, 0.38f, 0.72f, 0.62f)
    line(c, 0.72f, 0.38f, 0.48f, 0.62f)
}

private fun DrawScope.enterGlyph(c: Color) {
    line(c, 0.85f, 0.2f, 0.85f, 0.6f, 0.15f, 0.6f)
    line(c, 0.35f, 0.4f, 0.15f, 0.6f, 0.35f, 0.8f)
}

private fun DrawScope.undoGlyph(c: Color) {
    line(c, 0.3f, 0.2f, 0.1f, 0.4f, 0.3f, 0.6f)
    line(c, 0.1f, 0.4f, 0.62f, 0.4f, 0.9f, 0.55f, 0.9f, 0.7f, 0.7f, 0.82f, 0.45f, 0.82f)
}
