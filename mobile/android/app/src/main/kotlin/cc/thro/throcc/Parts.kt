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
import androidx.compose.foundation.layout.calculateEndPadding
import androidx.compose.foundation.layout.calculateStartPadding
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Block
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material.icons.filled.HeadsetOff
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.VerifiedUser
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material.icons.outlined.GppMaybe
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.FabPosition
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LargeFlexibleTopAppBar
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialShapes
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.MenuDefaults
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedListItem
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.adaptive.currentWindowAdaptiveInfoV2
import androidx.compose.material3.toShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.window.core.layout.WindowSizeClass
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.KnownServer
import cc.thro.throcc.core.CoreException
import cc.thro.throcc.core.KeyTrust
import cc.thro.throcc.core.NameCheck
import cc.thro.throcc.core.NoticeAction
import cc.thro.throcc.core.Person
import cc.thro.throcc.core.Server
import cc.thro.throcc.core.checkName
import cc.thro.throcc.core.initial
import cc.thro.throcc.core.nameProblem
import kotlinx.coroutines.launch

/** The width and side margins of a page's content, kept narrow in the middle of a wide window. */
fun Modifier.pageWidth() = fillMaxWidth().wrapContentWidth().widthIn(max = 640.dp).padding(horizontal = 16.dp)

