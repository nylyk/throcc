package cc.thro.throcc

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.LocalContext

/** The colour of an accent given as 0xRRGGBB. */
fun accentColor(rgb: UInt) = Color((0xFF000000u or rgb).toInt())

/** The system's colours with `accent` as the primary one. */
@Composable
fun ThroccTheme(accent: UInt, content: @Composable () -> Unit) {
    val context = LocalContext.current
    val system = if (isSystemInDarkTheme()) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
    val primary = accentColor(accent)
    val onPrimary = if (primary.luminance() > 0.5f) Color.Black else Color.White
    MaterialTheme(colorScheme = system.copy(primary = primary, onPrimary = onPrimary), content = content)
}
