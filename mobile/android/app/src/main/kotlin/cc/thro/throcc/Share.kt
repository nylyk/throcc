package cc.thro.throcc

import android.app.Activity
import android.app.PictureInPictureParams
import android.content.pm.ActivityInfo
import android.util.Rational
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.PictureInPictureAlt
import androidx.compose.material.icons.filled.VisibilityOff
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.PlainTooltip
import androidx.compose.material3.Text
import androidx.compose.material3.TooltipAnchorPosition
import androidx.compose.material3.TooltipBox
import androidx.compose.material3.TooltipDefaults
import androidx.compose.material3.rememberTooltipState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.Person
import cc.thro.throcc.core.Share

/** The native side of a share's surface: the core decodes into the window behind it. */
object ShareView {
    init {
        System.loadLibrary("throcc_mobile")
    }

    /** The `ANativeWindow` behind `surface`, with a reference for `Core.showShare`. */
    external fun nativeWindow(surface: Surface): Long
}

/** What the pill over a watched share warns of: its label, and what it means. */
private enum class ShareWarning(val label: String, val hint: String) {
    SlowConnection("Slow connection", "Frames are dropping. Ask the sharer for a lower bitrate."),
    CantKeepUp(
        "Can’t keep up",
        "This device decodes the share too slowly. Ask the sharer for a lower resolution or frame rate.",
    ),
}

/** The warning over `user`'s share, if any, the connection's first. */
private fun shareWarning(state: CoreState, user: ULong) = when (user) {
    in state.health.slowShares -> ShareWarning.SlowConnection
    in state.health.laggingShares -> ShareWarning.CantKeepUp
    else -> null
}

/**
 * `user`'s share, decoded straight into a surface. The core keeps the surfaces a share is shown in.
 * `onTop` puts it above the other share surfaces in the window.
 */
@Composable
private fun ShareSurface(model: AppModel, user: ULong, modifier: Modifier = Modifier, onTop: Boolean = false) {
    AndroidView(
        factory = { context ->
            SurfaceView(context).apply {
                setZOrderMediaOverlay(onTop)
                holder.addCallback(object : SurfaceHolder.Callback {
                    /** The number the core gave this surface while it shows the share. */
                    var shown: ULong? = null

                    override fun surfaceCreated(holder: SurfaceHolder) {
                        shown = model.core.showShare(user, ShareView.nativeWindow(holder.surface).toULong())
                    }

                    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

                    override fun surfaceDestroyed(holder: SurfaceHolder) {
                        shown?.let { model.core.hideShare(user, it) }
                        shown = null
                    }
                })
            }
        },
        modifier = modifier,
    )
}

