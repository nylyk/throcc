package cc.thro.throcc

import android.app.Application
import cc.thro.throcc.core.setAppDirs

class ThroccApp : Application() {
    override fun onCreate() {
        super.onCreate()
        // A restored backup would roll the epoch floors back under the same identity key.
        setAppDirs(noBackupFilesDir.path, cacheDir.path)
    }
}
