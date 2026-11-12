package cc.thro.throcc

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
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
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
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
import java.io.Serializable

private sealed interface RoomDialog : Serializable {
    data object New : RoomDialog
    data class Edit(val room: ULong) : RoomDialog
    data class Delete(val room: ULong) : RoomDialog
    data class Password(val room: ULong) : RoomDialog
}

@Composable
fun Rooms(model: AppModel, state: CoreState) {
    val permissions = state.role?.let(::permissionsOf)
    val managesRooms = permissions?.manageRooms == true
    var dialog by rememberSaveable { mutableStateOf<RoomDialog?>(null) }
    var leaving by rememberSaveable { mutableStateOf(false) }
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
                dialog = RoomDialog.Password(room.id)
            }
            else -> model.core.command(Command.Join(room.id))
        }
    }
    Page(
        model,
        server,
        subtitle = state.server?.address.takeIf { state.server?.name != null },
        actions = {
            if (managesRooms) IconButton({ dialog = RoomDialog.New }) { Icon(Icons.Default.Add, "New room") }
            OverflowMenu { close ->
                pages.forEach { (title, screen) ->
                    DropdownMenuItem({ Text(title) }, {
                        close()
                        model.open(screen)
                    })
                }
                DropdownMenuItem({ Text("Disconnect") }, model::disconnect)
                DangerItem("Leave Server") {
                    close()
                    leaving = true
                }
            }
        },
    ) { padding ->
        LazyColumn(Modifier.pageWidth(), contentPadding = padding) {
            item("connection") { ConnectionBanner(model, state) }
            if (state.server != null && state.rooms.isEmpty()) {
                item("hint") { Hint(Icons.AutoMirrored.Filled.VolumeUp, "No rooms yet.") }
            }
            items(state.rooms, { "room ${it.id}" }) { room ->
                RoomCard(model, state, room, { join(room) }) {
                    if (managesRooms) {
                        OverflowMenu { close ->
                            DropdownMenuItem({ Text("Edit Room…") }, {
                                close()
                                dialog = RoomDialog.Edit(room.id)
                            })
                            DangerItem("Delete Room") {
                                close()
                                dialog = RoomDialog.Delete(room.id)
                            }
                        }
                    }
                }
            }
            val lobby = state.people.filter { person -> state.rooms.none { it.id == person.room } }
            if (lobby.isNotEmpty()) {
                item("lobby") {
                    GroupTitle("Lobby")
                    Card(shape = MaterialTheme.shapes.extraLarge, colors = panelColors()) {
                        Column(Modifier.padding(vertical = 8.dp)) { lobby.forEach { PersonRow(model, state, it) } }
                    }
                }
            }
        }
    }
    fun room(id: ULong) = state.rooms.find { it.id == id }
    when (val open = dialog) {
        null -> {}
        RoomDialog.New -> NewRoom(model) { dialog = null }
        is RoomDialog.Edit -> room(open.room)?.let { EditRoom(model, it) { dialog = null } }
        is RoomDialog.Delete -> room(open.room)?.let { room ->
            Confirm(
                "Delete “${room.name}”?",
                if (state.people.any { it.room == room.id }) {
                    "Everyone in it goes back to the lobby. This can’t be undone."
                } else {
                    "This can’t be undone."
                },
                "Delete",
                onDismiss = { dialog = null },
            ) { model.core.command(Command.DeleteRoom(room.id)) }
        }
        is RoomDialog.Password -> room(open.room)?.let { room ->
            JoinLocked(model, room, state.current?.room == room.id) { dialog = null }
        }
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

/** A room with the people in it. Tapping it joins the room, or opens the one we are in. */
@Composable
private fun RoomCard(model: AppModel, state: CoreState, room: Room, onJoin: () -> Unit, menu: @Composable () -> Unit) {
    val people = state.people.filter { it.room == room.id }
    val current = state.current?.room == room.id
    val facts = listOfNotNull(
        "You’re here".takeIf { current },
        "Locked".takeIf { room.locked },
        people(people.size),
    )
    val shape = MaterialTheme.shapes.extraLarge
    Card(onJoin, Modifier.padding(bottom = 12.dp), shape = shape, colors = panelColors(current)) {
        Row(
            Modifier.padding(start = 20.dp, top = 12.dp, end = 8.dp, bottom = 4.dp),
            Arrangement.spacedBy(16.dp),
            Alignment.CenterVertically,
        ) {
            Icon(if (room.locked) Icons.Default.Lock else Icons.AutoMirrored.Filled.VolumeUp, null)
            Column(Modifier.weight(1f).heightIn(min = 48.dp), Arrangement.Center) {
                Text(
                    room.name,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.titleMediumEmphasized,
                )
                Text(
                    facts.joinToString(" · "),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            menu()
        }
        Column(Modifier.padding(bottom = 8.dp)) { people.forEach { PersonRow(model, state, it) } }
    }
}

@Composable
private fun PersonRow(model: AppModel, state: CoreState, person: Person) {
    val scheme = MaterialTheme.colorScheme
    val quiet = scheme.onSurfaceVariant
    val small = Modifier.size(20.dp)
    Row(
        Modifier
            .fillMaxWidth()
            .clickable { model.sheetPerson = person.user }
            .heightIn(min = 48.dp)
            .padding(horizontal = 20.dp),
        Arrangement.spacedBy(12.dp),
        Alignment.CenterVertically,
    ) {
        PersonPicture(model, person, 32.dp)
        Text(
            person.name,
            Modifier.weight(1f),
            if (person.blocked) quiet else scheme.onSurface,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        if (!person.blocked) {
            when (state.trust(person)) {
                Trust.KeyChanged -> Icon(Icons.Default.Warning, "Key changed", small, scheme.error)
                Trust.Unverified -> Icon(Icons.Outlined.GppMaybe, "Not verified", small, quiet)
                Trust.Verified -> {}
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
            Button(
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
    var name by rememberSaveable { mutableStateOf("") }
    var password by rememberSaveable { mutableStateOf("") }
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
    var name by rememberSaveable(room.id) { mutableStateOf(room.name) }
    var password by rememberSaveable(room.id) { mutableStateOf("") }
    var locked by rememberSaveable(room.id) { mutableStateOf(room.locked) }
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
    var password by rememberSaveable(room.id) { mutableStateOf("") }
    LaunchedEffect(joined) {
        if (joined) onDismiss()
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            Button(
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
