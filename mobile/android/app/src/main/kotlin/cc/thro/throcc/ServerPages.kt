package cc.thro.throcc

import android.content.ClipData
import android.content.Intent
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ButtonGroup
import androidx.compose.material3.ButtonGroupScope
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.NameChange
import cc.thro.throcc.core.PictureChange
import cc.thro.throcc.core.Role
import cc.thro.throcc.core.Server
import cc.thro.throcc.core.User
import cc.thro.throcc.core.canRemove
import cc.thro.throcc.core.canSetRole
import cc.thro.throcc.core.fingerprintShort
import cc.thro.throcc.core.fingerprintText
import kotlinx.coroutines.launch

@Composable
fun ServerOverview(model: AppModel, state: CoreState) {
    val server = state.server
    Page(model, "Server") { padding ->
        if (server == null) return@Page
        Form(padding) {
            if (state.permissions?.editServer == true) {
                EditServer(model, server)
            } else {
                val name = server.title
                Row(Modifier, Arrangement.spacedBy(16.dp), Alignment.CenterVertically) {
                    Avatar(model.picture(server.avatar), name, 80.dp)
                    Text(name, style = MaterialTheme.typography.headlineSmallEmphasized)
                }
            }
            Column {
                GroupRow(0, 2, supporting = { Text(server.address) }) { Text("Address") }
                GroupRow(
                    1,
                    2,
                    supporting = { Text(fingerprintText(server.fingerprint), fontFamily = FontFamily.Monospace) },
                ) { Text("Fingerprint") }
            }
        }
    }
}

@Composable
private fun EditServer(model: AppModel, server: Server) {
    val kept = model.pictureFile(server.avatar)
    var name by rememberSaveable { mutableStateOf(server.name.orEmpty()) }
    var chosen by rememberSaveable { mutableStateOf<ByteArray?>(null) }
    var edited by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(kept) {
        if (chosen.contentEquals(kept)) edited = false
    }
    val picture = if (edited) chosen else kept
    val named = validName(name)
    val renamed = named != server.name
    val repainted = !picture.contentEquals(kept)
    Panel {
        PictureField(model, picture, name.ifBlank { server.address }) {
            chosen = it
            edited = true
        }
        NameField(name) { name = it }
    }
    MainButton("Save", (renamed || repainted) && nameError(name) == null, model.serverSaved == ServerSave.Saving) {
        model.serverSaved = ServerSave.Saving
        val name = if (renamed) NameChange.Set(named) else NameChange.Keep
        val picture = if (repainted) PictureChange.Set(picture) else PictureChange.Keep
        model.core.command(Command.SetServerProfile(name, picture))
    }
    val outcome = when (val saved = model.serverSaved) {
        null, ServerSave.Saving -> null
        ServerSave.Saved -> "Saved"
        is ServerSave.Refused -> saved.reason
    }
    outcome?.let { Text(it, color = MaterialTheme.colorScheme.onSurfaceVariant) }
}

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun Members(model: AppModel, state: CoreState) {
    var removing by rememberSaveable { mutableStateOf<Long?>(null) }
    LaunchedEffect(Unit) { model.core.command(Command.ListUsers) }
    Page(model, "Members") { padding ->
        LazyColumn(Modifier.pageWidth(), contentPadding = padding) {
            item {
                if (model.users.isEmpty()) {
                    Box(Modifier.fillMaxWidth().padding(24.dp), Alignment.Center) { LoadingIndicator() }
                } else {
                    GroupTitle(people(model.users.size))
                }
            }
            itemsIndexed(model.users, { _, user -> user.id.toLong() }) { index, user ->
                Member(model, state, user, index) { removing = user.id.toLong() }
            }
        }
    }
    model.users.find { it.id.toLong() == removing }?.let { user ->
        RemoveDialog(user.name, { removing = null }) { cascade ->
            model.core.command(Command.RemoveUser(user.id, cascade))
        }
    }
}

@Composable
private fun Member(model: AppModel, state: CoreState, user: User, index: Int, onRemove: () -> Unit) {
    val role = state.role.takeIf { user.id != state.me }
    val roles = listOf(Role.MANAGER, Role.USER).filter { role != null && canSetRole(role, user.role, it) }
    val inviter = model.users.find { it.id == user.invitedBy }
    GroupRow(
        index,
        model.users.size,
        leading = { Avatar(model.picture(state.people.find { it.user == user.id }?.avatar), user.name, 40.dp) },
        trailing = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (roles.isEmpty()) {
                    Text(user.role.label, style = MaterialTheme.typography.labelLarge)
                } else {
                    RoleMenu(user.role, roles) { model.core.command(Command.SetRole(user.id, it)) }
                }
                if (role != null && canRemove(role, user.role)) {
                    OverflowMenu { close ->
                        DangerItem("Remove from Server") {
                            close()
                            onRemove()
                        }
                    }
                }
            }
        },
        supporting = {
            Column {
                inviter?.let { Text("Invited by ${it.name}") }
                Text(fingerprintShort(user.identityKey), fontFamily = FontFamily.Monospace)
            }
        },
    ) { Text(if (user.id == state.me) "${user.name} (you)" else user.name) }
}

