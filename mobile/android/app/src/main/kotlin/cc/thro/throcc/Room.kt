package cc.thro.throcc

import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.material.icons.filled.Headset
import androidx.compose.material.icons.filled.HeadsetOff
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material3.BottomAppBar
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.FilledTonalIconToggleButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
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
        if (onBack == null && state.server != null) {
            Hint(if (state.rooms.isEmpty()) "There are no rooms yet." else "Join a room on the left.")
        }
        return
    }
    Scaffold(topBar = { TopBar(room.name, onBack) }, bottomBar = { RoomBar(model, state) }) { padding ->
        Column(Modifier.padding(padding)) {
            ConnectionBanner(model, state)
            LazyVerticalGrid(
                GridCells.Adaptive(144.dp),
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                items(state.people.filter { it.room == room.id }, { it.user.toLong() }) { PersonCard(model, it) }
            }
        }
    }
}

@Composable
private fun PersonCard(model: AppModel, person: Person) {
    val scheme = MaterialTheme.colorScheme
    val colors = if (person.blocked) {
        CardDefaults.cardColors(scheme.surfaceContainerLow, scheme.outline)
    } else {
        CardDefaults.cardColors()
    }
    val ring = if (person.speaking) scheme.tertiary else Color.Transparent
    Card({ model.sheet = person.user }, colors = colors) {
        Column(
            Modifier.fillMaxWidth().padding(16.dp),
            Arrangement.spacedBy(8.dp),
            Alignment.CenterHorizontally,
        ) {
            PersonPicture(model, person, 72.dp, Modifier.border(3.dp, ring, CircleShape).padding(6.dp))
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(person.name, Modifier.weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis)
                MutedIcon(person)
            }
            person.sharing?.let {
                Text(
                    "Sharing ${it.label}",
                    color = scheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.labelMedium,
                )
            }
        }
    }
}

@Composable
private fun RoomBar(model: AppModel, state: CoreState) {
    BottomAppBar {
        Row(Modifier.fillMaxWidth(), Arrangement.SpaceEvenly, Alignment.CenterVertically) {
            FilledTonalIconToggleButton(!state.mic, { model.core.command(Command.SetMic(!it)) }) {
                if (state.mic) Icon(Icons.Default.Mic, "Mute") else Icon(Icons.Default.MicOff, "Unmute")
            }
            FilledTonalIconToggleButton(state.deafened, { model.core.command(Command.SetDeafened(it)) }) {
                if (state.deafened) {
                    Icon(Icons.Default.HeadsetOff, "Undeafen")
                } else {
                    Icon(Icons.Default.Headset, "Deafen")
                }
            }
            FilledIconButton(
                { model.core.command(Command.Join(null)) },
                colors = IconButtonDefaults.filledIconButtonColors(
                    MaterialTheme.colorScheme.error,
                    MaterialTheme.colorScheme.onError,
                ),
            ) { Icon(Icons.Default.CallEnd, "Leave Room") }
        }
    }
}
