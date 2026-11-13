package cc.thro.throcc

import android.graphics.Color.TRANSPARENT
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.LocalActivity
import androidx.activity.enableEdgeToEdge
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.luminance
import cc.thro.throcc.core.Palette
import cc.thro.throcc.core.isLightTheme

/** The colour given as 0xRRGGBB. */
fun color(rgb: UInt) = Color((0xFF000000u or rgb).toInt())

/** Whether the theme is a light one, as the desktop judges it. */
val Palette.isLight get() = isLightTheme(this)

/** The contrast ratio of two colours, from 1 to 21. */
private fun contrast(one: Color, other: Color): Float {
    val (darker, lighter) = listOf(one.luminance(), other.luminance()).sorted()
    return (lighter + 0.05f) / (darker + 0.05f)
}

/** `color` mixed toward `text` until it reads as text on `surface`, at 4.5 to 1. */
private fun readable(color: Color, text: Color, surface: Color) =
    (0..9).map { lerp(color, text, it / 10f) }.firstOrNull { contrast(it, surface) >= 4.5f } ?: text

/** Material's colours mixed from the eight of `palette`, as the desktop mixes its own. */
fun colorScheme(palette: Palette): ColorScheme {
    val bg = color(palette.bg)
    val panel = color(palette.panel)
    val text = color(palette.text)
    val good = color(palette.good)
    val raised = lerp(panel, text, 0.06f)
    val highest = lerp(panel, text, 0.1f)
    val selected = lerp(panel, color(palette.accent), 0.18f)
    val danger = lerp(panel, color(palette.bad), 0.18f)
    val hardest = listOf(highest, selected, danger).minBy { contrast(text, it) }
    val muted = readable(color(palette.muted), text, hardest)
    val accent = readable(color(palette.accent), text, hardest)
    val bad = readable(color(palette.bad), text, hardest)
    return darkColorScheme(
        primary = accent,
        onPrimary = bg,
        primaryContainer = lerp(panel, accent, 0.3f),
        onPrimaryContainer = text,
        inversePrimary = readable(color(palette.accent), bg, text),
        secondary = accent,
        onSecondary = bg,
        secondaryContainer = selected,
        onSecondaryContainer = text,
        tertiary = good,
        onTertiary = bg,
        tertiaryContainer = lerp(panel, good, 0.2f),
        onTertiaryContainer = text,
        background = bg,
        onBackground = text,
        surface = bg,
        onSurface = text,
        // This tone differs from every container's, so contentColorFor never gives a container the muted text.
        surfaceVariant = lerp(panel, text, 0.08f),
        onSurfaceVariant = muted,
        surfaceTint = accent,
        inverseSurface = text,
        inverseOnSurface = bg,
        error = bad,
        onError = bg,
        errorContainer = danger,
        onErrorContainer = text,
        outline = color(palette.muted),
        outlineVariant = lerp(panel, text, 0.12f),
        scrim = Color.Black,
        surfaceBright = raised,
        surfaceDim = bg,
        surfaceContainerLowest = bg,
        surfaceContainerLow = lerp(bg, panel, 0.5f),
        surfaceContainer = panel,
        surfaceContainerHigh = raised,
        surfaceContainerHighest = highest,
    )
}

/** `scheme` one step up, for a pane or a sheet that sits on the panel colour. */
fun raised(scheme: ColorScheme) = scheme.copy(
    background = scheme.surfaceContainer,
    surface = scheme.surfaceContainer,
    surfaceContainer = scheme.surfaceContainerHigh,
    surfaceContainerHigh = scheme.surfaceContainerHighest,
)

/** The theme of `palette`. The system bars' icons follow it too. */
@Composable
fun ThroccTheme(palette: Palette, content: @Composable () -> Unit) {
    val activity = LocalActivity.current as ComponentActivity
    LaunchedEffect(palette.isLight) {
        val bars = SystemBarStyle.auto(TRANSPARENT, TRANSPARENT) { !palette.isLight }
        activity.enableEdgeToEdge(bars, bars)
    }
    MaterialExpressiveTheme(colorScheme = remember(palette) { colorScheme(palette) }, content = content)
}
