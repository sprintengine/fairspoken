package ie.fairspoken.mobile.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/**
 * Crystal (design/crystal/README.md): frosted silver glass on a cool,
 * near-neutral backdrop, with the client's green as a hint, never a theme.
 */
@Immutable
data class CrystalColors(
    val dark: Boolean,
    val page: Color,
    val pageSheen: Color,
    /** Glass card fill. */
    val surface: Color,
    /** Fields, popovers, pressed glass. */
    val surfaceStrong: Color,
    /** 1px top/inner highlight on glass. */
    val edge: Color,
    /** Dividers and the outer border of glass. */
    val hairline: Color,
    val ink: Color,
    val ink2: Color,
    val ink3: Color,
    val accent: Color,
    /** Text and glyphs on an accent fill. */
    val onAccent: Color,
    val ok: Color,
    val warn: Color,
    val error: Color,
    /** Microphone on. */
    val live: Color,
    val shadow: Color,
) {
    /** `client-tint`: the accent at 5–8% (light) or 8–12% (dark). */
    val tint: Color get() = accent.copy(alpha = if (dark) 0.12f else 0.08f)
}

val LightCrystal = CrystalColors(
    dark = false,
    page = Color(0xFFEDEFF2),
    pageSheen = Color(0xFFFFFFFF),
    surface = Color.White.copy(alpha = 0.62f),
    surfaceStrong = Color.White.copy(alpha = 0.82f),
    edge = Color.White.copy(alpha = 0.85f),
    hairline = Color(0xFF101820).copy(alpha = 0.09f),
    ink = Color(0xFF15191D),
    ink2 = Color(0xFF5B6670),
    ink3 = Color(0xFF8A949C),
    accent = Color(0xFF2E8B5E),
    onAccent = Color.White,
    ok = Color(0xFF2E8B5E),
    warn = Color(0xFF9A6B12),
    error = Color(0xFFB4283C),
    live = Color(0xFFD9483B),
    shadow = Color(0xFF101820),
)

val DarkCrystal = CrystalColors(
    dark = true,
    page = Color(0xFF0D0F12),
    pageSheen = Color(0xFF1B1F24),
    surface = Color.White.copy(alpha = 0.055f),
    surfaceStrong = Color.White.copy(alpha = 0.09f),
    edge = Color.White.copy(alpha = 0.10f),
    hairline = Color.White.copy(alpha = 0.08f),
    ink = Color(0xFFE8ECEF),
    ink2 = Color(0xFF9AA4AD),
    ink3 = Color(0xFF6B747C),
    accent = Color(0xFF5CCB94),
    onAccent = Color(0xFF0D0F12),
    ok = Color(0xFF5CCB94),
    warn = Color(0xFFE7B95A),
    error = Color(0xFFFF7D8E),
    live = Color(0xFFFF6E62),
    shadow = Color.Black,
)

private val LocalCrystal = staticCompositionLocalOf { LightCrystal }

/** Light or dark, following the system setting. */
@Composable
fun CrystalTheme(dark: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    CompositionLocalProvider(LocalCrystal provides if (dark) DarkCrystal else LightCrystal, content = content)
}

object Crystal {
    val colors: CrystalColors
        @Composable @ReadOnlyComposable get() = LocalCrystal.current
}

/** System font; titles semibold with slightly tight tracking; numbers tabular. */
object Type {
    private val base = TextStyle(fontFeatureSettings = "tnum")

    val largeTitle: TextStyle
        @Composable @ReadOnlyComposable get() = base.copy(
            color = Crystal.colors.ink, fontSize = 30.sp, lineHeight = 36.sp,
            fontWeight = FontWeight.SemiBold, letterSpacing = (-0.6).sp,
        )
    val title: TextStyle
        @Composable @ReadOnlyComposable get() = base.copy(
            color = Crystal.colors.ink, fontSize = 22.sp, lineHeight = 28.sp,
            fontWeight = FontWeight.SemiBold, letterSpacing = (-0.3).sp,
        )
    val heading: TextStyle
        @Composable @ReadOnlyComposable get() = base.copy(
            color = Crystal.colors.ink, fontSize = 17.sp, lineHeight = 22.sp,
            fontWeight = FontWeight.SemiBold, letterSpacing = (-0.2).sp,
        )
    val label: TextStyle
        @Composable @ReadOnlyComposable get() = base.copy(
            color = Crystal.colors.ink, fontSize = 16.sp, lineHeight = 21.sp, fontWeight = FontWeight.Medium,
        )
    val body: TextStyle
        @Composable @ReadOnlyComposable get() = base.copy(color = Crystal.colors.ink2, fontSize = 15.sp, lineHeight = 21.sp)
    val caption: TextStyle
        @Composable @ReadOnlyComposable get() = base.copy(color = Crystal.colors.ink3, fontSize = 13.sp, lineHeight = 18.sp)
    /** Section names above grouped cards. */
    val section: TextStyle
        @Composable @ReadOnlyComposable get() = base.copy(
            color = Crystal.colors.ink2, fontSize = 13.sp, lineHeight = 18.sp,
            fontWeight = FontWeight.Medium, letterSpacing = 0.1.sp,
        )
    val transcript: TextStyle
        @Composable @ReadOnlyComposable get() = base.copy(
            color = Crystal.colors.ink, fontSize = 19.sp, lineHeight = 27.sp, fontFeatureSettings = null,
        )
    /** Addresses, tokens and timings. */
    val mono: TextStyle
        @Composable @ReadOnlyComposable get() = base.copy(
            color = Crystal.colors.ink3, fontSize = 13.sp, lineHeight = 18.sp, fontFamily = FontFamily.Monospace,
        )
}

/**
 * The still backdrop under the glass: a vertical sheen from `page-sheen` to
 * `page` and one soft accent wash in the top-leading corner. Never animated:
 * glass re-renders whenever what is behind it changes.
 */
@Composable
fun CrystalBackdrop(modifier: Modifier = Modifier) {
    val c = Crystal.colors
    Canvas(modifier) {
        drawRect(Brush.verticalGradient(0f to c.pageSheen, 0.55f to c.page, 1f to c.page))
        val center = Offset(size.width * 0.08f, size.height * 0.04f)
        val radius = size.maxDimension * 0.75f
        drawCircle(
            Brush.radialGradient(
                listOf(c.accent.copy(alpha = if (c.dark) 0.09f else 0.07f), c.accent.copy(alpha = 0f)),
                center,
                radius,
            ),
            radius,
            center,
        )
    }
}
