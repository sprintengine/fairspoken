package ie.fairspoken.mobile.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.BasicText
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.kyant.backdrop.Backdrop
import com.kyant.backdrop.drawBackdrop
import com.kyant.backdrop.effects.blur
import com.kyant.backdrop.effects.lens
import com.kyant.backdrop.effects.vibrancy
import com.kyant.backdrop.highlight.Highlight
import com.kyant.shapes.Capsule
import com.kyant.shapes.RoundedRectangle
import com.kyant.shapes.RoundedRectangularShape

object Ink {
    // Silver crystal: graphite text on frosted white, green only as a hint.
    val primary = Color(0xFF18211E)
    val secondary = Color(0xFF46524E)
    val faint = Color(0xFF7B8884)
    val accent = Color(0xFF2E9A6B)
    val record = Color(0xFF34B27A)
    val ok = Color(0xFF2E9A6B)
    val warn = Color(0xFF3A4440)
    /** Unmet or offline: plain silver. */
    val muted = Color(0xFF9AA5A1)
}

object Type {
    val title = TextStyle(color = Ink.primary, fontSize = 30.sp, fontWeight = FontWeight.SemiBold, letterSpacing = (-0.5).sp)
    val heading = TextStyle(color = Ink.primary, fontSize = 18.sp, fontWeight = FontWeight.SemiBold)
    val body = TextStyle(color = Ink.secondary, fontSize = 15.sp, lineHeight = 21.sp)
    val label = TextStyle(color = Ink.primary, fontSize = 15.sp, fontWeight = FontWeight.Medium)
    val caption = TextStyle(color = Ink.faint, fontSize = 12.5.sp, lineHeight = 17.sp)
    val transcript = TextStyle(color = Ink.primary, fontSize = 19.sp, lineHeight = 27.sp)
}

/**
 * The colour field under the glass. Static on purpose: glass re-renders
 * whenever what is behind it changes, and a still backdrop costs nothing.
 */
@Composable
fun Aurora(modifier: Modifier = Modifier, shade: Float = 0f) {
    Canvas(modifier) {
        drawRect(Brush.verticalGradient(listOf(Color(0xFFF3F5F4), Color(0xFFDCE2E0), Color(0xFFC5CDCA))))
        blob(Color(0xFF8E9A96), 0.10f, 0.08f, 0.70f, 0.30f)
        blob(Color.White, 0.95f, 0.30f, 0.70f, 0.85f)
        blob(Color(0xFF7FCFA6), 0.08f, 0.72f, 0.55f, 0.20f)
        blob(Color(0xFF6F7B77), 0.92f, 0.92f, 0.55f, 0.20f)
        // Facets of light for the glass to bend, like looking through crystal.
        drawRect(
            Brush.linearGradient(
                listOf(Color.Transparent, Color.White.copy(alpha = 0.6f), Color.Transparent),
                start = Offset(0f, size.height * 0.22f),
                end = Offset(size.width * 0.7f, size.height * 0.52f),
            ),
        )
        drawRect(
            Brush.linearGradient(
                listOf(Color.Transparent, Color.White.copy(alpha = 0.4f), Color.Transparent),
                start = Offset(size.width * 0.25f, size.height * 0.62f),
                end = Offset(size.width, size.height * 0.86f),
            ),
        )
        if (shade > 0f) drawRect(Ink.primary.copy(alpha = shade))
    }
}

private fun DrawScope.blob(color: Color, x: Float, y: Float, r: Float, alpha: Float) {
    val center = Offset(size.width * x, size.height * y)
    val radius = size.width * r
    drawCircle(
        Brush.radialGradient(listOf(color.copy(alpha = alpha), color.copy(alpha = 0f)), center, radius),
        radius,
        center,
    )
}

/** Liquid glass: blur, lens refraction at the rim, vibrancy and a specular edge. */
fun Modifier.liquidGlass(
    backdrop: Backdrop,
    shape: RoundedRectangularShape,
    tint: Color = Color.White.copy(alpha = 0.38f),
    blurRadius: Dp = 14.dp,
    refraction: Dp = 16.dp,
): Modifier = drawBackdrop(
    backdrop = backdrop,
    shape = { shape },
    effects = {
        vibrancy()
        blur(blurRadius.toPx())
        lens(refraction.toPx(), refraction.toPx() * 2f, depthEffect = true)
    },
    highlight = { Highlight.Default },
    onDrawSurface = { drawRect(tint) },
)

