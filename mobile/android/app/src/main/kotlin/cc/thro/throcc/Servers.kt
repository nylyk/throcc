package cc.thro.throcc

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.KnownServer
import cc.thro.throcc.core.isInviteCode

@Composable
fun Servers(model: AppModel, state: CoreState) {
    var removing by remember { mutableStateOf<KnownServer?>(null) }
    Scaffold(
        topBar = {
            TopBar("Servers") {
                IconButton({ model.open(Screen.Settings) }) { Icon(Icons.Default.Settings, "Settings") }
            }
        },
        floatingActionButton = {
            FloatingActionButton({ model.open(Screen.AddServer) }) { Icon(Icons.Default.Add, "Add server") }
        },
    ) { padding ->
        if (model.servers.isEmpty()) Hint("Add your first server to get started.", Modifier.padding(padding))
        LazyColumn(contentPadding = padding) {
            items(model.servers, key = { it.address }) { server ->
                ServerRow(model, server, state.server?.address == server.address) { removing = server }
            }
        }
    }
    removing?.let { server ->
        Confirm(
            "Remove “${server.name ?: server.address}”?",
            "It leaves your list, but you stay a member. Adding it again needs an invite. " +
                "To end your membership, connect and choose Leave Server.",
            "Remove",
            onDismiss = { removing = null },
        ) { model.forgetServer(server.address) }
    }
}

@Composable
private fun ServerRow(model: AppModel, server: KnownServer, connected: Boolean, onRemove: () -> Unit) {
    val title = server.name ?: server.address
    ListItem(
        headlineContent = { Text(title) },
        modifier = Modifier.clickable(enabled = model.connecting == null) { model.openServer(server.address) },
        supportingContent = server.name?.let { { Text(server.address) } },
        leadingContent = { Avatar(remember(server) { server.picture?.toImage() }, title, 40.dp) },
        trailingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (connected) {
                    val style = MaterialTheme.typography.labelMedium
                    Text("Connected", color = MaterialTheme.colorScheme.primary, style = style)
                }
                if (model.connecting == server.address) {
                    CircularProgressIndicator(Modifier.padding(12.dp).size(24.dp))
                } else {
                    IconButton(onRemove) { Icon(Icons.Default.Close, "Remove from List") }
                }
            }
        },
    )
}

@Composable
fun AddServer(model: AppModel) {
    var address by rememberSaveable { mutableStateOf("") }
    var code by rememberSaveable { mutableStateOf("") }
    val joining = model.connecting != null
    val wrongCode = code.isNotBlank() && !isInviteCode(code)
    Scaffold(topBar = { TopBar("Add Server", model::back) }) { padding ->
        Form(padding) {
            OutlinedTextField(
                address,
                { address = it },
                Modifier.fillMaxWidth(),
                label = { Text("Address") },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Next),
                singleLine = true,
            )
            OutlinedTextField(
                code,
                { code = it },
                Modifier.fillMaxWidth(),
                textStyle = LocalTextStyle.current.copy(fontFamily = FontFamily.Monospace),
                label = { Text("Invite code") },
                supportingText = {
                    Text(
                        "The code also proves you reached the real server. " +
                            "Leave it empty for a server you’ve joined before.",
                    )
                },
                isError = wrongCode,
                keyboardOptions = KeyboardOptions(KeyboardCapitalization.Characters, autoCorrectEnabled = false),
                singleLine = true,
            )
            Button(
                { model.connect(address.trim(), code.trim().ifEmpty { null }) },
                enabled = !joining && address.isNotBlank() && !wrongCode,
            ) { Text(if (joining) "Joining…" else "Join") }
        }
    }
}
