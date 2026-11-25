package cc.thro.throcc

import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ScreenShare
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.material.icons.filled.Headset
import androidx.compose.material.icons.filled.HeadsetOff
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FabPosition
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilledTonalIconToggleButton
import androidx.compose.material3.HorizontalFloatingToolbar
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.key
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.Person
import cc.thro.throcc.core.Share

/** The room we are in, with `onBack` where it covers the pages. */
@Composable
fun RoomPane(model: AppModel, state: CoreState, onBack: (() -> Unit)?) {
    val room = state.rooms.find { it.id == state.current?.room }
    if (room == null) {
        if (onBack == null) {
            val hint = when {
                state.server == null -> "Connect to a server to join a room."
                state.rooms.isEmpty() -> "There are no rooms yet."
                else -> "Join a room on the left."
            }
            Hint(Icons.AutoMirrored.Filled.VolumeUp, hint)
        }
        return
    }
    val people = state.people.filter { it.room == room.id }
    val watching = state.current?.watching.orEmpty()
    Page(
        model,
        room.name,
        onBack,
        largeTitle = false,
        notices = onBack != null,
        floatingButton = { RoomControls(model, state) },
        buttonPosition = FabPosition.Center,
    ) { padding ->
        BoxWithConstraints(Modifier.padding(padding.exceptBottom()).fillMaxSize()) {
            val spacing = 12.dp
            val width = maxWidth - 32.dp
            val columns = ((width + spacing) / (CARD_MIN_WIDTH + spacing)).toInt().coerceAtLeast(1)
            // Whole pixels, so that a full row's cards and gaps never add up past the row and wrap.
            val cardWidth = with(LocalDensity.current) { ((width - spacing * (columns - 1)) / columns).toPx().toInt().toDp() }
            Column(
                Modifier
                    .verticalScroll(rememberScrollState())
                    .heightIn(min = maxHeight)
                    .padding(horizontal = 16.dp)
                    .padding(bottom = padding.calculateBottomPadding() + 96.dp),
                Arrangement.spacedBy(spacing),
            ) {
                if (onBack != null) ConnectionBanner(model, state)
                Spacer(Modifier.weight(1f))
                for (person in people) {
                    val share = person.sharing.takeUnless { person.blocked || person.user !in watching } ?: continue
                    key("share-${person.user}") { ShareTile(model, state, person, share) }
                }
                FlowRow(
                    Modifier.fillMaxWidth(),
                    Arrangement.spacedBy(spacing, Alignment.CenterHorizontally),
                    Arrangement.spacedBy(spacing),
                ) {
                    for (person in people) {
                        key(person.user) {
                            PersonCard(model, person, person.user in watching, Modifier.width(cardWidth))
                        }
                    }
                }
                Spacer(Modifier.weight(1f))
            }
        }
    }
}

private val CARD_MIN_WIDTH = 152.dp

@Composable
private fun PersonCard(model: AppModel, person: Person, watched: Boolean, modifier: Modifier) {
    val scheme = MaterialTheme.colorScheme
    val colors = if (person.blocked) {
        CardDefaults.cardColors(scheme.surfaceContainerLow, scheme.onSurfaceVariant)
    } else {
        panelColors()
    }
    val ring = if (person.speaking) scheme.tertiary else Color.Transparent
    Card({ model.sheetPerson = person.user }, modifier, shape = MaterialTheme.shapes.extraLarge, colors = colors) {
        Column(
            Modifier.fillMaxWidth().padding(16.dp),
            Arrangement.spacedBy(8.dp),
            Alignment.CenterHorizontally,
        ) {
            PersonPicture(model, person, 88.dp, Modifier.border(3.dp, ring, CircleShape).padding(6.dp))
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                OneLineText(
                    person.name,
                    Modifier.weight(1f, fill = false),
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.titleMedium,
                )
                MutedIcon(person)
            }
            // Held empty for those not sharing, so every card has the same height.
            val share = person.sharing.takeUnless { person.blocked || person.user == model.state.value.me }
            if (share == null) Spacer(Modifier.height(WATCH_HEIGHT)) else WatchButton(model, person, share, watched)
        }
    }
}

private val WATCH_HEIGHT = 32.dp

/** Starts watching `person`'s share in the room, or says it is the one being watched. */
@Composable
private fun WatchButton(model: AppModel, person: Person, share: Share, watched: Boolean) {
    FilledTonalButton(
        { model.core.watchOnly(person.user) },
        Modifier.height(WATCH_HEIGHT).semantics { contentDescription = "Watch ${person.name}’s share, ${share.label}" },
        enabled = !watched,
        contentPadding = PaddingValues(horizontal = 12.dp),
    ) {
        Icon(Icons.AutoMirrored.Filled.ScreenShare, null, Modifier.size(16.dp))
        Spacer(Modifier.width(6.dp))
        Text(if (watched) "Watching" else "Watch", style = MaterialTheme.typography.labelLarge)
    }
}

/** The mic, sound, share and leave controls, floating over the room. */
@Composable
fun RoomControls(model: AppModel, state: CoreState) {
    val scheme = MaterialTheme.colorScheme
    val size = Modifier.padding(horizontal = 4.dp).size(IconButtonDefaults.mediumContainerSize())
    val shapes = IconButtonDefaults.toggleableShapes(
        IconButtonDefaults.mediumRoundShape,
        IconButtonDefaults.mediumPressedShape,
        IconButtonDefaults.mediumSelectedRoundShape,
    )
    val mic = state.mic && !model.micDenied
    val setMic = { on: Boolean ->
        if (model.micDenied) {
            model.notices.trySend("To talk, allow the microphone for throcc in the system settings.")
        } else {
            model.core.command(Command.SetMic(on))
        }
    }
    HorizontalFloatingToolbar(expanded = true) {
        FilledTonalIconToggleButton(!mic, { setMic(!it) }, shapes, size) {
            Icon(if (mic) Icons.Default.Mic else Icons.Default.MicOff, "Mute")
        }
        FilledTonalIconToggleButton(state.deafened, { model.core.command(Command.SetDeafened(it)) }, shapes, size) {
            Icon(if (state.deafened) Icons.Default.HeadsetOff else Icons.Default.Headset, "Deafen")
        }
        if (model.core.canShare()) {
            ShareScreenButton(model, state.current?.sharing == true, shapes, size)
        }
        FilledIconButton(
            { model.core.command(Command.Join(null)) },
            IconButtonDefaults.shapes(IconButtonDefaults.mediumRoundShape, IconButtonDefaults.mediumPressedShape),
            size,
            colors = IconButtonDefaults.filledIconButtonColors(scheme.error, scheme.onError),
        ) { Icon(Icons.Default.CallEnd, "Leave Room") }
    }
}
