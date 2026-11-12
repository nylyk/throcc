package cc.thro.throcc

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.OpenInNew
import androidx.compose.material.icons.filled.Block
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.Palette as PaletteIcon
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.RemoveModerator
import androidx.compose.material.icons.filled.VerifiedUser
import androidx.compose.material3.Card
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.MicLevel
import cc.thro.throcc.core.NoiseSuppression
import cc.thro.throcc.core.Voice
import cc.thro.throcc.core.Palette
import cc.thro.throcc.core.fingerprintShort
import cc.thro.throcc.core.quietestGateDbfs
import kotlin.math.roundToInt
import kotlinx.coroutines.delay

private const val SOURCE = "https://github.com/nylyk/throcc"

private val COLOR_LABELS = listOf("Background", "Panel", "Text", "Muted text", "Accent", "Warning", "Good", "Bad")

@Composable
fun Settings(model: AppModel) {
    val pages = listOf(
        Triple(Icons.Default.Person, "Profile", Screen.Profile),
        Triple(Icons.Default.Mic, "Voice", Screen.Voice),
        Triple(Icons.Default.PaletteIcon, "Appearance", Screen.Appearance),
        Triple(Icons.Default.VerifiedUser, "Verified People", Screen.Verified),
        Triple(Icons.Default.Block, "Blocked People", Screen.Blocked),
        Triple(Icons.Default.Info, "About", Screen.About),
    )
    Page(model, "Settings") { padding ->
        Form(padding) {
            Column {
                pages.forEachIndexed { index, (icon, title, screen) ->
                    GroupRow(index, pages.size, { model.open(screen) }, leading = { Icon(icon, null) }) { Text(title) }
                }
            }
        }
    }
}

@Composable
fun Profile(model: AppModel) {
    val profile = model.profile ?: return
    var name by rememberSaveable { mutableStateOf(profile.name) }
    var picture by rememberSaveable { mutableStateOf(profile.picture) }
    val changed = validName(name) != profile.name || !picture.contentEquals(profile.picture)
    Page(model, "Profile") { padding ->
        Form(padding) {
            Panel {
                PictureField(model, picture, name) { picture = it }
                NameField(name) { name = it }
            }
            MainButton("Save", changed && validName(name) != null) { model.saveProfile(name, picture) }
        }
    }
}

private val NOISE_LEVELS = listOf(
    NoiseSuppression.OFF to "Off",
    NoiseSuppression.LOW to "Low",
    NoiseSuppression.MODERATE to "Moderate",
    NoiseSuppression.HIGH to "High",
)

@Composable
fun VoiceSettings(model: AppModel) {
    val voice = model.voice
    val slide = { changed: Voice -> model.setVoice(changed, keep = false) }
    val keep = { model.setVoice(model.voice, keep = true) }
    val titled = MaterialTheme.typography.titleMediumEmphasized
    Page(model, "Voice") { padding ->
        Form(padding) {
            Panel {
                Text("Voice activation", style = titled)
                Labeled("Sensitivity", decibels(voice.sensitivityDbfs))
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    LevelMeter(model, voice.sensitivityDbfs)
                    Slider(
                        voice.sensitivityDbfs,
                        { slide(voice.copy(sensitivityDbfs = it.roundToInt().toFloat())) },
                        valueRange = quietestGateDbfs()..0f,
                        onValueChangeFinished = keep,
                    )
                }
                SwitchRow("Speech detection", voice.speechOnly) {
                    model.setVoice(voice.copy(speechOnly = it), keep = true)
                }
                Text(
                    "Your mic sends while it is louder than the mark.",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Panel {
                Text("Processing", style = titled)
                Text("Noise suppression", style = MaterialTheme.typography.titleMedium)
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                    NOISE_LEVELS.forEachIndexed { index, (level, label) ->
                        SegmentedButton(
                            voice.noiseSuppression == level,
                            { model.setVoice(voice.copy(noiseSuppression = level), keep = true) },
                            SegmentedButtonDefaults.itemShape(index, NOISE_LEVELS.size),
                            icon = {},
                            label = { Text(label, maxLines = 1) },
                        )
                    }
                }
                SwitchRow("Automatic gain", voice.automaticGain) {
                    model.setVoice(voice.copy(automaticGain = it), keep = true)
                }
            }
            Panel {
                Text("Volume", style = titled)
                VolumeSlider("Input volume", voice.inputVolume, { slide(voice.copy(inputVolume = it)) }, keep)
                VolumeSlider("Output volume", voice.outputVolume, { slide(voice.copy(outputVolume = it)) }, keep)
            }
        }
    }
}

private fun decibels(dbfs: Float) = "${dbfs.roundToInt()} dB".replace('-', '−')

