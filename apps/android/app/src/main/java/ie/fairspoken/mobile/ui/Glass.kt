package ie.fairspoken.mobile.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicText
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.DpOffset
import androidx.compose.ui.unit.dp
import com.kyant.backdrop.Backdrop
import com.kyant.backdrop.drawBackdrop
import com.kyant.backdrop.effects.blur
import com.kyant.backdrop.effects.lens
import com.kyant.backdrop.effects.vibrancy
import com.kyant.backdrop.highlight.Highlight
import com.kyant.backdrop.shadow.Shadow
import com.kyant.shapes.Capsule
import com.kyant.shapes.RoundedRectangle
import com.kyant.shapes.RoundedRectangularShape

/** Radii from the spec: 20 for cards, 12 for fields and rows, capsule for buttons. */
object Radii {
    val card = 20.dp
    val field = 12.dp
}

/**
 * Liquid glass: blur, a little lens refraction at the rim, vibrancy, a
 * specular edge, a hairline border and a soft drop shadow. [tint] goes over
 * the surface; the spec keeps it for the one primary control on a screen.
 * It is read while drawing, so an animated tint doesn't recompose.
 */
@Composable
fun Modifier.glass(
    backdrop: Backdrop,
    shape: RoundedRectangularShape,
    fill: Color = Crystal.colors.surface,
    tint: () -> Color = { Color.Transparent },
    blurRadius: Dp = 18.dp,
    refraction: Dp = 12.dp,
    elevated: Boolean = true,
): Modifier {
    val c = Crystal.colors
    return this
        .drawBackdrop(
            backdrop = backdrop,
            shape = { shape },
            effects = {
                vibrancy()
                blur(blurRadius.toPx())
                if (refraction > 0.dp) lens(refraction.toPx(), refraction.toPx() * 2f, depthEffect = true)
            },
            highlight = { Highlight.Default.copy(alpha = if (c.dark) 0.5f else 1f) },
            shadow = {
                Shadow(
                    radius = if (elevated) 24.dp else 6.dp,
                    offset = DpOffset(0.dp, if (elevated) 8.dp else 2.dp),
                    color = c.shadow,
                    alpha = if (c.dark) 0.30f else if (elevated) 0.07f else 0.05f,
                )
            },
            onDrawSurface = {
                drawRect(fill)
                val overlay = tint()
                if (overlay.alpha > 0f) drawRect(overlay)
            },
        )
        .border(0.8.dp, c.hairline, shape)
}

/** Scales a control down a touch while pressed. */
private fun Modifier.pressScale(pressed: Boolean, enabled: Boolean = true) = graphicsLayer {
    val s = if (pressed) 0.96f else 1f
    scaleX = s
    scaleY = s
    alpha = if (enabled) 1f else 0.45f
}

@Composable
fun GlassCard(
    backdrop: Backdrop,
    modifier: Modifier = Modifier,
    padding: PaddingValues = PaddingValues(20.dp),
    spacing: Dp = 16.dp,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(
        modifier.glass(backdrop, RoundedRectangle(Radii.card)).padding(padding),
        verticalArrangement = Arrangement.spacedBy(spacing),
        content = content,
    )
}

/**
 * A glass button sitting on the backdrop. [primary] tints it with the accent:
 * use that for the one main action on a screen.
 */
@Composable
fun GlassButton(
    text: String,
    backdrop: Backdrop,
    modifier: Modifier = Modifier,
    primary: Boolean = false,
    enabled: Boolean = true,
    busy: Boolean = false,
    onClick: () -> Unit,
) {
    val c = Crystal.colors
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    val textColor = if (primary) c.onAccent else c.ink
    Box(
        modifier
            .heightIn(min = 54.dp)
            .pressScale(pressed, enabled)
            .glass(
                backdrop,
                Capsule(),
                fill = if (primary) c.accent.copy(alpha = 0.92f) else c.surfaceStrong,
                blurRadius = 10.dp,
                refraction = 10.dp,
                elevated = primary,
            )
            .clickable(interaction, indication = null, enabled = enabled && !busy, role = Role.Button, onClick = onClick)
            .padding(horizontal = 24.dp, vertical = 15.dp),
        contentAlignment = Alignment.Center,
    ) {
        if (busy) {
            Spinner(textColor, Modifier.size(20.dp))
        } else {
            BasicText(text, style = Type.label.copy(color = textColor, fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold))
        }
    }
}

