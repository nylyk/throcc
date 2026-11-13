package cc.thro.throcc

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
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
import cc.thro.throcc.core.StartedOver
import cc.thro.throcc.core.fingerprintText
import cc.thro.throcc.core.initial
import cc.thro.throcc.core.isInviteCode

@Composable
fun Servers(model: AppModel, state: CoreState) {
    var removing by rememberSaveable { mutableStateOf<String?>(null) }
    Page(
        model,
        "Servers",
        onBack = null,
        actions = { IconButton({ model.open(Screen.Settings) }) { Icon(Icons.Default.Settings, "Settings") } },
        floatingButton = { AddServerButton { model.addServer() } },
    ) { padding ->
        if (model.servers.isEmpty()) {
            Hint(Icons.Default.Dns, "Add your first server to get started.", Modifier.padding(padding))
        }
        LazyColumn(Modifier.pageWidth(), contentPadding = padding) {
            itemsIndexed(model.servers, key = { _, server -> server.address }) { index, server ->
                ServerRow(model, server, index, state.holdsSessionWith(server.address)) { removing = server.address }
            }
            item { Spacer(Modifier.height(88.dp)) }
        }
    }
    model.servers.find { it.address == removing }?.let { server ->
        Confirm(
            "Remove “${server.title}”?",
            "It leaves your list, but you stay a member. Adding it again needs an invite. " +
                "To end your membership, connect and choose Leave Server.",
            "Remove",
            onDismiss = { removing = null },
        ) { model.forgetServer(server.address) }
    }
}

@Composable
private fun AddServerButton(onClick: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    ExtendedFloatingActionButton(
        { Text("Add Server") },
        // The button hides its text from accessibility services, which read the icon's description.
        { Icon(Icons.Default.Add, "Add Server") },
        onClick,
        containerColor = scheme.primary,
        contentColor = scheme.onPrimary,
    )
}

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
private fun ServerRow(model: AppModel, server: KnownServer, index: Int, connected: Boolean, onRemove: () -> Unit) {
    val title = server.title
    GroupRow(
        index,
        model.servers.size,
        { model.openServer(server.address) },
        leading = { Avatar(model.picture(server), initial(title), 40.dp) },
        trailing = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (model.connectingTo == server.address) LoadingIndicator(Modifier.size(32.dp))
                OverflowMenu { close ->
                    DangerItem("Remove from List") {
                        close()
                        onRemove()
                    }
                }
            }
        },
        supporting = {
            Column {
                if (server.name != null) Text(server.address)
                if (connected) Connected()
            }
        },
    ) { Text(title) }
}

@Composable
private fun Connected() {
    val color = MaterialTheme.colorScheme.primary
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(Icons.Default.Link, null, Modifier.padding(end = 4.dp).size(16.dp), color)
        Text("Connected", color = color, style = MaterialTheme.typography.labelLarge)
    }
}

@Composable
fun AddServer(model: AppModel) {
    var address by rememberSaveable { mutableStateOf(model.addingAddress) }
    var code by rememberSaveable { mutableStateOf("") }
    val wrongCode = code.isNotBlank() && !isInviteCode(code)
    Page(model, "Add Server") { padding ->
        Form(padding) {
            Panel {
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
                            if (wrongCode) {
                                "An invite code looks like K7Q-M2XPD."
                            } else {
                                "The code also proves you reached the real server. " +
                                    "Leave it empty for a server you’ve joined before."
                            },
                        )
                    },
                    isError = wrongCode,
                    keyboardOptions = KeyboardOptions(KeyboardCapitalization.Characters, autoCorrectEnabled = false),
                    singleLine = true,
                )
            }
            MainButton("Join", address.isNotBlank() && !wrongCode, model.connectingTo != null) {
                model.connect(address.trim(), code.trim().ifEmpty { null })
            }
        }
    }
}

/** Why signing in failed, with the user's ways forward. */
@Composable
fun SignInProblemDialog(model: AppModel, problem: SignInProblem) {
    when (problem) {
        is SignInProblem.KeyChanged -> KeyChangedDialog(model, problem)
        is SignInProblem.IdentityMissing -> when (val started = model.startingOver) {
            is StartingOver.Done -> StartedOverDialog(model, problem, started.started)
            else -> IdentityMissingDialog(model, problem, started)
        }
    }
}

@Composable
private fun KeyChangedDialog(model: AppModel, problem: SignInProblem.KeyChanged) {
    val close = { model.signInProblem = null }
    AlertDialog(
        onDismissRequest = close,
        confirmButton = {
            Button({
                close()
                model.addServer(problem.address)
            }) { Text("Use a Fresh Invite") }
        },
        dismissButton = { DangerButton("Trust the New Key") { model.trustNewKey(problem) } },
        title = { Text("This server’s key changed") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                listOf("You trusted" to problem.expected, "It now has" to problem.actual).forEach { (label, key) ->
                    Column {
                        Text(label, style = MaterialTheme.typography.labelLarge)
                        Text(fingerprintText(key), fontFamily = FontFamily.Monospace)
                    }
                }
                Text(
                    "This happens when the server was reinstalled, and also when someone is pretending to be it. " +
                        "The safe way back is a fresh invite code: add the server again with it, " +
                        "and the code proves the new key.",
                )
            }
        },
    )
}

@Composable
private fun IdentityMissingDialog(model: AppModel, problem: SignInProblem.IdentityMissing, started: StartingOver?) {
    val running = started == StartingOver.Running
    val close = { if (!running) model.signInProblem = null }
    AlertDialog(
        onDismissRequest = close,
        confirmButton = {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TextButton({ model.tryAgain(problem) }, enabled = !running) { Text("Try Again") }
                DangerButton(if (running) "Starting Over…" else "Start Over", !running, model::startOver)
            }
        },
        dismissButton = { TextButton(close, enabled = !running) { Text("Cancel") } },
        title = { Text("Your identity is missing") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    "Your identity is missing from this device’s keychain, so your servers won’t know it’s you. " +
                        "If it still isn’t found, restart throcc.",
                )
                Text(
                    "Starting over makes a new identity and takes every server off your list. " +
                        "Joining your servers again needs new invites, and anyone who verified you needs to verify you again.",
                )
                (started as? StartingOver.Failed)?.let { ErrorText(it.reason) }
            }
        },
    )
}

@Composable
private fun StartedOverDialog(model: AppModel, problem: SignInProblem.IdentityMissing, started: StartedOver) {
    val close = { model.signInProblem = null }
    val recovered = started == StartedOver.RECOVERED
    AlertDialog(
        onDismissRequest = close,
        confirmButton = {
            if (recovered) Button({ model.tryAgain(problem) }) { Text("Join") } else TextButton(close) { Text("Close") }
        },
        dismissButton = { if (recovered) TextButton(close) { Text("Close") } },
        title = { Text(if (recovered) "Your identity is back" else "You have a new identity") },
        text = {
            Text(
                if (recovered) {
                    "Your keychain had it after all. Your servers are still on your list."
                } else {
                    "Your servers have left your list. Add each one again with a new invite."
                },
            )
        },
    )
}
