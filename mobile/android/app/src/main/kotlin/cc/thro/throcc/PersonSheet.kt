package cc.thro.throcc

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Logout
import androidx.compose.material.icons.automirrored.filled.VolumeOff
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material.icons.filled.Block
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.PersonRemove
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilledTonalIconToggleButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.SheetValue
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderState
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.KeyWarning
import cc.thro.throcc.core.Person
import cc.thro.throcc.core.SafetyCheck
import cc.thro.throcc.core.Sound
import cc.thro.throcc.core.SoundLevel
import cc.thro.throcc.core.canKickFromRoom
import cc.thro.throcc.core.canRemove
import cc.thro.throcc.core.fingerprintShort
import cc.thro.throcc.core.gainToLoudness
import cc.thro.throcc.core.loudnessToGain
import kotlin.math.roundToInt

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PersonSheet(model: AppModel, state: CoreState, person: Person) {
    val close = { model.sheetPerson = null }
    MaterialTheme(colorScheme = raised(MaterialTheme.colorScheme)) {
        ModalBottomSheet(
            close,
            sheetState = rememberBottomSheetState(SheetValue.Hidden, setOf(SheetValue.Hidden, SheetValue.Expanded)),
            containerColor = MaterialTheme.colorScheme.surface,
        ) {
            Box {
                Column(
                    Modifier.verticalScroll(rememberScrollState()).padding(start = 16.dp, end = 16.dp, bottom = 24.dp),
                    Arrangement.spacedBy(16.dp),
                ) {
                    Row(Modifier.padding(start = 8.dp), Arrangement.spacedBy(16.dp), Alignment.CenterVertically) {
                        PersonPicture(model, person, 72.dp)
                        Column(Modifier.weight(1f)) {
                            Text(person.name, style = MaterialTheme.typography.headlineSmallEmphasized)
                            if (!person.blocked) {
                                Text(person.role.label, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                            if (!person.blocked && person.user != state.me) {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    TrustLabel(person.trust)
                                    person.verifiedAs?.let {
                                        Text(
                                            "Verified as $it",
                                            Modifier.padding(start = 8.dp),
                                            MaterialTheme.colorScheme.onSurfaceVariant,
                                            style = MaterialTheme.typography.labelLarge,
                                        )
                                    }
                                }
                            }
                        }
                        IconButton(close, Modifier.align(Alignment.Top)) { Icon(Icons.Default.Close, "Close") }
                    }
                    when {
                        person.blocked -> MainButton("Unblock") { model.core.command(Command.Unblock(person.user)) }
                        person.user == state.me -> Own(model)
                        else -> Other(model, state, person)
                    }
                    Moderation(model, state, person)
                }
                SnackbarHost(model.snackbars, Modifier.align(Alignment.BottomCenter))
            }
        }
    }
}

@Composable
private fun Own(model: AppModel) {
    Text(
        "To verify your safety number with someone, open their profile while they open yours. " +
            "You have a separate number with each person.",
        Modifier.padding(horizontal = 8.dp),
        MaterialTheme.colorScheme.onSurfaceVariant,
    )
    FilledTonalButton({
        model.sheetPerson = null
        model.open(Screen.Profile)
    }) { Text("Edit Profile") }
}

/** What the profile shows of the safety-number comparison, read from `check` at once. */
private class Comparison(check: SafetyCheck, name: String) {
    val groups = check.groups()
    val keyChangedWhileOpen = check.keyChangedWhileOpen()
    val canMark = check.canMark()
    val canDismissWarning = check.canDismissWarning()
    val explanation = check.explanation(name)
}

@Composable
private fun Other(model: AppModel, state: CoreState, person: Person) {
    val check = remember(person.user) { model.core.safetyCheck(person.user).apply { follow() } }
    // Counts changes to the comparison that no new state brings, so what shows is read again.
    var changes by remember(check) { mutableIntStateOf(0) }
    LaunchedEffect(check, state) {
        if (check.follow()) changes++
    }
    val shown = remember(check, state, changes, person.name) { Comparison(check, person.name) }
    val changing = { change: () -> Unit ->
        change()
        changes++
    }
    val warning = state.keyWarnings.find { it.user == person.user }
    if (warning != null) KeyChanged(person, warning, shown.canDismissWarning) { changing(check::dismissWarning) }
    if (shown.keyChangedWhileOpen) ChangedWhileOpen(person) { changing(check::compareAgain) }
    Panel { SafetyNumber(person, shown) { changing { check.mark(!person.verified) } } }
    Panel {
        Volume(model, person, Sound.VOICE, "Voice", person.voice)
        if (person.sharing?.audio == true) Volume(model, person, Sound.SHARE, "Stream audio", person.shareAudio)
    }
}

@Composable
private fun KeyChanged(person: Person, warning: KeyWarning, canDismiss: Boolean, onDismiss: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    val colors = CardDefaults.cardColors(scheme.errorContainer, scheme.onErrorContainer)
    Card(shape = MaterialTheme.shapes.extraLarge, colors = colors) {
        Column(Modifier.fillMaxWidth().padding(20.dp), Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Icon(Icons.Default.Warning, null)
                Text("${person.name}’s key changed", style = MaterialTheme.typography.titleMediumEmphasized)
            }
            Text("${person.name}’s new key doesn’t match the one you verified. Someone might be listening in.")
            listOf("Before" to warning.verified, "Now" to warning.now).forEach { (label, key) ->
                Row {
                    Text(label, Modifier.width(64.dp))
                    Text(fingerprintShort(key), fontFamily = FontFamily.Monospace)
                }
            }
            TextButton(
                onDismiss,
                Modifier.align(Alignment.End),
                canDismiss,
                colors = ButtonDefaults.textButtonColors(contentColor = scheme.onErrorContainer),
            ) { Text("Dismiss Warning") }
        }
    }
}