/** A round glass button with a drawn glyph, for the top bar. */
@Composable
fun GlassIconButton(
    label: String,
    backdrop: Backdrop,
    modifier: Modifier = Modifier,
    size: Dp = 44.dp,
    onClick: () -> Unit,
    glyph: DrawScope.(Color) -> Unit,
) {
    val c = Crystal.colors
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    Box(
        modifier
            .size(size)
            .pressScale(pressed)
            .glass(backdrop, Capsule(), fill = c.surfaceStrong, blurRadius = 10.dp, refraction = 8.dp, elevated = false)
            .clickable(interaction, indication = null, role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        val ink = c.ink
        Canvas(Modifier.size(size * 0.46f)) { glyph(ink) }
    }
}

/**
 * A flat capsule for use inside a glass card (no glass on glass).
 * [primary] fills it with the accent.
 */
@Composable
fun PillButton(
    text: String,
    modifier: Modifier = Modifier,
    primary: Boolean = false,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    val c = Crystal.colors
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    Box(
        modifier
            .heightIn(min = 36.dp)
            .pressScale(pressed, enabled)
            .background(if (primary) c.accent else c.ink.copy(alpha = if (c.dark) 0.10f else 0.06f), CircleShape)
            .clickable(interaction, indication = null, enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        BasicText(text, style = Type.label.copy(color = if (primary) c.onAccent else c.ink, fontSize = Type.body.fontSize))
    }
}

@Composable
fun Chip(text: String, selected: Boolean, onClick: () -> Unit) {
    val c = Crystal.colors
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    Box(
        Modifier
            .pressScale(pressed)
            .background(if (selected) c.accent.copy(alpha = if (c.dark) 0.18f else 0.12f) else Color.Transparent, CircleShape)
            .border(1.dp, if (selected) c.accent.copy(alpha = 0.55f) else c.hairline, CircleShape)
            .clickable(interaction, indication = null, role = Role.RadioButton, onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 8.dp),
    ) {
        BasicText(text, style = Type.body.copy(color = if (selected) c.ink else c.ink2))
    }
}

/** A plain text field, for inside a glass card. */
@Composable
fun Field(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
    password: Boolean = false,
    mono: Boolean = false,
    imeAction: ImeAction = ImeAction.Done,
    onAction: () -> Unit = {},
) {
    val c = Crystal.colors
    val style: TextStyle = if (mono) Type.mono.copy(color = c.ink, fontSize = Type.label.fontSize) else Type.label
    Box(modifier.padding(vertical = 14.dp), contentAlignment = Alignment.CenterStart) {
        if (value.isEmpty()) BasicText(placeholder, style = style.copy(color = c.ink3), maxLines = 1)
        BasicTextField(
            value = value,
            onValueChange = onValueChange,
            singleLine = true,
            textStyle = style,
            cursorBrush = SolidColor(c.accent),
            visualTransformation = if (password) PasswordVisualTransformation() else VisualTransformation.None,
            keyboardOptions = KeyboardOptions(
                keyboardType = if (password) KeyboardType.Password else KeyboardType.Uri,
                capitalization = KeyboardCapitalization.None,
                imeAction = imeAction,
                autoCorrectEnabled = false,
            ),
            // Next keeps its default (focus moves on); Done and Go submit.
            keyboardActions = KeyboardActions(onDone = { onAction() }, onGo = { onAction() }),
            modifier = Modifier.fillMaxWidth().semantics { contentDescription = placeholder },
        )
    }
}

@Composable
fun CrystalSwitch(checked: Boolean, onChange: (Boolean) -> Unit, label: String, modifier: Modifier = Modifier) {
    val c = Crystal.colors
    val track = if (checked) c.accent else c.ink.copy(alpha = if (c.dark) 0.16f else 0.10f)
    Canvas(
        modifier
            .size(50.dp, 30.dp)
            .clickable(remember { MutableInteractionSource() }, null, role = Role.Switch) { onChange(!checked) }
            .semantics { contentDescription = label },
    ) {
        val r = size.height / 2
        drawRoundRect(track, cornerRadius = CornerRadius(r))
        val knobX = if (checked) size.width - r else r
        drawCircle(Color.Black.copy(alpha = 0.10f), r - 2.dp.toPx(), Offset(knobX, r + 1.dp.toPx()))
        drawCircle(Color.White, r - 2.5.dp.toPx(), Offset(knobX, r))
    }
}

/** A labelled row with a trailing control, for settings cards. */
@Composable
fun SettingRow(
    title: String,
    modifier: Modifier = Modifier,
    detail: String? = null,
    trailing: @Composable () -> Unit = {},
) {
    Row(modifier.fillMaxWidth().heightIn(min = 44.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            BasicText(title, style = Type.label)
            if (detail != null) BasicText(detail, style = Type.caption, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
        Spacer(Modifier.width(12.dp))
        trailing()
    }
}

@Composable
fun SwitchRow(title: String, checked: Boolean, onChange: (Boolean) -> Unit, detail: String? = null) {
    SettingRow(title, detail = detail) { CrystalSwitch(checked, onChange, title) }
}

@Composable
fun Hairline(modifier: Modifier = Modifier) {
    Box(modifier.fillMaxWidth().height(0.8.dp).background(Crystal.colors.hairline))
}

/** Status dot with a soft halo. */
@Composable
fun Dot(color: Color, modifier: Modifier = Modifier) {
    Canvas(modifier.size(10.dp)) {
        drawCircle(color.copy(alpha = 0.25f), size.minDimension / 2)
        drawCircle(color, size.minDimension / 3.4f)
    }
}

/** A filled circle with a check, or an empty ring: one setup step. */
@Composable
fun StepMark(done: Boolean, modifier: Modifier = Modifier) {
    val c = Crystal.colors
    Canvas(modifier.size(22.dp)) {
        if (done) {
            drawCircle(c.accent)
            checkGlyph(c.onAccent)
        } else {
            drawCircle(c.ink3, style = Stroke(1.5.dp.toPx()))
        }
    }
}

@Composable
fun Spinner(color: Color, modifier: Modifier) {
    val turn by rememberInfiniteTransition(label = "spin").animateFloat(
        0f, 360f, infiniteRepeatable(tween(900, easing = LinearEasing), RepeatMode.Restart), label = "turn",
    )
    Canvas(modifier) { spinner(color, turn) }
}

@Composable
fun Glyph(modifier: Modifier, color: Color, draw: DrawScope.(Color) -> Unit) {
    Canvas(modifier) { draw(color) }
}

/** The card shape for flat rows that need a background (selected host). */
val RowShape = RoundedCornerShape(Radii.field)

// Icons are drawn, not bundled: a handful of paths beats an icon library.

private fun DrawScope.stroke(width: Float = size.minDimension * 0.09f) =
    Stroke(width, cap = StrokeCap.Round, join = StrokeJoin.Round)

/** Polyline through (x, y) pairs given as fractions of the canvas. */
fun DrawScope.polyline(color: Color, vararg xy: Float, width: Float = size.minDimension * 0.09f) {
    val path = Path()
    xy.toList().chunked(2).forEachIndexed { i, (x, y) ->
        if (i == 0) path.moveTo(size.width * x, size.height * y) else path.lineTo(size.width * x, size.height * y)
    }
    drawPath(path, color, style = stroke(width))
}

fun DrawScope.micGlyph(color: Color, strokeWidth: Float = size.minDimension * 0.085f) {
    val w = size.width
    val h = size.height
    val capsuleW = w * 0.34f
    val capsuleH = h * 0.5f
    drawRoundRect(
        color,
        topLeft = Offset((w - capsuleW) / 2, h * 0.08f),
        size = Size(capsuleW, capsuleH),
        cornerRadius = CornerRadius(capsuleW / 2),
    )
    val arc = Path().apply {
        moveTo(w * 0.24f, h * 0.44f)
        cubicTo(w * 0.24f, h * 0.78f, w * 0.76f, h * 0.78f, w * 0.76f, h * 0.44f)
    }
    drawPath(arc, color, style = Stroke(strokeWidth, cap = StrokeCap.Round))
    drawLine(color, Offset(w / 2, h * 0.71f), Offset(w / 2, h * 0.88f), strokeWidth, StrokeCap.Round)
    drawLine(color, Offset(w * 0.36f, h * 0.9f), Offset(w * 0.64f, h * 0.9f), strokeWidth, StrokeCap.Round)
}

fun DrawScope.stopGlyph(color: Color) {
    val s = size.minDimension * 0.42f
    drawRoundRect(
        color,
        topLeft = Offset((size.width - s) / 2, (size.height - s) / 2),
        size = Size(s, s),
        cornerRadius = CornerRadius(s * 0.22f),
    )
}

fun DrawScope.checkGlyph(color: Color) = polyline(color, 0.28f, 0.52f, 0.44f, 0.67f, 0.74f, 0.35f, width = size.minDimension * 0.1f)

fun DrawScope.spinner(color: Color, sweepStart: Float) {
    val stroke = size.minDimension * 0.09f
    drawArc(
        color,
        startAngle = sweepStart,
        sweepAngle = 260f,
        useCenter = false,
        topLeft = Offset(stroke, stroke),
        size = Size(size.width - stroke * 2, size.height - stroke * 2),
        style = Stroke(stroke, cap = StrokeCap.Round),
    )
}

/** Settings: two sliders. */
fun DrawScope.slidersGlyph(color: Color) {
    val w = size.minDimension * 0.085f
    polyline(color, 0.12f, 0.32f, 0.88f, 0.32f, width = w)
    polyline(color, 0.12f, 0.68f, 0.88f, 0.68f, width = w)
    val r = size.minDimension * 0.13f
    drawCircle(color, r, Offset(size.width * 0.34f, size.height * 0.32f))
    drawCircle(color, r, Offset(size.width * 0.66f, size.height * 0.68f))
}

fun DrawScope.backGlyph(color: Color) = polyline(color, 0.62f, 0.18f, 0.3f, 0.5f, 0.62f, 0.82f)

fun DrawScope.chevronDownGlyph(color: Color) = polyline(color, 0.25f, 0.4f, 0.5f, 0.65f, 0.75f, 0.4f)

fun DrawScope.chevronUpGlyph(color: Color) = polyline(color, 0.25f, 0.6f, 0.5f, 0.35f, 0.75f, 0.6f)
