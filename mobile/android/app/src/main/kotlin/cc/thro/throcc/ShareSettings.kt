package cc.thro.throcc

import android.app.Activity
import android.media.projection.MediaProjectionManager
import androidx.activity.compose.LocalActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ScreenShare
import androidx.compose.material.icons.automirrored.filled.StopScreenShare
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalIconToggleButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconToggleButtonShapes
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SheetValue
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.rememberBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.fitShareChoice
import cc.thro.throcc.core.mbps
import cc.thro.throcc.core.shareBitrates
import cc.thro.throcc.core.shareFrameRates
import cc.thro.throcc.core.shareResolutions
import kotlin.math.roundToInt

/** Opens the share settings, and once the person agrees to share their screen, shares it; or stops our share. */
@Composable
fun ShareScreenButton(model: AppModel, sharing: Boolean, shapes: IconToggleButtonShapes, modifier: Modifier) {
    val activity = LocalActivity.current
    val consent = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (result.resultCode != Activity.RESULT_OK || activity == null) return@rememberLauncherForActivityResult
        val screen = activity.windowManager.maximumWindowMetrics.bounds
        val dpi = activity.resources.configuration.densityDpi
        shareScreen(activity, result, screen.width(), screen.height(), dpi, model.shareAudio)
    }
    FilledTonalIconToggleButton(
        sharing,
        { if (sharing) model.core.command(Command.StopShare) else model.shareSettings = true },
        shapes,
        modifier,
    ) {
        if (sharing) {
            Icon(Icons.AutoMirrored.Filled.StopScreenShare, "Stop Sharing")
        } else {
            Icon(Icons.AutoMirrored.Filled.ScreenShare, "Share Screen")
        }
    }
    if (model.shareSettings) {
        ShareSettings(model) {
            if (model.mock) {
                model.core.command(Command.StartShare(model.shareChoice, model.shareAudio))
            } else if (activity != null) {
                consent.launch(activity.getSystemService(MediaProjectionManager::class.java).createScreenCaptureIntent())
            }
        }
    }
}

/** What the next share asks for, as the desktop's Share Your Screen dialog offers it. `start` asks for the screen. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ShareSettings(model: AppModel, start: () -> Unit) {
    val close = { model.shareSettings = false }
    val choice = model.shareChoice
    val bitrates = shareBitrates(choice)
    val heading = MaterialTheme.typography.titleMedium
    MaterialTheme(colorScheme = raised(MaterialTheme.colorScheme)) {
        ModalBottomSheet(
            close,
            sheetState = rememberBottomSheetState(SheetValue.Hidden, setOf(SheetValue.Hidden, SheetValue.Expanded)),
            containerColor = MaterialTheme.colorScheme.surface,
        ) {
            Column(
                Modifier.verticalScroll(rememberScrollState()).padding(start = 24.dp, end = 24.dp, bottom = 24.dp),
                Arrangement.spacedBy(16.dp),
            ) {
                Text("Share Your Screen", style = MaterialTheme.typography.headlineSmallEmphasized)
                Text("Resolution", style = heading)
                Choices(shareResolutions().map { it.height to it.label }, choice.height) {
                    model.shareChoice = fitShareChoice(choice.copy(height = it))
                }
                Text("Frame rate", style = heading)
                Choices(shareFrameRates().map { it.toUByte() }.map { it to "$it fps" }, choice.fps) {
                    model.shareChoice = fitShareChoice(choice.copy(fps = it))
                }
                Column {
                    Labeled("Bitrate", mbps(choice.kbps))
                    if (bitrates.size > 1) {
                        val step = bitrates.indexOfFirst { it >= choice.kbps }.takeIf { it >= 0 } ?: bitrates.lastIndex
                        Slider(
                            step.toFloat(),
                            { model.shareChoice = choice.copy(kbps = bitrates[it.roundToInt().coerceIn(bitrates.indices)]) },
                            valueRange = 0f..bitrates.lastIndex.toFloat(),
                            steps = bitrates.size - 2,
                        )
                        Row {
                            Text(mbps(bitrates.first()), Modifier.weight(1f), MaterialTheme.colorScheme.onSurfaceVariant)
                            Text(mbps(bitrates.last()), color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
                SwitchRow("Share device audio", model.shareAudio) { model.shareAudio = it }
                MainButton("Start Sharing") {
                    close()
                    start()
                }
            }
        }
    }
}

/** One choice of `options`, each a value with its label, as a row of segmented buttons. */
@Composable
private fun <T> Choices(options: List<Pair<T, String>>, picked: T, pick: (T) -> Unit) {
    SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
        options.forEachIndexed { index, (value, label) ->
            SegmentedButton(
                value == picked,
                { pick(value) },
                SegmentedButtonDefaults.itemShape(index, options.size),
                icon = {},
                label = { Text(label, maxLines = 1) },
            )
        }
    }
}
