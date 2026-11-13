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
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.lerp
import cc.thro.throcc.core.Palette
import cc.thro.throcc.core.isLightTheme
import cc.thro.throcc.core.themeColors

/** The colour given as 0xRRGGBB. */
fun color(rgb: UInt) = Color((0xFF000000u or rgb).toInt())

/** Whether the theme is a light one, as the desktop judges it. */
val Palette.isLight get() = isLightTheme(this)

/** Material's colours from those every client mixes from the eight of `palette`. */
fun colorScheme(palette: Palette): ColorScheme {
    val mixed = themeColors(palette)
    val bg = color(mixed.bg)
    val panel = color(mixed.panel)
    val text = color(mixed.text)
    val accent = color(mixed.accent)
    val good = color(mixed.good)
    val raised = color(mixed.raised)
    return darkColorScheme(
        primary = accent,
        onPrimary = bg,
        primaryContainer = lerp(panel, accent, 0.3f),
        onPrimaryContainer = text,
        inversePrimary = color(palette.accent),
        secondary = accent,
        onSecondary = bg,
        secondaryContainer = color(mixed.selected),
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
        onSurfaceVariant = color(mixed.muted),
        surfaceTint = accent,
        inverseSurface = text,
        inverseOnSurface = bg,
        error = color(mixed.bad),
        onError = bg,
        errorContainer = color(mixed.danger),
        onErrorContainer = text,
        outline = color(palette.muted),
        outlineVariant = color(mixed.line),
        scrim = Color.Black,
        surfaceBright = raised,
        surfaceDim = bg,
        surfaceContainerLowest = bg,
        surfaceContainerLow = lerp(bg, panel, 0.5f),
        surfaceContainer = panel,
        surfaceContainerHigh = raised,
        surfaceContainerHighest = color(mixed.control),
    )
}

/** The warning colour, which Material's scheme has no place for. It reads as text on every surface. */
val LocalWarning = staticCompositionLocalOf { Color.Unspecified }

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
    val warning = remember(palette) { color(themeColors(palette).warning) }
    CompositionLocalProvider(LocalWarning provides warning) {
        MaterialExpressiveTheme(colorScheme = remember(palette) { colorScheme(palette) }, content = content)
    }
}
