package cc.thro.throcc

import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.luminance
import cc.thro.throcc.core.Palette

/** The colour given as 0xRRGGBB. */
fun color(rgb: UInt) = Color((0xFF000000u or rgb).toInt())

/** Material's colours mixed from the eight of `palette`, as the desktop mixes its own. */
fun colorScheme(palette: Palette): ColorScheme {
    val bg = color(palette.bg)
    val panel = color(palette.panel)
    val text = color(palette.text)
    val muted = color(palette.muted)
    val accent = color(palette.accent)
    val good = color(palette.good)
    val bad = color(palette.bad)
    val raised = lerp(panel, text, 0.06f)
    val onAccent = if (accent.luminance() > 0.5f) bg else text
    return darkColorScheme(
        primary = accent,
        onPrimary = onAccent,
        primaryContainer = lerp(panel, accent, 0.25f),
        onPrimaryContainer = text,
        inversePrimary = accent,
        secondary = accent,
        onSecondary = onAccent,
        secondaryContainer = lerp(panel, accent, 0.18f),
        onSecondaryContainer = text,
        tertiary = good,
        onTertiary = bg,
        tertiaryContainer = lerp(panel, good, 0.2f),
        onTertiaryContainer = text,
        background = bg,
        onBackground = text,
        surface = bg,
        onSurface = text,
        surfaceVariant = raised,
        onSurfaceVariant = muted,
        surfaceTint = accent,
        inverseSurface = text,
        inverseOnSurface = bg,
        error = bad,
        onError = bg,
        errorContainer = lerp(panel, bad, 0.2f),
        onErrorContainer = text,
        outline = lerp(panel, muted, 0.6f),
        outlineVariant = lerp(panel, text, 0.12f),
        scrim = Color.Black,
        surfaceBright = raised,
        surfaceDim = bg,
        surfaceContainerLowest = bg,
        surfaceContainerLow = lerp(bg, panel, 0.5f),
        surfaceContainer = panel,
        surfaceContainerHigh = raised,
        surfaceContainerHighest = lerp(panel, text, 0.1f),
    )
}

/** `scheme` for a pane beside the main one: it sits on the panel colour. */
fun sidePane(scheme: ColorScheme) = scheme.copy(
    background = scheme.surfaceContainer,
    surface = scheme.surfaceContainer,
    surfaceContainer = scheme.surfaceContainerHigh,
    surfaceContainerLow = scheme.surfaceContainerHigh,
    surfaceContainerHigh = scheme.surfaceContainerHighest,
)

@Composable
fun ThroccTheme(palette: Palette, content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = colorScheme(palette), content = content)
}