val Role.label
    get() = when (this) {
        Role.USER -> "User"
        Role.MANAGER -> "Manager"
        Role.ADMIN -> "Admin"
    }

@Composable
private fun RoleMenu(current: Role, roles: List<Role>, onPick: (Role) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        TextButton({ open = true }, contentPadding = ButtonDefaults.TextButtonWithIconContentPadding) {
            Text(current.label)
            Icon(Icons.Default.ArrowDropDown, "Change role", tint = MaterialTheme.colorScheme.onSurface)
        }
        DropdownMenu(open, { open = false }) {
            roles.forEach { role ->
                DropdownMenuItem({ Text(role.label) }, {
                    open = false
                    onPick(role)
                })
            }
        }
    }
}

@Composable
fun Invites(model: AppModel, state: CoreState) {
    val context = LocalContext.current
    val clipboard = LocalClipboard.current
    val scope = rememberCoroutineScope()
    val code = model.invite
    val cap = state.permissions?.inviteCap
    val create = { model.core.command(Command.CreateInvite) }
    Page(model, "Invite People") { padding ->
        Form(padding) {
            if (code == null) {
                MainButton("Create Code", onClick = create)
                return@Form
            }
            Panel {
                Text(
                    code,
                    Modifier.fillMaxWidth(),
                    fontFamily = FontFamily.Monospace,
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.headlineMedium,
                )
                Text(
                    "Single use, expires in 24 hours." + cap?.let { " You can make $it a day." }.orEmpty(),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                ButtonGroup({}, Modifier.fillMaxWidth()) {
                    customItem({
                        CodeButton(Icons.Default.ContentCopy, "Copy", filled = false) {
                            val copied = ClipEntry(ClipData.newPlainText("Invite code", code))
                            scope.launch { clipboard.setClipEntry(copied) }
                        }
                    }, {})
                    customItem({
                        CodeButton(Icons.Default.Share, "Share", filled = true) {
                            val send = Intent(Intent.ACTION_SEND).setType("text/plain")
                            send.putExtra(Intent.EXTRA_TEXT, code)
                            context.startActivity(Intent.createChooser(send, null))
                        }
                    }, {})
                }
            }
            TextButton(create, Modifier.align(Alignment.CenterHorizontally)) { Text("New Code") }
        }
    }
}

/** One of the buttons that share an invite code, filled for the main one. */
@Composable
private fun ButtonGroupScope.CodeButton(icon: ImageVector, text: String, filled: Boolean, onClick: () -> Unit) {
    val presses = remember { MutableInteractionSource() }
    val width = Modifier.weight(1f).animateWidth(presses)
    val label: @Composable RowScope.() -> Unit = {
        Icon(icon, null, Modifier.padding(end = 8.dp).size(18.dp))
        Text(text)
    }
    if (filled) {
        Button(onClick, width, interactionSource = presses, content = label)
    } else {
        FilledTonalButton(onClick, width, interactionSource = presses, content = label)
    }
}

@Composable
fun Keys(model: AppModel, state: CoreState) {
    val others = state.people.filter { it.user != state.me }
    Page(model, "Privacy and Keys") { padding ->
        LazyColumn(Modifier.pageWidth(), contentPadding = padding) {
            state.server?.let { server ->
                item {
                    GroupTitle("This Server")
                    GroupRow(
                        0,
                        1,
                        supporting = { Text(fingerprintText(server.fingerprint), fontFamily = FontFamily.Monospace) },
                    ) { Text(server.address) }
                    GroupTitle("People Here")
                }
            }
            itemsIndexed(others, { _, person -> person.user.toLong() }) { index, person ->
                GroupRow(
                    index,
                    others.size,
                    { model.sheetPerson = person.user },
                    leading = { PersonPicture(model, person, 40.dp) },
                    trailing = { if (!person.blocked) TrustLabel(state.trust(person)) },
                    supporting = person.identityKey?.let { key ->
                        { Text(fingerprintShort(key), fontFamily = FontFamily.Monospace) }
                    },
                ) { Text(person.name) }
            }
        }
    }
}
