package cc.thro.throcc

import android.content.ClipData
import android.content.Intent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
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
import cc.thro.throcc.core.permissionsOf
import kotlinx.coroutines.launch

@Composable
fun ServerOverview(model: AppModel, state: CoreState) {
    val server = state.server
    Scaffold(topBar = { TopBar("Server", model::back) }) { padding ->
        if (server == null) return@Scaffold
        Form(padding) {
            if (state.role?.let(::permissionsOf)?.editServer == true) {
                EditServer(model, server)
            } else {
                Avatar(model.picture(server.avatar), server.name ?: server.address, 72.dp)
                server.name?.let { Field("Name", it) }
            }
            Field("Address", server.address)
            Field("Fingerprint", fingerprintText(server.fingerprint), FontFamily.Monospace)
        }
    }
}

@Composable
private fun Field(label: String, value: String, font: FontFamily? = null) {
    Column {
        Text(label, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.labelMedium)
        Text(value, fontFamily = font)
    }
}

@Composable
private fun EditServer(model: AppModel, server: Server) {
    val kept = remember(server.avatar?.toHexString()) { server.avatar?.let(model.core::avatar) }
    var name by rememberSaveable { mutableStateOf(server.name.orEmpty()) }
    var picture by rememberSaveable { mutableStateOf(kept) }
    val named = validName(name)
    val renamed = named != server.name
    val repainted = !picture.contentEquals(kept)
    LaunchedEffect(Unit) { model.serverSaved = null }
    PictureField(model, picture, name.ifBlank { server.address }) { picture = it }
    NameField(name) { name = it }
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Button(
            {
                model.serverSaved = SAVING
                val name = if (renamed) NameChange.Set(named) else NameChange.Keep
                val picture = if (repainted) PictureChange.Set(picture) else PictureChange.Keep
                model.core.command(Command.SetServerProfile(name, picture))
            },
            enabled = (renamed || repainted) && nameError(name) == null && model.serverSaved != SAVING,
        ) { Text("Save") }
        model.serverSaved?.let { Text(it, color = MaterialTheme.colorScheme.onSurfaceVariant) }
    }
}

@Composable
fun Members(model: AppModel, state: CoreState) {
    var removing by remember { mutableStateOf<User?>(null) }
    LaunchedEffect(Unit) { model.core.command(Command.ListUsers) }
    Scaffold(topBar = { TopBar("Members", model::back) }) { padding ->
        LazyColumn(contentPadding = padding) {
            item { SectionTitle("${model.users.size} People") }
            items(model.users, { it.id.toLong() }) { user -> Member(model, state, user) { removing = user } }
        }
    }
    removing?.let { user ->
        RemoveDialog(user.name, { removing = null }) { cascade ->
            model.core.command(Command.RemoveUser(user.id, cascade))
        }
    }
}

@Composable
private fun Member(model: AppModel, state: CoreState, user: User, onRemove: () -> Unit) {
    val role = state.role.takeIf { user.id != state.me }
    val roles = listOf(Role.MANAGER, Role.USER).filter { role != null && canSetRole(role, user.role, it) }
    val inviter = model.users.find { it.id == user.invitedBy }
    ListItem(
        { Text(if (user.id == state.me) "${user.name} (you)" else user.name) },
        supportingContent = {
            Column {
                inviter?.let { Text("Invited by ${it.name}") }
                Text(fingerprintShort(user.identityKey), fontFamily = FontFamily.Monospace)
            }
        },
        leadingContent = {
            Avatar(model.picture(state.people.find { it.user == user.id }?.avatar), user.name, 40.dp)
        },
        trailingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (roles.isEmpty()) {
                    Text(user.role.label)
                } else {
                    RoleMenu(user.role, roles) { model.core.command(Command.SetRole(user.id, it)) }
                }
                if (role != null && canRemove(role, user.role)) {
                    IconButton(onRemove) { Icon(Icons.Default.Close, "Remove") }
                }
            }
        },
    )
}

@Composable
private fun RoleMenu(current: Role, roles: List<Role>, onPick: (Role) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        TextButton({ open = true }) { Text(current.label) }
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
    val cap = state.role?.let(::permissionsOf)?.inviteCap
    Scaffold(topBar = { TopBar("Invite People", model::back) }) { padding ->
        Form(padding) {
            if (code != null) {
                Text(code, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.headlineMedium)
                Text(
                    "Single use, expires in 24 hours." + cap?.let { " You can make $it a day." }.orEmpty(),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton({
                        scope.launch { clipboard.setClipEntry(ClipEntry(ClipData.newPlainText("Invite code", code))) }
                    }) { Text("Copy") }
                    OutlinedButton({
                        val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, code)
                        context.startActivity(Intent.createChooser(send, null))
                    }) { Text("Share") }
                }
            }
            Button({ model.core.command(Command.CreateInvite) }) {
                Text(if (code == null) "Create Code" else "New Code")
            }
        }
    }
}

@Composable
fun Keys(model: AppModel, state: CoreState) {
    Scaffold(topBar = { TopBar("Privacy and Keys", model::back) }) { padding ->
        LazyColumn(contentPadding = padding) {
            state.server?.let { server ->
                item {
                    SectionTitle("This Server")
                    ListItem(
                        { Text(server.address) },
                        supportingContent = {
                            Text(fingerprintText(server.fingerprint), fontFamily = FontFamily.Monospace)
                        },
                    )
                    SectionTitle("People Here")
                }
            }
            items(state.people.filter { it.user != state.me }, { it.user.toLong() }) { person ->
                ListItem(
                    { Text(person.name) },
                    Modifier.clickable { model.sheet = person.user },
                    supportingContent = person.identityKey?.let { key ->
                        { Text(fingerprintShort(key), fontFamily = FontFamily.Monospace) }
                    },
                    leadingContent = { PersonPicture(model, person, 40.dp) },
                    trailingContent = { if (!person.blocked) TrustLabel(state.trust(person)) },
                )
            }
        }
    }
}