@Composable
private fun Labeled(label: String, value: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
        Text(value, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun SwitchRow(label: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        Modifier.fillMaxWidth().toggleable(checked, role = Role.Switch, onValueChange = onChange),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
        Switch(checked, null)
    }
}

/** A volume from 0 to 200 % with its percentage, which calls `done` once let go. */
@Composable
private fun VolumeSlider(label: String, volume: Float, onChange: (Float) -> Unit, done: () -> Unit) {
    Column {
        Labeled(label, "${(volume * 100).roundToInt()}%")
        Slider(volume.coerceIn(0f, 2f), onChange, valueRange = 0f..2f, onValueChangeFinished = done)
    }
}

/**
 * The mic level as a bar, in the speaking colour while the gate lets it through, with a mark at
 * `sensitivity`. Outside a call it says where the level shows.
 */
@Composable
private fun LevelMeter(model: AppModel, sensitivity: Float) {
    var level by remember { mutableStateOf<MicLevel?>(null) }
    LaunchedEffect(Unit) {
        while (true) {
            level = model.core.micLevel()
            delay(100)
        }
    }
    val shown = level ?: return Text(
        "Your level shows here while you're in a room.",
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    val scheme = MaterialTheme.colorScheme
    val fraction = { dbfs: Float -> (1 - dbfs / quietestGateDbfs()).coerceIn(0f, 1f) }
    Box(Modifier.fillMaxWidth().height(8.dp).clip(CircleShape).background(scheme.surfaceContainerHighest)) {
        Box(
            Modifier.fillMaxWidth(fraction(shown.dbfs)).fillMaxHeight()
                .background(if (shown.gateOpen) scheme.tertiary else scheme.onSurfaceVariant, CircleShape),
        )
        Row(Modifier.fillMaxSize()) {
            if (fraction(sensitivity) > 0f) Spacer(Modifier.weight(fraction(sensitivity)))
            Box(Modifier.width(2.dp).fillMaxHeight().background(scheme.onSurface))
            if (fraction(sensitivity) < 1f) Spacer(Modifier.weight(1f - fraction(sensitivity)))
        }
    }
}

@Composable
fun Appearance(model: AppModel) {
    val themes = remember { model.core.themes() }
    val picked = model.theme
    val own = remember(picked) { model.core.customPalette() }
    Page(model, "Appearance") { padding ->
        LazyVerticalGrid(
            GridCells.Adaptive(144.dp),
            Modifier.pageWidth(),
            contentPadding = padding,
            verticalArrangement = Arrangement.spacedBy(12.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            items(themes, { it.name }) { theme ->
                ThemeOption(theme.name, theme.palette, theme.name == picked.name) { model.pickTheme(theme.name) }
            }
            item(CUSTOM) { ThemeOption(CUSTOM, own, picked.name == CUSTOM) { model.pickTheme(CUSTOM) } }
            if (picked.name == CUSTOM) item(span = { GridItemSpan(maxLineSpan) }) { CustomColors(model, own) }
        }
    }
}

@Composable
private fun ThemeOption(name: String, palette: Palette, selected: Boolean, onPick: () -> Unit) {
    val shape = MaterialTheme.shapes.large
    val pick = Modifier.clip(shape).selectable(selected, role = Role.RadioButton, onClick = onPick)
    Card(pick, shape, panelColors(selected)) {
        Column(Modifier.padding(12.dp), Arrangement.spacedBy(8.dp)) {
            ThemePreview(palette)
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                RadioButton(selected, null)
                Text(name, style = MaterialTheme.typography.titleSmall)
            }
        }
    }
}

/** A small picture of a window in the colours of `palette`. */
@Composable
private fun ThemePreview(palette: Palette) {
    val shape = MaterialTheme.shapes.small
    val bar = @Composable { width: Float, rgb: UInt ->
        Box(Modifier.fillMaxWidth(width).height(6.dp).background(color(rgb), CircleShape))
    }
    Row(
        Modifier
            .fillMaxWidth()
            .height(72.dp)
            .clip(shape)
            .background(color(palette.bg))
            .border(1.dp, MaterialTheme.colorScheme.outlineVariant, shape),
    ) {
        Column(
            Modifier.width(48.dp).fillMaxHeight().background(color(palette.panel)).padding(8.dp),
            Arrangement.spacedBy(6.dp),
        ) {
            bar(1f, palette.accent)
            bar(0.8f, palette.text)
            bar(0.6f, palette.muted)
        }
        Row(Modifier.padding(8.dp), Arrangement.spacedBy(4.dp)) {
            listOf(palette.good, palette.warning, palette.bad).forEach {
                Box(Modifier.size(8.dp).background(color(it), CircleShape))
            }
        }
    }
}

@Composable
private fun CustomColors(model: AppModel, palette: Palette) {
    val colors = with(palette) { listOf(bg, panel, text, muted, accent, warning, good, bad) }
    Panel {
        Text("Colors", style = MaterialTheme.typography.titleMediumEmphasized)
        COLOR_LABELS.forEachIndexed { index, label ->
            ColorField(label, colors[index]) { rgb ->
                val own = colors.toMutableList().also { it[index] = rgb }
                model.pickCustomTheme(Palette(own[0], own[1], own[2], own[3], own[4], own[5], own[6], own[7]))
            }
        }
    }
}

@Composable
private fun ColorField(label: String, rgb: UInt, onChange: (UInt) -> Unit) {
    var hex by rememberSaveable { mutableStateOf(rgb.toString(16).padStart(6, '0').uppercase()) }
    val shape = MaterialTheme.shapes.small
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Box(Modifier.size(40.dp).background(color(rgb), shape).border(1.dp, MaterialTheme.colorScheme.outline, shape))
        OutlinedTextField(
            hex,
            { typed ->
                hex = typed.filter { it in "0123456789abcdefABCDEF" }.take(6).uppercase()
                hex.takeIf { it.length == 6 }?.toUIntOrNull(16)?.let(onChange)
            },
            Modifier.weight(1f),
            label = { Text(label) },
            prefix = { Text("#") },
            keyboardOptions = KeyboardOptions(KeyboardCapitalization.Characters, autoCorrectEnabled = false),
            singleLine = true,
        )
    }
}

@Composable
fun VerifiedPeople(model: AppModel, state: CoreState) {
    var removing by rememberSaveable { mutableStateOf<ByteArray?>(null) }
    LaunchedEffect(state.people.map { it.verified }) { model.loadVerified() }
    val people = model.verified.orEmpty()
    Page(model, "Verified People") { padding ->
        if (model.verified?.isEmpty() == true) {
            val hint = "Nobody yet. Compare safety numbers with someone to verify them."
            Hint(Icons.Default.VerifiedUser, hint, Modifier.padding(padding))
        }
        LazyColumn(Modifier.pageWidth(), contentPadding = padding) {
            itemsIndexed(people) { index, person ->
                GroupRow(
                    index,
                    people.size,
                    trailing = {
                        IconButton({ removing = person.key }) {
                            Icon(Icons.Default.RemoveModerator, "Mark as Not Verified")
                        }
                    },
                    supporting = {
                        Column {
                            if (person.servers.isNotEmpty()) Text(person.servers.joinToString(", "))
                            Text(fingerprintShort(person.key), fontFamily = FontFamily.Monospace)
                        }
                    },
                ) { Text(person.name) }
            }
        }
    }
    people.find { it.key.contentEquals(removing) }?.let { person ->
        Confirm(
            "Mark “${person.name}” as not verified?",
            "To verify them again, you compare safety numbers once more.",
            "Mark as Not Verified",
            onDismiss = { removing = null },
        ) { model.unverify(person.key) }
    }
}

@Composable
fun BlockedPeople(model: AppModel, state: CoreState) {
    LaunchedEffect(state.people.map { it.blocked }) { model.loadBlocked() }
    val people = model.blocked.orEmpty()
    Page(model, "Blocked People") { padding ->
        if (model.blocked?.isEmpty() == true) {
            Hint(Icons.Default.Block, "Nobody. Tap someone in a room to block them.", Modifier.padding(padding))
        }
        LazyColumn(Modifier.pageWidth(), contentPadding = padding) {
            itemsIndexed(people) { index, person ->
                GroupRow(
                    index,
                    people.size,
                    leading = { Avatar(model.picture(person), person.name, 40.dp) },
                    trailing = { TextButton({ model.unblock(person.key) }) { Text("Unblock") } },
                    supporting = {
                        Column {
                            person.server?.let { Text(it) }
                            Text(fingerprintShort(person.key), fontFamily = FontFamily.Monospace)
                        }
                    },
                ) { Text(person.name) }
            }
        }
    }
}

@Composable
fun About(model: AppModel) {
    val context = LocalContext.current
    val links = LocalUriHandler.current
    val version = remember { context.packageManager.getPackageInfo(context.packageName, 0).versionName.orEmpty() }
    Page(model, "About") { padding ->
        Form(padding) {
            Column {
                GroupRow(0, 2, trailing = { Text(version, style = MaterialTheme.typography.bodyLarge) }) {
                    Text("Version")
                }
                GroupRow(
                    1,
                    2,
                    {
                        try {
                            links.openUri(SOURCE)
                        } catch (_: IllegalArgumentException) {
                            model.notices.trySend("No app on this device opens web pages.")
                        }
                    },
                    trailing = { Icon(Icons.AutoMirrored.Filled.OpenInNew, null) },
                    supporting = { Text(SOURCE) },
                ) { Text("Source") }
            }
        }
    }
}