@Composable
fun GlassCard(
    backdrop: Backdrop,
    modifier: Modifier = Modifier,
    content: @Composable BoxScope.() -> Unit,
) {
    Box(
        modifier
            .liquidGlass(backdrop, RoundedRectangle(30.dp), refraction = 20.dp)
            .padding(20.dp),
        content = content,
    )
}

@Composable
fun GlassButton(
    text: String,
    backdrop: Backdrop,
    modifier: Modifier = Modifier,
    tint: Color = Color.White.copy(alpha = 0.55f),
    textColor: Color = Ink.primary,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    Box(
        modifier
            .graphicsLayer {
                val s = if (pressed) 0.96f else 1f
                scaleX = s; scaleY = s
                alpha = if (enabled) 1f else 0.45f
            }
            .liquidGlass(backdrop, Capsule(), tint = tint, blurRadius = 8.dp, refraction = 10.dp)
            .clickable(interaction, indication = null, enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 18.dp, vertical = 11.dp),
        contentAlignment = Alignment.Center,
    ) {
        BasicText(text, style = Type.label.copy(color = textColor))
    }
}

@Composable
fun GlassChip(text: String, selected: Boolean, backdrop: Backdrop, onClick: () -> Unit) {
    GlassButton(
        text = text,
        backdrop = backdrop,
        tint = if (selected) Ink.accent.copy(alpha = 0.18f) else Color.White.copy(alpha = 0.30f),
        textColor = if (selected) Ink.primary else Ink.secondary,
        onClick = onClick,
    )
}

@Composable
fun GlassField(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    backdrop: Backdrop,
    modifier: Modifier = Modifier,
    password: Boolean = false,
    onDone: () -> Unit = {},
) {
    Box(
        modifier
            .liquidGlass(backdrop, Capsule(), tint = Color.White.copy(alpha = 0.60f), blurRadius = 6.dp, refraction = 8.dp)
            .padding(horizontal = 18.dp, vertical = 13.dp),
    ) {
        if (value.isEmpty()) BasicText(placeholder, style = Type.body.copy(color = Ink.faint))
        BasicTextField(
            value = value,
            onValueChange = onValueChange,
            singleLine = true,
            textStyle = Type.label,
            cursorBrush = SolidColor(Ink.accent),
            visualTransformation = if (password) PasswordVisualTransformation() else VisualTransformation.None,
            keyboardOptions = KeyboardOptions(
                keyboardType = if (password) KeyboardType.Password else KeyboardType.Uri,
                imeAction = ImeAction.Done,
                autoCorrectEnabled = false,
            ),
            keyboardActions = KeyboardActions(onDone = { onDone() }),
            modifier = Modifier.fillMaxWidth().semantics { contentDescription = placeholder },
        )
    }
}

@Composable
fun GlassSwitch(checked: Boolean, onChange: (Boolean) -> Unit, modifier: Modifier = Modifier) {
    Canvas(
        modifier
            .size(52.dp, 30.dp)
            .clickable(remember { MutableInteractionSource() }, null, role = Role.Switch) { onChange(!checked) },
    ) {
        val r = size.height / 2
        drawRoundRect(
            if (checked) Ink.accent.copy(alpha = 0.85f) else Ink.primary.copy(alpha = 0.12f),
            cornerRadius = CornerRadius(r),
        )
        drawRoundRect(Ink.primary.copy(alpha = 0.08f), cornerRadius = CornerRadius(r), style = Stroke(1.dp.toPx()))
        val knobX = if (checked) size.width - r else r
        drawCircle(Ink.primary.copy(alpha = 0.10f), r - 2.dp.toPx(), Offset(knobX, r + 0.5.dp.toPx()))
        drawCircle(Color.White, r - 3.dp.toPx(), Offset(knobX, r))
    }
}

/** Status dot with a soft halo. */
@Composable
fun Dot(color: Color, modifier: Modifier = Modifier) {
    Canvas(modifier.size(10.dp)) {
        drawCircle(color.copy(alpha = 0.3f), size.minDimension / 2)
        drawCircle(color, size.minDimension / 3.2f)
    }
}

// Icons are drawn, not bundled: a handful of paths beats an icon library.

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

fun DrawScope.checkGlyph(color: Color) {
    val w = size.width
    val h = size.height
    val path = Path().apply {
        moveTo(w * 0.24f, h * 0.52f)
        lineTo(w * 0.42f, h * 0.70f)
        lineTo(w * 0.78f, h * 0.32f)
    }
    drawPath(path, color, style = Stroke(size.minDimension * 0.1f, cap = StrokeCap.Round))
}

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
