package cc.thro.throcc

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.VolumeOff
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconToggleButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.KeyWarning
import cc.thro.throcc.core.Person
import cc.thro.throcc.core.Sound
import cc.thro.throcc.core.SoundLevel
import cc.thro.throcc.core.canKickFromRoom
import cc.thro.throcc.core.canRemove
import cc.thro.throcc.core.fingerprintShort
import kotlin.math.roundToInt

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PersonSheet(model: AppModel, state: CoreState, person: Person) {
    ModalBottomSheet({ model.sheet = null }, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(
            Modifier.verticalScroll(rememberScrollState()).padding(start = 24.dp, end = 24.dp, bottom = 24.dp),
            Arrangement.spacedBy(16.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                PersonPicture(model, person, 72.dp)
                Column {
                    Text(person.name, style = MaterialTheme.typography.titleLarge)
                    if (!person.blocked) Text(person.role.label, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    if (!person.blocked && person.user != state.me) TrustLabel(state.trust(person))
                }
            }
            when {
                person.blocked -> Button({ model.core.command(Command.Unblock(person.user)) }) { Text("Unblock") }
                person.user == state.me -> Own(model)
                else -> Other(model, state, person)
            }
            Moderation(model, state, person)
        }
    }
}

@Composable
private fun Own(model: AppModel) {
    Text(
        "To verify your safety number with someone, open their profile while they open yours. " +
            "You have a separate number with each person.",
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    OutlinedButton({
        model.sheet = null
        model.open(Screen.Profile)
    }) { Text("Edit Profile") }
}

@Composable
private fun Other(model: AppModel, state: CoreState, person: Person) {
    val warning = state.keyWarnings.find { it.user == person.user }
    if (warning != null) KeyChanged(model, person, warning)
    SafetyNumber(model, person, warning)
    Volume(model, person, Sound.VOICE, "Voice", person.voice)
    if (person.sharing?.audio == true) Volume(model, person, Sound.SHARE, "Stream audio", person.shareAudio)
    OutlinedButton({ model.core.command(Command.Block(person.user)) }) { Text("Block") }
}

@Composable
private fun KeyChanged(model: AppModel, person: Person, warning: KeyWarning) {
    val scheme = MaterialTheme.colorScheme
    Card(colors = CardDefaults.cardColors(scheme.errorContainer, scheme.onErrorContainer)) {
        Column(Modifier.fillMaxWidth().padding(16.dp), Arrangement.spacedBy(8.dp)) {
            Text("${person.name}’s key changed", style = MaterialTheme.typography.titleMedium)
            Text("${person.name}’s new key doesn’t match the one you verified. Someone might be listening in.")
            listOf("Before" to warning.verified, "Now" to warning.now).forEach { (label, key) ->
                Row {
                    Text(label, Modifier.width(64.dp))
                    Text(fingerprintShort(key), fontFamily = FontFamily.Monospace)
                }
            }
            TextButton(
                { model.core.command(Command.DismissKeyChange(person.user, person.identityKey)) },
                Modifier.align(Alignment.End),
                colors = ButtonDefaults.textButtonColors(contentColor = scheme.onErrorContainer),
            ) { Text("Dismiss Warning") }
        }
    }
}

@Composable
private fun SafetyNumber(model: AppModel, person: Person, warning: KeyWarning?) {
    val key = person.identityKey
    val number = remember(key?.toHexString()) { key?.let(model.core::safetyNumber) }
    val quiet = MaterialTheme.colorScheme.onSurfaceVariant
    if (key == null || number == null) {
        Text("Their safety number appears once you share a room.", color = quiet)
        return
    }
    Text(
        number.chunked(5).chunked(4).joinToString("\n") { it.joinToString(" ") },
        Modifier.fillMaxWidth(),
        fontFamily = FontFamily.Monospace,
        textAlign = TextAlign.Center,
        style = MaterialTheme.typography.titleMedium,
    )
    Text(
        if (warning?.now.contentEquals(key)) {
            "This number belongs to ${person.name}’s new key. " +
                "Compare it with theirs in person or through another app before you trust it."
        } else {
            "To check that your calls with ${person.name} are end-to-end encrypted, " +
                "compare these numbers with theirs in person or through another app."
        },
        color = quiet,
        style = MaterialTheme.typography.bodySmall,
    )
    val mark = { model.core.command(Command.SetVerified(person.user, key, !person.verified)) }
    if (person.verified) {
        OutlinedButton(mark) { Text("Mark as Not Verified") }
    } else {
        Button(mark) { Text("Mark as Verified") }
    }
}

@Composable
private fun Volume(model: AppModel, person: Person, sound: Sound, label: String, level: SoundLevel) {
    var volume by remember(person.user, sound) { mutableFloatStateOf(level.volume.coerceIn(0f, 2f)) }
    val set = { changed: SoundLevel -> model.core.command(Command.SetSoundLevel(person.user, sound, changed)) }
    Column {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(label, Modifier.weight(1f))
            Text("${(volume * 100).roundToInt()}%", color = MaterialTheme.colorScheme.onSurfaceVariant)
            IconToggleButton(level.muted, { set(SoundLevel(volume, it)) }) {
                if (level.muted) {
                    Icon(Icons.AutoMirrored.Filled.VolumeOff, "Unmute")
                } else {
                    Icon(Icons.AutoMirrored.Filled.VolumeUp, "Mute")
                }
            }
        }
        Slider(
            volume,
            {
                volume = it
                set(SoundLevel(it, level.muted))
            },
            valueRange = 0f..2f,
        )
    }
}

/** Kicking and removing `person`, where our role allows it. */
@Composable
private fun Moderation(model: AppModel, state: CoreState, person: Person) {
    val role = state.role
    if (role == null || person.user == state.me) return
    var removing by remember { mutableStateOf(false) }
    val danger = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error)
    val room = person.room
    if (room != null && canKickFromRoom(role, person.role)) {
        TextButton({ model.core.command(Command.KickFromRoom(person.user, room)) }, colors = danger) {
            Text("Kick from Room")
        }
    }
    if (canRemove(role, person.role)) {
        TextButton({ removing = true }, colors = danger) { Text("Remove from Server") }
    }
    if (removing) {
        RemoveDialog(person.name, { removing = false }) { cascade ->
            model.core.command(Command.RemoveUser(person.user, cascade))
            model.sheet = null
        }
    }
}
