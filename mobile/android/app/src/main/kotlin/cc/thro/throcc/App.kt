package cc.thro.throcc

import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.adaptive.currentWindowAdaptiveInfoV2
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation3.runtime.NavEntry
import androidx.navigation3.ui.NavDisplay
import androidx.window.core.layout.WindowSizeClass
import cc.thro.throcc.core.CoreState

@Composable
fun App(model: AppModel) {
    val state by model.state.collectAsStateWithLifecycle()
    LaunchedEffect(model) {
        for (notice in model.notices) model.snackbars.showSnackbar(notice)
    }
    val context = LocalContext.current
    val request = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
        model.answered()
    }
    LaunchedEffect(model) {
        for (permissions in model.asks) {
            val missing = permissions.filter { context.checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED }
            if (missing.isEmpty()) model.answered() else request.launch(missing.toTypedArray())
        }
    }
    ThroccTheme(model.theme.palette) {
        Surface(Modifier.fillMaxSize()) {
            if (model.profile == null) Onboarding(model) else Panes(model, state)
        }
        state.people.find { it.user == model.sheetPerson }?.let { PersonSheet(model, state, it) }
    }
}

/** The pages beside our room in a wide window. A narrow one shows the room as the page in front. */
@Composable
private fun Panes(model: AppModel, state: CoreState) {
    val wide = currentWindowAdaptiveInfoV2().windowSizeClass
        .isWidthAtLeastBreakpoint(WindowSizeClass.WIDTH_DP_EXPANDED_LOWER_BOUND)
    val roomInFront = !wide && model.roomInFront && state.current != null
    val pagesEnd = WindowInsets.safeDrawing.only(WindowInsetsSides.End)
    val roomStart = WindowInsets.safeDrawing.only(WindowInsetsSides.Start)
    Row(Modifier.fillMaxSize()) {
        val scheme = MaterialTheme.colorScheme
        MaterialTheme(colorScheme = if (wide) raised(scheme) else scheme) {
            NavDisplay(
                backStack = if (roomInFront) model.stack + Screen.Room else model.stack,
                modifier = if (wide) Modifier.width(360.dp).consumeWindowInsets(pagesEnd) else Modifier.weight(1f),
                onBack = { if (roomInFront) model.roomInFront = false else model.back() },
                // NavDisplay keeps an entry until the stack changes, so an entry that captured the
                // state would show a stale one.
                entryProvider = { screen -> NavEntry(screen) { Shown(model, screen) } },
            )
        }
        if (wide) Box(Modifier.weight(1f).consumeWindowInsets(roomStart)) { RoomPane(model, state, null) }
    }
}

@Composable
private fun Shown(model: AppModel, screen: Screen) {
    val state by model.state.collectAsStateWithLifecycle()
    when (screen) {
        Screen.Servers -> Servers(model, state)
        Screen.AddServer -> AddServer(model)
        Screen.Rooms -> Rooms(model, state)
        Screen.Room -> RoomPane(model, state) { model.roomInFront = false }
        Screen.Settings -> Settings(model)
        Screen.Profile -> Profile(model)
        Screen.Voice -> VoiceSettings(model)
        Screen.Appearance -> Appearance(model)
        Screen.Verified -> VerifiedPeople(model, state)
        Screen.Blocked -> BlockedPeople(model, state)
        Screen.About -> About(model)
        Screen.Server -> ServerOverview(model, state)
        Screen.Members -> Members(model, state)
        Screen.Invites -> Invites(model, state)
        Screen.Keys -> Keys(model, state)
    }
}
