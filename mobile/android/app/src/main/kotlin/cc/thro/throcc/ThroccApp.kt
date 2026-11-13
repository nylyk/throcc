package cc.thro.throcc

import android.app.Application
import android.app.ForegroundServiceStartNotAllowedException
import android.content.Intent
import android.content.pm.ApplicationInfo
import cc.thro.throcc.core.Core
import cc.thro.throcc.core.setAppDirs
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

class ThroccApp : Application() {
    /** The model every Activity of this process shows. It outlives them, and so does its core. */
    var model: AppModel? = null
        private set

    override fun onCreate() {
        super.onCreate()
        // A restored backup would roll the epoch floors back under the same identity key.
        setAppDirs(noBackupFilesDir.path, cacheDir.path)
    }

    /** The model, which the first `launch` intent of the process opens over the core, or in a debuggable build
     *  with the `mock` extra over the sample one at the page `screen` names. */
    fun open(launch: Intent): AppModel {
        model?.let { return it }
        val debuggable = applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0
        val mock = debuggable && launch.getBooleanExtra("mock", false)
        val sample = launch.getStringExtra("screen").takeIf { mock }
        val opened = AppModel(if (mock) Core.mock(onboarded = sample != "onboarding") else Core(), mock, sample)
        model = opened
        MainScope().launch {
            opened.state.map { it.liveRoom != null }.distinctUntilChanged().collect { startVoice() }
        }
        return opened
    }

    /** The service of the room we are in starts. One that runs looks at the mic permission again. */
    fun startVoice() {
        val model = model ?: return
        if (model.mock || model.state.value.liveRoom == null) return
        try {
            startForegroundService(Intent(this, VoiceService::class.java))
        } catch (_: ForegroundServiceStartNotAllowedException) {
            // The system refuses the start while the app is off screen.
        }
    }
}
