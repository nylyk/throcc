package cc.thro.throcc

import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
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
import androidx.compose.material3.FilledTonalIconToggleButton
import androidx.compose.material3.HorizontalFloatingToolbar
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.Person

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
            Modifier.padding(padding).padding(horizontal = 16.dp),
            contentPadding = PaddingValues(bottom = 96.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (onBack != null) item(span = { GridItemSpan(maxLineSpan) }) { ConnectionBanner(model, state) }
            for (person in people) {
                val share = person.sharing.takeUnless { person.blocked } ?: continue
                item("share-${person.user}", span = { GridItemSpan(maxLineSpan) }) {
                    ShareTile(model, state, person, share)
                }
            }
            items(people, { it.user.toLong() }) { PersonCard(model, it) }
        }
    }
}

@Composable
private fun PersonCard(model: AppModel, person: Person) {
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
            val sharing = person.sharing
            Sharing(
                sharing?.label.orEmpty(),
                if (sharing == null) Modifier.alpha(0f).clearAndSetSemantics {} else Modifier,
            )
        }
    }
}

@Composable
private fun Sharing(label: String, modifier: Modifier = Modifier) {
    val quiet = MaterialTheme.colorScheme.onSurfaceVariant
    Row(modifier, verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Icon(Icons.AutoMirrored.Filled.ScreenShare, "Sharing", Modifier.size(16.dp), quiet)
        Text(
            label,
            color = quiet,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.labelLarge,
        )
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