/** A warning over a share. Long-press tells what it means. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun WarningPill(warning: ShareWarning) {
    TooltipBox(
        TooltipDefaults.rememberTooltipPositionProvider(TooltipAnchorPosition.Below),
        { PlainTooltip { Text(warning.hint) } },
        rememberTooltipState(),
    ) {
        Row(
            Modifier
                .background(MaterialTheme.colorScheme.surfaceContainer.copy(alpha = 0.8f), CircleShape)
                .padding(horizontal = 12.dp, vertical = 8.dp),
            Arrangement.spacedBy(6.dp),
            Alignment.CenterVertically,
        ) {
            Icon(Icons.Default.Warning, null, Modifier.size(18.dp), LocalWarning.current)
            Text(
                warning.label,
                Modifier.semantics { contentDescription = "${warning.label}. ${warning.hint}" },
                style = MaterialTheme.typography.labelLarge,
            )
        }
    }
}

/** A spinner until the share has a picture to show. */
@Composable
private fun Loading(state: CoreState, user: ULong) {
    if (state.current?.showing?.contains(user) != true) {
        CircularProgressIndicator(color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** Whose share this is, over the picture. */
@Composable
private fun Sharer(model: AppModel, person: Person, details: String, modifier: Modifier = Modifier) {
    Row(
        modifier
            .background(MaterialTheme.colorScheme.surfaceContainer.copy(alpha = 0.8f), CircleShape)
            .padding(start = 4.dp, top = 4.dp, end = 12.dp, bottom = 4.dp),
        Arrangement.spacedBy(8.dp),
        Alignment.CenterVertically,
    ) {
        PersonPicture(model, person, 24.dp)
        Text(
            person.name,
            Modifier.weight(1f, fill = false),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.labelLarge,
        )
        Text(
            details,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            style = MaterialTheme.typography.labelMedium,
        )
    }
}

/** Shows `user`'s share fullscreen in a small window over other apps. */
private fun enterPictureInPicture(activity: Activity, model: AppModel, user: ULong, share: Share) {
    // Android takes no window wider than 2.39:1 or narrower than its inverse.
    val aspect = share.aspect.coerceIn(0.42f, 2.39f)
    model.fullscreenBeforePictureInPicture = model.fullscreenShare
    model.fullscreenShare = user
    val params = PictureInPictureParams.Builder().setAspectRatio(Rational((aspect * 1000).toInt(), 1000)).build()
    activity.enterPictureInPictureMode(params)
}

/** The share being watched in the room, at most [MAX_TILE_HEIGHT] of the window's height, with a way to fullscreen it. */
@Composable
fun ShareTile(model: AppModel, state: CoreState, person: Person, share: Share) {
    val warning = shareWarning(state, person.user)
    val window = LocalWindowInfo.current.containerSize
    val maxHeight = with(LocalDensity.current) { window.height.toDp() } * MAX_TILE_HEIGHT
    Box(Modifier.fillMaxWidth(), Alignment.Center) {
        Card(
            Modifier.heightIn(max = maxHeight).aspectRatio(share.aspect.coerceAtLeast(0.5f)),
            shape = MaterialTheme.shapes.extraLarge,
            colors = panelColors(),
        ) {
            Box(Modifier.fillMaxSize().background(Color.Black)) {
                Box(Modifier.fillMaxSize(), Alignment.Center) {
                    // Kept under fullscreen, so the codec never goes without a surface.
                    ShareSurface(model, person.user, Modifier.aspectRatio(share.aspect))
                    Loading(state, person.user)
                }
                Row(
                    Modifier.fillMaxWidth().align(Alignment.TopStart).padding(12.dp),
                    Arrangement.spacedBy(8.dp),
                    Alignment.CenterVertically,
                ) {
                    Sharer(model, person, share.label, Modifier.weight(1f, fill = false))
                    Spacer(Modifier.weight(1f))
                    warning?.let { WarningPill(it) }
                }
                val activity = LocalView.current.context as? Activity
                Row(
                    Modifier
                        .align(Alignment.BottomEnd)
                        .padding(12.dp)
                        .background(MaterialTheme.colorScheme.surfaceContainer.copy(alpha = 0.8f), CircleShape),
                ) {
                    IconButton({ model.core.command(Command.Unwatch(person.user)) }) {
                        Icon(Icons.Default.VisibilityOff, "Stop Watching")
                    }
                    if (activity != null) {
                        IconButton({ enterPictureInPicture(activity, model, person.user, share) }) {
                            Icon(Icons.Default.PictureInPictureAlt, "Picture in Picture")
                        }
                    }
                    IconButton({ model.fullscreenShare = person.user }) {
                        Icon(Icons.Default.Fullscreen, "Fullscreen")
                    }
                }
            }
        }
    }
}

/** The share tile's height at most, as a share of the window's: a tall share stays on screen. */
private const val MAX_TILE_HEIGHT = 0.6f

/**
 * The share `model.fullscreenShare` over the whole window, in its shape, turning with the phone. A tap
 * shows the controls, with everyone in the room and the call's controls along the bottom.
 */
@Composable
fun ShareFullscreen(model: AppModel, state: CoreState) {
    val user = model.fullscreenShare ?: return
    val room = state.current?.room
    val person = state.people.find { it.user == user }
    val share = person?.sharing
    val watching = state.current?.watching?.contains(user) == true
    LaunchedEffect(share == null || !watching) {
        if (share == null || !watching) model.fullscreenShare = null
    }
    if (share == null) return
    if (model.pictureInPicture) {
        Box(Modifier.fillMaxSize().background(Color.Black), Alignment.Center) {
            ShareSurface(model, user, Modifier.aspectRatio(share.aspect), onTop = true)
        }
        return
    }
    BackHandler { model.fullscreenShare = null }
    Immersive()
    Turning()
    KeepScreenOn()
    var controls by remember { mutableStateOf(true) }
    Box(
        Modifier
            .fillMaxSize()
            .background(Color.Black)
            .clickable(remember { MutableInteractionSource() }, null) { controls = !controls },
    ) {
        Box(Modifier.fillMaxSize(), Alignment.Center) {
            ShareSurface(model, user, Modifier.aspectRatio(share.aspect), onTop = true)
            Loading(state, user)
        }
        AnimatedVisibility(controls, Modifier.align(Alignment.TopStart), enter = fadeIn(), exit = fadeOut()) {
            Row(
                Modifier.safeDrawingPadding().padding(8.dp),
                Arrangement.spacedBy(8.dp),
                Alignment.CenterVertically,
            ) {
                FilledTonalIconButton({ model.fullscreenShare = null }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back")
                }
                Text(person.name, color = Color.White, style = MaterialTheme.typography.titleMedium)
            }
        }
        shareWarning(state, user)?.let { warning ->
            // Shown with or without the controls, as the desktop does.
            Box(Modifier.align(Alignment.TopEnd).safeDrawingPadding().padding(12.dp)) { WarningPill(warning) }
        }
        AnimatedVisibility(controls, Modifier.align(Alignment.BottomCenter), enter = fadeIn(), exit = fadeOut()) {
            Column(
                Modifier.safeDrawingPadding().padding(8.dp),
                Arrangement.spacedBy(12.dp),
                Alignment.CenterHorizontally,
            ) {
                RoomStrip(model, state.people.filter { it.room == room })
                RoomControls(model, state)
            }
        }
    }
}

/** Everyone in the room, small. */
@Composable
private fun RoomStrip(model: AppModel, people: List<Person>) {
    val scheme = MaterialTheme.colorScheme
    // Wraps, so a full room of 30 stays on screen.
    FlowRow(
        Modifier
            .background(scheme.surfaceContainer.copy(alpha = 0.8f), MaterialTheme.shapes.extraLarge)
            .padding(horizontal = 8.dp, vertical = 4.dp),
        Arrangement.spacedBy(4.dp, Alignment.CenterHorizontally),
        Arrangement.spacedBy(4.dp),
    ) {
        for (person in people) {
            val ring = if (person.speaking) scheme.tertiary else Color.Transparent
            PersonPicture(model, person, 28.dp, Modifier.border(2.dp, ring, CircleShape).padding(3.dp))
        }
    }
}

/** Lets the window turn with the device while shown, as the user's rotation setting allows. */
@Composable
private fun Turning() {
    val activity = LocalView.current.context as? Activity ?: return
    DisposableEffect(activity) {
        activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_FULL_USER
        onDispose { activity.uprightOnPhones() }
    }
}

/** Keeps the screen from turning off while shown. */
@Composable
private fun KeepScreenOn() {
    val view = LocalView.current
    DisposableEffect(view) {
        view.keepScreenOn = true
        onDispose { view.keepScreenOn = false }
    }
}

/** Hides the system bars while shown; a swipe from the edge brings them back for a moment. */
@Composable
private fun Immersive() {
    val view = LocalView.current
    DisposableEffect(view) {
        val window = (view.context as? android.app.Activity)?.window ?: return@DisposableEffect onDispose {}
        val controller = WindowCompat.getInsetsController(window, view)
        controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        controller.hide(WindowInsetsCompat.Type.systemBars())
        onDispose { controller.show(WindowInsetsCompat.Type.systemBars()) }
    }
}