/** Says the number on screen is a new one, which the user compares again before acting on it. */
@Composable
private fun ChangedWhileOpen(person: Person, onCompareAgain: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    Card(shape = MaterialTheme.shapes.extraLarge, colors = CardDefaults.cardColors(scheme.surfaceContainerHigh)) {
        Column(Modifier.fillMaxWidth().padding(20.dp), Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Icon(Icons.Default.Warning, null, tint = LocalWarning.current)
                Text("${person.name}’s key changed while this was open, so this is a new number. Compare it again.")
            }
            TextButton(onCompareAgain, Modifier.align(Alignment.End)) { Text("Compare Again") }
        }
    }
}

@Composable
private fun SafetyNumber(person: Person, shown: Comparison, onMark: () -> Unit) {
    val quiet = MaterialTheme.colorScheme.onSurfaceVariant
    if (shown.groups.isEmpty()) {
        Text("Their safety number appears once you share a room.", color = quiet)
    } else {
        Text(
            shown.groups.chunked(4).joinToString("\n") { it.joinToString(" ") },
            Modifier.fillMaxWidth(),
            fontFamily = FontFamily.Monospace,
            textAlign = TextAlign.Center,
            style = MaterialTheme.typography.titleMedium,
        )
    }
    shown.explanation?.let { Text(it, color = quiet, style = MaterialTheme.typography.bodyMedium) }
    if (person.verified) {
        OutlinedButton(onMark, Modifier.fillMaxWidth(), shown.canMark) { Text("Mark as Not Verified") }
    } else {
        MainButton("Mark as Verified", shown.canMark, onClick = onMark)
    }
}

@Composable
private fun Volume(model: AppModel, person: Person, sound: Sound, label: String, level: SoundLevel) {
    val volume = remember(person.user, sound) { SliderState(gainToLoudness(level.volume), trackRange = 0f..2f) }
    val set = { changed: SoundLevel -> model.core.command(Command.SetSoundLevel(person.user, sound, changed)) }
    Column {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(label, Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
            Text("${(volume.value * 100).roundToInt()}%", color = MaterialTheme.colorScheme.onSurfaceVariant)
            FilledTonalIconToggleButton(
                level.muted,
                { set(SoundLevel(level.volume, it)) },
                IconButtonDefaults.toggleableShapes(),
            ) {
                val icon = if (level.muted) Icons.AutoMirrored.Filled.VolumeOff else Icons.AutoMirrored.Filled.VolumeUp
                Icon(icon, "Mute")
            }
        }
        Slider(volume, {
            volume.value = it
            set(SoundLevel(loudnessToGain(it), level.muted))
        })
    }
}

private class Action(val icon: ImageVector, val label: String, val danger: Boolean = false, val run: () -> Unit)

/** Blocking `person`, and kicking and removing them where our role allows it. */
@Composable
private fun Moderation(model: AppModel, state: CoreState, person: Person) {
    if (person.user == state.me) return
    var removing by rememberSaveable(person.user) { mutableStateOf(false) }
    val role = state.role
    val room = person.room
    val actions = listOfNotNull(
        Action(Icons.Default.Block, "Block") { model.core.command(Command.Block(person.user)) }
            .takeIf { !person.blocked },
        Action(Icons.AutoMirrored.Filled.Logout, "Kick from Room") {
            room?.let { model.core.command(Command.KickFromRoom(person.user, it)) }
        }.takeIf { role != null && room != null && canKickFromRoom(role, person.role) },
        Action(Icons.Default.PersonRemove, "Remove from Server", danger = true) { removing = true }
            .takeIf { role != null && canRemove(role, person.role) },
    )
    Column {
        actions.forEachIndexed { index, action ->
            val color = if (action.danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface
            GroupRow(index, actions.size, action.run, leading = { Icon(action.icon, null, tint = color) }) {
                Text(action.label, color = color)
            }
        }
    }
    if (removing) {
        RemoveDialog(person.name, { removing = false }) { cascade ->
            model.core.command(Command.RemoveUser(person.user, cascade))
            model.sheetPerson = null
        }
    }
}