/** A page under a large title that shrinks as the content scrolls. A short window gets the small title. */
@Composable
fun Page(
    model: AppModel,
    title: String,
    onBack: (() -> Unit)? = model::back,
    subtitle: String? = null,
    largeTitle: Boolean = true,
    notices: Boolean = true,
    actions: @Composable RowScope.() -> Unit = {},
    floatingButton: @Composable () -> Unit = {},
    buttonPosition: FabPosition = FabPosition.End,
    content: @Composable (PaddingValues) -> Unit,
) {
    val tall = largeTitle && currentWindowAdaptiveInfoV2().windowSizeClass
        .isHeightAtLeastBreakpoint(WindowSizeClass.HEIGHT_DP_MEDIUM_LOWER_BOUND)
    val scroll = if (tall) {
        TopAppBarDefaults.exitUntilCollapsedScrollBehavior()
    } else {
        TopAppBarDefaults.pinnedScrollBehavior()
    }
    val name = @Composable {
        Text(title, fontWeight = FontWeight.Medium, maxLines = 2, overflow = TextOverflow.Ellipsis)
    }
    val back = @Composable {
        if (onBack != null) IconButton(onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
    }
    val colors = TopAppBarDefaults.topAppBarColors(actionIconContentColor = MaterialTheme.colorScheme.onSurface)
    Scaffold(
        Modifier.nestedScroll(scroll.nestedScrollConnection),
        topBar = {
            if (tall) {
                LargeFlexibleTopAppBar(
                    name,
                    subtitle = subtitle?.let { { Text(it, maxLines = 1, overflow = TextOverflow.Ellipsis) } },
                    navigationIcon = back,
                    actions = actions,
                    colors = colors,
                    scrollBehavior = scroll,
                )
            } else {
                TopAppBar(name, navigationIcon = back, actions = actions, colors = colors, scrollBehavior = scroll)
            }
        },
        snackbarHost = { if (notices && model.sheetPerson == null) SnackbarHost(model.snackbars) },
        floatingActionButton = floatingButton,
        floatingActionButtonPosition = buttonPosition,
        contentWindowInsets = WindowInsets.safeDrawing,
        content = content,
    )
}

/** These paddings above and beside only, for scrolling content that takes the bottom one inside. */
@Composable
fun PaddingValues.exceptBottom(): PaddingValues {
    val direction = LocalLayoutDirection.current
    return PaddingValues(calculateStartPadding(direction), calculateTopPadding(), calculateEndPadding(direction), 0.dp)
}

/** A scrolling column of fields. */
@Composable
fun Form(padding: PaddingValues, content: @Composable ColumnScope.() -> Unit) {
    Column(
        Modifier.padding(padding.exceptBottom())
            .verticalScroll(rememberScrollState())
            .padding(bottom = padding.calculateBottomPadding())
            .pageWidth()
            .padding(vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
        content = content,
    )
}

@Composable
fun panelColors(selected: Boolean = false) = CardDefaults.cardColors(
    if (selected) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surfaceContainer,
)

/** A rounded container on the panel colour around a group of fields. */
@Composable
fun Panel(content: @Composable ColumnScope.() -> Unit) {
    Card(Modifier.fillMaxWidth(), MaterialTheme.shapes.extraLarge, panelColors()) {
        Column(Modifier.padding(20.dp), Arrangement.spacedBy(16.dp), content = content)
    }
}

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun Hint(icon: ImageVector, text: String, modifier: Modifier = Modifier) {
    val scheme = MaterialTheme.colorScheme
    Column(
        modifier.fillMaxSize().padding(24.dp),
        Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
        Alignment.CenterHorizontally,
    ) {
        Box(
            Modifier.size(96.dp).background(scheme.surfaceContainerHigh, MaterialShapes.Cookie9Sided.toShape()),
            Alignment.Center,
        ) { Icon(icon, null, Modifier.size(40.dp), scheme.onSurfaceVariant) }
        Text(text, color = scheme.onSurfaceVariant, textAlign = TextAlign.Center)
    }
}

@Composable
fun GroupTitle(text: String) {
    Text(
        text,
        Modifier.padding(start = 16.dp, top = 16.dp, bottom = 8.dp),
        MaterialTheme.colorScheme.onSurfaceVariant,
        style = MaterialTheme.typography.titleSmallEmphasized,
    )
}

/** Row `index` of `count` in a grouped list, rounded where the group begins and ends. */
@Composable
fun GroupRow(
    index: Int,
    count: Int,
    onClick: (() -> Unit)? = null,
    leading: @Composable (() -> Unit)? = null,
    trailing: @Composable (() -> Unit)? = null,
    supporting: @Composable (() -> Unit)? = null,
    title: @Composable () -> Unit,
) {
    val scheme = MaterialTheme.colorScheme
    val shapes = ListItemDefaults.segmentedShapes(index, count)
    val colors = ListItemDefaults.segmentedColors(scheme.surfaceContainer, trailingContentColor = scheme.onSurface)
    val gap = Modifier.padding(bottom = ListItemDefaults.SegmentedGap)
    if (onClick == null) {
        SegmentedListItem(
            shapes,
            gap,
            leadingContent = leading,
            trailingContent = trailing,
            supportingContent = supporting,
            colors = colors,
            content = title,
        )
    } else {
        SegmentedListItem(
            onClick,
            shapes,
            gap,
            leadingContent = leading,
            trailingContent = trailing,
            supportingContent = supporting,
            colors = colors,
            content = title,
        )
    }
}

/** The one main action of a page: a tall filled button that shows progress while `busy`. */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun MainButton(text: String, enabled: Boolean = true, busy: Boolean = false, onClick: () -> Unit) {
    val height = ButtonDefaults.MediumContainerHeight
    Button(
        onClick,
        ButtonDefaults.shapesFor(height),
        Modifier.fillMaxWidth().heightIn(height),
        enabled && !busy,
        contentPadding = ButtonDefaults.contentPaddingFor(height),
    ) {
        if (busy) {
            LoadingIndicator(
                Modifier.padding(end = ButtonDefaults.iconSpacingFor(height)).size(ButtonDefaults.iconSizeFor(height)),
                LocalContentColor.current,
            )
        }
        Text(text, style = ButtonDefaults.textStyleFor(height))
    }
}

/** A round picture, or the letter it stands in with where there is none, or a person sign for no name yet. */
@Composable
fun Avatar(picture: ImageBitmap?, initial: String?, size: Dp, modifier: Modifier = Modifier) {
    Box(
        modifier.size(size).clip(CircleShape).background(MaterialTheme.colorScheme.secondaryContainer),
        Alignment.Center,
    ) {
        val type = MaterialTheme.typography
        when {
            picture != null -> Image(picture, null, Modifier.fillMaxSize(), contentScale = ContentScale.Crop)
            initial == null -> Icon(Icons.Default.Person, null, Modifier.size(size / 2))
            else -> Text(
                initial,
                color = MaterialTheme.colorScheme.onSecondaryContainer,
                style = if (size < 56.dp) type.titleMedium else type.headlineMedium,
            )
        }
    }
}

/** The picture of `person`, or a block sign for someone blocked. */
@Composable
fun PersonPicture(model: AppModel, person: Person, size: Dp, modifier: Modifier = Modifier) {
    if (person.blocked) {
        Icon(Icons.Default.Block, null, modifier.size(size), MaterialTheme.colorScheme.onSurfaceVariant)
    } else {
        Avatar(model.picture(person.avatar), person.initial, size, modifier)
    }
}

@Composable
fun MutedIcon(person: Person) {
    val tint = MaterialTheme.colorScheme.onSurfaceVariant
    when {
        person.deafened -> Icon(Icons.Default.HeadsetOff, "Deafened", Modifier.size(20.dp), tint)
        !person.mic -> Icon(Icons.Default.MicOff, "Muted", Modifier.size(20.dp), tint)
    }
}

@Composable
fun TrustLabel(trust: KeyTrust) {
    val scheme = MaterialTheme.colorScheme
    val (icon, text, color) = when (trust) {
        KeyTrust.KEY_CHANGED -> Triple(Icons.Default.Warning, "Key changed", scheme.error)
        KeyTrust.VERIFIED -> Triple(Icons.Default.VerifiedUser, "Verified", scheme.primary)
        KeyTrust.UNVERIFIED -> Triple(Icons.Outlined.GppMaybe, "Not verified", scheme.onSurfaceVariant)
    }
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Icon(icon, null, Modifier.size(16.dp), color)
        Text(text, color = color, style = MaterialTheme.typography.labelLarge)
    }
}

fun people(count: Int) = if (count == 1) "1 person" else "$count people"

val Server.title get() = name ?: address

val KnownServer.title get() = name ?: address

/** The name a server keeps for `typed`, or null where it refuses it. */
fun validName(typed: String) = (checkName(typed) as? NameCheck.Fine)?.name

@Composable
fun NameField(name: String, onChange: (String) -> Unit) {
    val error = nameProblem(name)
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
                try {
                    profilePicture(context, uri)?.let(onChange)
                        ?: model.notices.trySend("Couldn’t read that picture. Choose another one.")
                } catch (failure: CoreException) {
                    model.notices.trySend(failure.explained)
                }
            }
        }
    }
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Avatar(remember(picture) { picture?.toImage() }, initial(name).takeIf { name.isNotBlank() }, 80.dp)
        Column {
            FilledTonalButton({ picker.launch(PickVisualMediaRequest(PickVisualMedia.ImageOnly)) }) {
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

/** A menu item for something that can't be undone. */
@Composable
fun DangerItem(text: String, onClick: () -> Unit) {
    val colors = MenuDefaults.itemColors(textColor = MaterialTheme.colorScheme.error)
    DropdownMenuItem({ Text(text) }, onClick, colors = colors)
}

@Composable
fun DangerButton(text: String, enabled: Boolean = true, onClick: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    Button(onClick, enabled = enabled, colors = ButtonDefaults.buttonColors(scheme.error, scheme.onError)) { Text(text) }
}

/** Asks before `action`, which can't be undone. */
@Composable
fun Confirm(title: String, text: String?, action: String, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            DangerButton(action) {
                onConfirm()
                onDismiss()
            }
        },
        dismissButton = { TextButton(onDismiss) { Text("Cancel") } },
        title = { Text(title) },
        text = text?.let { { Text(it) } },
    )
}

