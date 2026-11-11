package cc.thro.throcc

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ScreenShare
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material.icons.outlined.GppMaybe
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.Person
import cc.thro.throcc.core.Room
import cc.thro.throcc.core.isValidPassword
import cc.thro.throcc.core.limits
import cc.thro.throcc.core.permissionsOf

private sealed interface RoomDialog {
    data object New : RoomDialog
    data class Edit(val room: Room) : RoomDialog
    data class Delete(val room: Room) : RoomDialog
    data class Password(val room: Room) : RoomDialog
}

@Composable
fun Rooms(model: AppModel, state: CoreState) {
    val permissions = state.role?.let(::permissionsOf)
    val managesRooms = permissions?.manageRooms == true
    var dialog by remember { mutableStateOf<RoomDialog?>(null) }
    var leaving by remember { mutableStateOf(false) }
    val server = state.server?.let { it.name ?: it.address }.orEmpty()
    val pages = listOfNotNull(
        "Server" to Screen.Server,
        ("Members" to Screen.Members).takeIf { permissions?.listUsers == true },
        "Invite People" to Screen.Invites,
        "Privacy and Keys" to Screen.Keys,
    )
    fun join(room: Room) {
        when {
            state.current?.room == room.id -> model.roomInFront = true
            room.locked && !managesRooms -> {
                model.passwordRefused = null
                dialog = RoomDialog.Password(room)
            }
            else -> model.core.command(Command.Join(room.id))
        }
    }
    Scaffold(
        topBar = {
            TopBar(server, model::back) {
                if (managesRooms) IconButton({ dialog = RoomDialog.New }) { Icon(Icons.Default.Add, "New room") }
                OverflowMenu { close ->
                    pages.forEach { (title, screen) ->
                        DropdownMenuItem({ Text(title) }, {
                            close()
                            model.open(screen)
                        })
                    }
                    DropdownMenuItem({ Text("Disconnect") }, model::disconnect)
                    DropdownMenuItem({ Text("Leave Server") }, {
                        close()
                        leaving = true
                    })
                }
            }
        },
    ) { padding ->
        Column(Modifier.padding(padding)) {
            ConnectionBanner(model, state)
            if (state.server != null && state.rooms.isEmpty()) {
                Text("No rooms yet.", Modifier.padding(16.dp), MaterialTheme.colorScheme.onSurfaceVariant)
            }
            LazyColumn {
                state.rooms.forEach { room ->
                    item("room ${room.id}") {
                        RoomRow(room, state.current?.room == room.id, { join(room) }) {
                            if (managesRooms) {
                                OverflowMenu { close ->
                                    DropdownMenuItem({ Text("Edit Room…") }, {
                                        close()
                                        dialog = RoomDialog.Edit(room)
                                    })
                                    DropdownMenuItem({ Text("Delete Room") }, {
                                        close()
                                        dialog = RoomDialog.Delete(room)
                                    })
                                }
                            }
                        }
                    }
                    items(state.people.filter { it.room == room.id }, { "person ${it.user}" }) {
                        PersonRow(model, state, it)
                    }
                }
                val lobby = state.people.filter { person -> state.rooms.none { it.id == person.room } }
                if (lobby.isNotEmpty()) item("lobby") { SectionTitle("Lobby") }
                items(lobby, { "person ${it.user}" }) { PersonRow(model, state, it) }
            }
        }
    }
    when (val open = dialog) {
        null -> {}
        RoomDialog.New -> NewRoom(model) { dialog = null }
        is RoomDialog.Edit -> EditRoom(model, open.room) { dialog = null }
        is RoomDialog.Delete -> Confirm(
            "Delete “${open.room.name}”?",
            if (state.people.any { it.room == open.room.id }) {
                "Everyone in it goes back to the lobby. This can’t be undone."
            } else {
                "This can’t be undone."
            },
            "Delete",
            onDismiss = { dialog = null },
        ) { model.core.command(Command.DeleteRoom(open.room.id)) }
        is RoomDialog.Password -> JoinLocked(model, open.room, state.current?.room == open.room.id) { dialog = null }
    }
    if (leaving) {
        Confirm(
            "Leave “$server”?",
            "You lose your membership and the server leaves your list. Coming back needs a new invite.",
            "Leave",
            onDismiss = { leaving = false },
        ) { model.core.command(Command.LeaveServer) }
    }
}

