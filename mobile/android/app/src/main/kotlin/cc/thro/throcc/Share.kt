package cc.thro.throcc

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
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.ScreenShare
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material3.Card
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
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
import androidx.compose.ui.platform.LocalView
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

    /** The `ANativeWindow` behind `surface`, with a reference for `Core.setShareView`. */
    external fun nativeWindow(surface: Surface): Long
}

/** A share's shape, width over height. */
private val Share.aspect get() = width.toFloat() / height.toFloat().coerceAtLeast(1f)

/** `user`'s share, decoded straight into a surface. */
@Composable
private fun ShareSurface(model: AppModel, user: ULong, modifier: Modifier = Modifier) {
    AndroidView(
        factory = { context ->
            SurfaceView(context).apply {
                holder.addCallback(object : SurfaceHolder.Callback {
                    override fun surfaceCreated(holder: SurfaceHolder) = model.showShare(user, holder.surface)
                    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}
                    override fun surfaceDestroyed(holder: SurfaceHolder) = model.hideShare(user, holder.surface)
                })
            }
        },
        modifier = modifier,
    )
}

/** A share in the room: watched in place, with a way to fullscreen it, or offered to watch. */
@Composable
fun ShareTile(model: AppModel, state: CoreState, person: Person, share: Share) {
    val watching = state.current?.watching?.contains(person.user) == true
    Card(
        Modifier.fillMaxWidth().aspectRatio(share.aspect.coerceIn(0.5f, 2.4f)),
        shape = MaterialTheme.shapes.extraLarge,
        colors = panelColors(),
    ) {
        Box(Modifier.fillMaxSize()) {
            if (watching) {
                Box(Modifier.fillMaxSize().background(Color.Black), Alignment.Center) {
                    // The fullscreen view takes the share while it is open.
                    if (model.fullscreenShare != person.user) {
                        ShareSurface(model, person.user, Modifier.aspectRatio(share.aspect))
                    }
                }
                Row(Modifier.align(Alignment.TopEnd).padding(8.dp), Arrangement.spacedBy(8.dp)) {
                    FilledTonalIconButton({ model.fullscreenShare = person.user }) {
                        Icon(Icons.Default.Fullscreen, "Fullscreen")
                    }
                    FilledTonalIconButton({ model.core.command(Command.Unwatch(person.user)) }) {
                        Icon(Icons.Default.Close, "Stop Watching")
                    }
                }
            } else {
                Column(
                    Modifier.align(Alignment.Center).padding(16.dp),
                    Arrangement.spacedBy(8.dp),
                    Alignment.CenterHorizontally,
                ) {
                    Icon(Icons.AutoMirrored.Filled.ScreenShare, null, Modifier.size(32.dp))
                    Text(
                        "${person.name} is sharing",
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Text(share.label, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    FilledTonalButton({ model.watch(person.user) }) { Text("Watch") }
                }
            }
        }
    }
}

/**
 * The share `model.fullscreenShare` over the whole window, in its shape, turning with the phone. A tap
 * shows the controls; everyone in the room stays shown along the bottom.
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
    BackHandler { model.fullscreenShare = null }
    Immersive()
    var controls by remember { mutableStateOf(true) }
    Box(
        Modifier
            .fillMaxSize()
            .background(Color.Black)
            .clickable(remember { MutableInteractionSource() }, null) { controls = !controls },
    ) {
        Box(Modifier.fillMaxSize(), Alignment.Center) {
            ShareSurface(model, user, Modifier.aspectRatio(share.aspect))
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
        RoomStrip(model, state.people.filter { it.room == room }, Modifier.align(Alignment.BottomCenter))
    }
}

/** Everyone in the room, small, so nobody listens unseen while a share fills the window. */
@Composable
private fun RoomStrip(model: AppModel, people: List<Person>, modifier: Modifier) {
    val scheme = MaterialTheme.colorScheme
    Row(
        modifier.safeDrawingPadding().padding(8.dp)
            .background(scheme.surfaceContainer.copy(alpha = 0.8f), MaterialTheme.shapes.extraLarge)
            .padding(horizontal = 8.dp, vertical = 4.dp),
        Arrangement.spacedBy(4.dp),
    ) {
        for (person in people) {
            val ring = if (person.speaking) scheme.tertiary else Color.Transparent
            PersonPicture(model, person, 28.dp, Modifier.border(2.dp, ring, CircleShape).padding(3.dp))
        }
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