/** Asks before removing `name` from the server, and whether to remove everyone they invited too. */
@Composable
fun RemoveDialog(name: String, onDismiss: () -> Unit, onRemove: (cascade: Boolean) -> Unit) {
    var cascade by rememberSaveable { mutableStateOf(false) }
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            DangerButton("Remove") {
                onRemove(cascade)
                onDismiss()
            }
        },
        dismissButton = { TextButton(onDismiss) { Text("Cancel") } },
        title = { Text("Remove “$name”?") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("They are disconnected at once and need a new invite to come back.")
                Row(
                    Modifier.heightIn(min = 48.dp).toggleable(cascade, role = Role.Checkbox) { cascade = it },
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
    val notice = state.notice ?: return
    val signIn = { state.server?.let { model.connect(it.address) } ?: Unit }
    val (action, onAction) = when (notice.action) {
        NoticeAction.RECONNECT -> "Reconnect" to model::reconnect
        NoticeAction.SIGN_IN_HERE -> "Sign In Here" to signIn
        NoticeAction.TRY_AGAIN -> "Try Again" to signIn
        null -> null to {}
    }
    Banner(notice.hint, notice.problem, notice.busy || model.connectingTo != null, action, onAction)
}

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
private fun Banner(text: String, problem: String?, busy: Boolean, action: String?, onAction: () -> Unit) {
    val container = CardDefaults.cardColors(MaterialTheme.colorScheme.surfaceContainerHigh)
    Card(Modifier.fillMaxWidth().padding(bottom = 12.dp), MaterialTheme.shapes.extraLarge, container) {
        Column(Modifier.fillMaxWidth().padding(16.dp), Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                if (busy) LoadingIndicator(Modifier.size(24.dp)) else Icon(Icons.Default.CloudOff, null)
                Text(text)
            }
            problem?.let { ErrorText(it) }
            if (action != null) Button(onAction, Modifier.align(Alignment.End), !busy) { Text(action) }
        }
    }
}

/** Small text in the error colour, for what went wrong or why something can't be read. */
@Composable
fun ErrorText(text: String, modifier: Modifier = Modifier) {
    Text(text, modifier, MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
}