@Composable
private fun RoomRow(room: Room, current: Boolean, onJoin: () -> Unit, menu: @Composable () -> Unit) {
    val container = if (current) MaterialTheme.colorScheme.secondaryContainer else ListItemDefaults.containerColor
    ListItem(
        headlineContent = { Text(room.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        modifier = Modifier.clickable(onClick = onJoin),
        leadingContent = {
            if (room.locked) Icon(Icons.Default.Lock, "Locked") else Icon(Icons.AutoMirrored.Filled.VolumeUp, null)
        },
        trailingContent = menu,
        colors = ListItemDefaults.colors(containerColor = container),
    )
}

@Composable
private fun PersonRow(model: AppModel, state: CoreState, person: Person) {
    val scheme = MaterialTheme.colorScheme
    val quiet = scheme.onSurfaceVariant
    val small = Modifier.size(18.dp)
    Row(
        Modifier
            .fillMaxWidth()
            .clickable { model.sheet = person.user }
            .padding(start = 56.dp, end = 24.dp, top = 8.dp, bottom = 8.dp),
        Arrangement.spacedBy(12.dp),
        Alignment.CenterVertically,
    ) {
        PersonPicture(model, person, 28.dp)
        Row(Modifier.weight(1f), Arrangement.spacedBy(6.dp), Alignment.CenterVertically) {
            val color = if (person.blocked) scheme.outline else quiet
            Text(person.name, Modifier.weight(1f, fill = false), color, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (!person.blocked) {
                when (state.trust(person)) {
                    Trust.KeyChanged -> Icon(Icons.Default.Warning, "Key changed", small, scheme.error)
                    Trust.Unverified -> Icon(Icons.Outlined.GppMaybe, "Not verified", small, scheme.outline)
                    Trust.Verified -> {}
                }
            }
        }
        if (person.sharing != null) Icon(Icons.AutoMirrored.Filled.ScreenShare, "Sharing", small, quiet)
        MutedIcon(person)
    }
}

@Composable
private fun PasswordField(password: String, hint: String, refused: String? = null, onChange: (String) -> Unit) {
    val tooLong = password.isNotEmpty() && !isValidPassword(password)
    val error = refused ?: "Passwords can be at most ${limits().passwordChars} characters.".takeIf { tooLong }
    OutlinedTextField(
        password,
        onChange,
        Modifier.fillMaxWidth(),
        label = { Text("Password") },
        supportingText = { Text(error ?: hint) },
        isError = error != null,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
        singleLine = true,
    )
}

/** A dialog of room fields with Cancel and one action, which also closes it. */
@Composable
private fun RoomForm(
    title: String,
    action: String,
    enabled: Boolean,
    onDismiss: () -> Unit,
    onAction: () -> Unit,
    fields: @Composable () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            TextButton(
                {
                    onAction()
                    onDismiss()
                },
                enabled = enabled,
            ) { Text(action) }
        },
        dismissButton = { TextButton(onDismiss) { Text("Cancel") } },
        title = { Text(title) },
        text = { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) { fields() } },
    )
}

@Composable
private fun NewRoom(model: AppModel, onDismiss: () -> Unit) {
    var name by remember { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    val valid = validName(name)
    RoomForm(
        "New Room",
        "Create",
        valid != null && (password.isEmpty() || isValidPassword(password)),
        onDismiss,
        { valid?.let { model.core.command(Command.CreateRoom(it, password.ifEmpty { null })) } },
    ) {
        NameField(name) { name = it }
        PasswordField(password, "Leave it empty to let anyone join.") { password = it }
    }
}

@Composable
private fun EditRoom(model: AppModel, room: Room, onDismiss: () -> Unit) {
    var name by remember { mutableStateOf(room.name) }
    var password by remember { mutableStateOf("") }
    var locked by remember { mutableStateOf(room.locked) }
    val valid = validName(name)
    RoomForm(
        "Edit Room",
        "Save",
        valid != null && (password.isEmpty() || isValidPassword(password)),
        onDismiss,
        {
            if (valid != null && valid != room.name) model.core.command(Command.RenameRoom(room.id, valid))
            when {
                password.isNotEmpty() -> model.core.command(Command.SetRoomPassword(room.id, password))
                room.locked && !locked -> model.core.command(Command.SetRoomPassword(room.id, null))
            }
        },
    ) {
        NameField(name) { name = it }
        PasswordField(
            password,
            if (locked) {
                "Members need the password to join. Type a new one to change it."
            } else {
                "Anyone can join. Type a password to lock the room."
            },
        ) { password = it }
        if (locked) TextButton({ locked = false }) { Text("Remove Password") }
    }
}

@Composable
private fun JoinLocked(model: AppModel, room: Room, joined: Boolean, onDismiss: () -> Unit) {
    var password by remember { mutableStateOf("") }
    LaunchedEffect(joined) {
        if (joined) onDismiss()
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            TextButton(
                { model.core.command(Command.JoinWithPassword(room.id, password)) },
                enabled = isValidPassword(password),
            ) { Text("Join") }
        },
        dismissButton = { TextButton(onDismiss) { Text("Cancel") } },
        title = { Text("Join “${room.name}”") },
        text = {
            PasswordField(password, "This room is locked. Enter its password to join.", model.passwordRefused) {
                password = it
                model.passwordRefused = null
            }
        },
    )
}
