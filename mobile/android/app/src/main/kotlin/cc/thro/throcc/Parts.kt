package cc.thro.throcc

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts.PickVisualMedia
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Block
import androidx.compose.material.icons.filled.HeadsetOff
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role as Semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import cc.thro.throcc.core.Connection
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.NameCheck
import cc.thro.throcc.core.Person
import cc.thro.throcc.core.Role
import cc.thro.throcc.core.checkName
import cc.thro.throcc.core.limits
import kotlinx.coroutines.launch

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TopBar(title: String, onBack: (() -> Unit)? = null, actions: @Composable RowScope.() -> Unit = {}) {
    TopAppBar(
        title = { Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        navigationIcon = {
            if (onBack != null) IconButton(onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
        },
        actions = actions,
    )
}

/** A scrolling column of fields, kept narrow in a wide window. */
@Composable
fun Form(padding: PaddingValues, content: @Composable ColumnScope.() -> Unit) {
    Column(
        Modifier
            .padding(padding)
            .consumeWindowInsets(padding)
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(16.dp)
            .widthIn(max = 480.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
        content = content,
    )
}

@Composable
fun Hint(text: String, modifier: Modifier = Modifier) {
    Box(modifier.fillMaxSize().padding(24.dp), Alignment.Center) {
        Text(text, color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
    }
}

@Composable
fun SectionTitle(text: String) {
    Text(
        text,
        Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        color = MaterialTheme.colorScheme.primary,
        style = MaterialTheme.typography.titleSmall,
    )
}

/** A round picture, or the first letter of `name` where there is none. */
@Composable
fun Avatar(picture: ImageBitmap?, name: String, size: Dp, modifier: Modifier = Modifier) {
    Box(
        modifier.size(size).clip(CircleShape).background(MaterialTheme.colorScheme.secondaryContainer),
        Alignment.Center,
    ) {
        if (picture != null) {
            Image(picture, null, Modifier.fillMaxSize(), contentScale = ContentScale.Crop)
        } else {
            val letter = name.take(if (name.firstOrNull()?.isHighSurrogate() == true) 2 else 1)
            val color = MaterialTheme.colorScheme.onSecondaryContainer
            Text(letter.uppercase(), color = color, fontSize = (size.value * 0.4f).sp)
        }
    }
}

/** The picture of `person`, or a block sign for someone blocked. */
@Composable
fun PersonPicture(model: AppModel, person: Person, size: Dp, modifier: Modifier = Modifier) {
    if (person.blocked) {
        Icon(Icons.Default.Block, null, modifier.size(size), MaterialTheme.colorScheme.outline)
    } else {
        Avatar(model.picture(person.avatar), person.name, size, modifier)
    }
}

@Composable
fun MutedIcon(person: Person) {
    val tint = MaterialTheme.colorScheme.onSurfaceVariant
    when {
        person.deafened -> Icon(Icons.Default.HeadsetOff, "Deafened", Modifier.size(18.dp), tint)
        !person.mic -> Icon(Icons.Default.MicOff, "Muted", Modifier.size(18.dp), tint)
    }
}

enum class Trust { Verified, Unverified, KeyChanged }

fun CoreState.trust(person: Person) = when {
    keyWarnings.any { it.user == person.user } -> Trust.KeyChanged
    person.verified || person.user == me -> Trust.Verified
    else -> Trust.Unverified
}

@Composable
fun TrustLabel(trust: Trust) {
    val style = MaterialTheme.typography.labelMedium
    when (trust) {
        Trust.KeyChanged -> Text("Key changed", color = MaterialTheme.colorScheme.error, style = style)
        Trust.Verified -> Text("Verified", color = MaterialTheme.colorScheme.primary, style = style)
        Trust.Unverified -> Text("Not verified", color = MaterialTheme.colorScheme.onSurfaceVariant, style = style)
    }
}

val Role.label
    get() = when (this) {
        Role.USER -> "User"
        Role.MANAGER -> "Manager"
        Role.ADMIN -> "Admin"
    }

/** The name a server keeps for `typed`, or null where it refuses it. */
fun validName(typed: String) = (checkName(typed) as? NameCheck.Fine)?.name

/** Why a server refuses `typed` as a name, or null where it takes it or nothing is typed. */
fun nameError(typed: String) = when (checkName(typed)) {
    NameCheck.TooLong -> "Names can be at most ${limits().nameChars} characters."
    NameCheck.HidesText -> "Names can’t contain control or invisible characters."
    NameCheck.ShowsNothing -> "Names need at least one visible character."
    NameCheck.Empty, is NameCheck.Fine -> null
}

@Composable
fun NameField(name: String, onChange: (String) -> Unit) {
    val error = nameError(name)
    OutlinedTextField(
        name,
        onChange,
        Modifier.fillMaxWidth(),
        label = { Text("Name") },
        supportingText = error?.let { { Text(it) } },
        isError = error != null,
        singleLine = true,
    )
}

/** The picture with buttons to choose another from the photo picker and to remove it. */
@Composable
fun PictureField(model: AppModel, picture: ByteArray?, name: String, onChange: (ByteArray?) -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val picker = rememberLauncherForActivityResult(PickVisualMedia()) { uri ->
        if (uri != null) {
            scope.launch {
                squarePicture(context, uri)?.let(onChange) ?: model.notices.trySend("Couldn’t read that picture.")
            }
        }
    }
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Avatar(remember(picture) { picture?.toImage() }, name, 72.dp)
        Column {
            OutlinedButton({ picker.launch(PickVisualMediaRequest(PickVisualMedia.ImageOnly)) }) {
                Text("Choose Picture…")
            }
            if (picture != null) TextButton({ onChange(null) }) { Text("Remove") }
        }
    }
}

/** A three-dot button that opens a menu of `items`. Each item calls the function it is given to close the menu. */
@Composable
fun OverflowMenu(items: @Composable ColumnScope.(close: () -> Unit) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        IconButton({ open = true }) { Icon(Icons.Default.MoreVert, "More") }
        DropdownMenu(open, { open = false }) { items { open = false } }
    }
}

@Composable
fun Confirm(title: String, text: String, action: String, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            TextButton({
                onConfirm()
                onDismiss()
            }) { Text(action) }
        },
        dismissButton = { TextButton(onDismiss) { Text("Cancel") } },
        title = { Text(title) },
        text = { Text(text) },
    )
}

