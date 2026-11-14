package cc.thro.throcc

import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.CircleShape
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
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
        LazyVerticalGrid(
            GridCells.Adaptive(152.dp),
            Modifier.padding(padding.exceptBottom()).padding(horizontal = 16.dp),
            contentPadding = PaddingValues(bottom = padding.calculateBottomPadding() + 96.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (onBack != null) item(span = { GridItemSpan(maxLineSpan) }) { ConnectionBanner(model, state) }
            for (person in people) {
                val share = person.sharing.takeUnless { person.blocked || person.user !in watching } ?: continue
                item("share-${person.user}", span = { GridItemSpan(maxLineSpan) }) {
                    ShareTile(model, state, person, share)
                }
            }
            items(people, { it.user.toLong() }) { PersonCard(model, it, it.user in watching) }
        }
    }
}

@Composable
private fun PersonCard(model: AppModel, person: Person, watched: Boolean) {
    val scheme = MaterialTheme.colorScheme
    val colors = if (person.blocked) {
        CardDefaults.cardColors(scheme.surfaceContainerLow, scheme.onSurfaceVariant)
    } else {
        panelColors()
    }
    val ring = if (person.speaking) scheme.tertiary else Color.Transparent
    Card({ model.sheetPerson = person.user }, shape = MaterialTheme.shapes.extraLarge, colors = colors) {
        Column(
            Modifier.fillMaxWidth().padding(16.dp),
            Arrangement.spacedBy(8.dp),
            Alignment.CenterHorizontally,
        ) {
            PersonPicture(model, person, 88.dp, Modifier.border(3.dp, ring, CircleShape).padding(6.dp))
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(
                    person.name,
                    Modifier.weight(1f, fill = false),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.titleMedium,
                )
                MutedIcon(person)
            }
            // Held empty for those not sharing, so every card has the same height.
            val share = person.sharing.takeUnless { person.blocked }
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

/** The mic, sound and leave controls, floating over the room. */
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
        FilledIconButton(
            { model.core.command(Command.Join(null)) },
            IconButtonDefaults.shapes(IconButtonDefaults.mediumRoundShape, IconButtonDefaults.mediumPressedShape),
            size,
            colors = IconButtonDefaults.filledIconButtonColors(scheme.error, scheme.onError),
        ) { Icon(Icons.Default.CallEnd, "Leave Room") }
    }
}
