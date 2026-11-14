package cc.thro.throcc

import android.app.Activity
import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import cc.thro.throcc.core.Command

class MainActivity : ComponentActivity() {
    private lateinit var model: AppModel

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        uprightOnPhones()
        model = (application as ThroccApp).open(intent)
        setContent { App(model) }
    }

    override fun onPictureInPictureModeChanged(inPictureInPicture: Boolean, newConfig: Configuration) {
        super.onPictureInPictureModeChanged(inPictureInPicture, newConfig)
        model.pictureInPicture = inPictureInPicture
        if (!inPictureInPicture) model.fullscreenShare = model.fullscreenBeforePictureInPicture
    }

    override fun onStart() {
        super.onStart()
        model.core.command(Command.Reconnect)
        (application as ThroccApp).startVoice()
    }

    /** Closing the app, from the recent apps or the Picture in Picture window, ends the connection. */
    override fun onDestroy() {
        super.onDestroy()
        if (isFinishing) model.disconnect()
    }
}

/** Keeps a phone's window upright; a tablet's turns freely. */
fun Activity.uprightOnPhones() {
    requestedOrientation = if (resources.configuration.smallestScreenWidthDp < 600) {
        ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
    } else {
        ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
    }
}