/** Asks before removing `name` from the server, and whether to remove everyone they invited too. */
@Composable
fun RemoveDialog(name: String, onDismiss: () -> Unit, onRemove: (cascade: Boolean) -> Unit) {
    var cascade by remember { mutableStateOf(false) }
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            TextButton({
                onRemove(cascade)
                onDismiss()
            }) { Text("Remove") }
        },
        dismissButton = { TextButton(onDismiss) { Text("Cancel") } },
        title = { Text("Remove “$name”?") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("They are disconnected at once and need a new invite to come back.")
                Row(
                    Modifier.toggleable(cascade, role = Semantics.Checkbox) { cascade = it },
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Checkbox(cascade, null)
                    Text("Also remove everyone they invited, and everyone those people invited")
                }
            }
        },
    )
}

/** What became of the connection, with a way to get it back. */
@Composable
fun ConnectionBanner(model: AppModel, state: CoreState) {
    val signIn = { state.server?.let { model.connect(it.address) } ?: Unit }
    when (val connection = state.connection) {
        Connection.Connected, Connection.Disconnected -> {}
        is Connection.Reconnecting -> Banner(
            "Reconnecting (attempt ${connection.attempt}): ${connection.reason}. You’ll be put back in your room.",
        )
        Connection.AwaitingReconnect -> Banner(
            "The connection was down for a while. Reconnecting here signs you out on any other device.",
            "Reconnect",
            model::reconnect,
        )
        Connection.SignedInElsewhere -> Banner("Signed in on another device.", "Sign In Here", signIn)
        is Connection.Failed -> Banner("Disconnected: ${connection.reason}", "Try Again", signIn)
    }
}

@Composable
private fun Banner(text: String, action: String? = null, onAction: () -> Unit = {}) {
    Card(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        Column(Modifier.padding(16.dp), Arrangement.spacedBy(8.dp)) {
            Text(text)
            if (action != null) Button(onAction, Modifier.align(Alignment.End)) { Text(action) }
        }
    }
}
