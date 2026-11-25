package cc.thro.throcc

import android.Manifest
import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.hardware.display.DisplayManager
import android.media.AudioAttributes
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Handler
import android.os.IBinder
import android.os.Process
import androidx.activity.result.ActivityResult
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.IntentCompat
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.CoreState
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

private const val CHANNEL = "room"
private const val NOTIFICATION = 1
private const val LEAVE = "cc.thro.throcc.LEAVE"
private const val SHARE = "cc.thro.throcc.SHARE"

/** Has the room's service share the screen the person agreed to share with `consent`, `width`×`height` pixels at
 *  `dpi`, with its sound if `audio`. */
fun shareScreen(context: Context, consent: ActivityResult, width: Int, height: Int, dpi: Int, audio: Boolean) {
    val share = Intent(context, VoiceService::class.java)
        .setAction(SHARE)
        .putExtra("code", consent.resultCode)
        .putExtra("consent", consent.data)
        .putExtra("width", width)
        .putExtra("height", height)
        .putExtra("dpi", dpi)
        .putExtra("audio", audio)
    context.startForegroundService(share)
}

/** The name of the room `state` has us in and of its server, or null outside a room. */
private fun described(state: CoreState): Pair<String, String>? = state.liveRoom?.let { room ->
    state.rooms.find { it.id == room }?.name.orEmpty() to state.server?.title.orEmpty()
}

/** The service that runs the sound of the room we are in, also while the app is off screen. */
class VoiceService : Service() {
    private val scope = MainScope()
    private val audio by lazy { getSystemService(AudioManager::class.java) }
    private val notifications by lazy { getSystemService(NotificationManager::class.java) }
    private lateinit var model: AppModel

    /** Whether the sound is open with the mic, or null until the service has set up the room's audio. */
    private var withMic: Boolean? = null

    /** Whether something else holds the audio, for a while as a phone call does, or for good. */
    private var interrupted = false

    /** Whether the service holds a screen the person agreed to share. */
    private var projecting = false

    private val focus = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
        .setAudioAttributes(
            AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                .build(),
        )
        .setOnAudioFocusChangeListener { change ->
            when (change) {
                AudioManager.AUDIOFOCUS_LOSS, AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> {
                    interrupted = true
                    model.closeAudio()
                }
                AudioManager.AUDIOFOCUS_GAIN -> if (interrupted) {
                    interrupted = false
                    open()
                }
            }
        }
        .build()

    // AAudio streams stay on the device they were opened on.
    private val devices = object : AudioDeviceCallback() {
        override fun onAudioDevicesAdded(added: Array<AudioDeviceInfo>) = open()

        override fun onAudioDevicesRemoved(removed: Array<AudioDeviceInfo>) = open()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        notifications.createNotificationChannel(NotificationChannel(CHANNEL, "Room", NotificationManager.IMPORTANCE_LOW))
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val model = (application as ThroccApp).model
        if (model == null || intent?.action == LEAVE) {
            model?.core?.command(Command.Join(null))
            if (withMic == null) stopSelf()
            return START_NOT_STICKY
        }
        if (intent?.action == SHARE) {
            share(model, intent)
            return START_NOT_STICKY
        }
        val granted = checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
        val notification = notification(described(model.state.value))
        // The system refuses the microphone type to a service started while the app is off screen.
        val mic = when {
            promote(notification, granted) -> granted
            granted && promote(notification, false) -> false
            else -> {
                if (withMic == null) stopSelf()
                return START_NOT_STICKY
            }
        }
        if (withMic == null) setUp(model) else if (interrupted) regainFocus()
        if (withMic != mic) {
            withMic = mic
            model.micDenied = !granted
            if (!mic) model.core.command(Command.SetMic(false))
            open()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        if (withMic == null) return
        audio.unregisterAudioDeviceCallback(devices)
        model.closeAudio()
        audio.abandonAudioFocusRequest(focus)
    }

    /** Whether the system let the service into the foreground, for the mic too where `mic` is granted. */
    private fun promote(notification: Notification, mic: Boolean): Boolean {
        val types = ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK or
            (if (mic) ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE else 0) or
            if (projecting) ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION else 0
        return try {
            ServiceCompat.startForeground(this, NOTIFICATION, notification, types)
            true
        } catch (_: IllegalStateException) {
            false
        } catch (_: SecurityException) {
            false
        }
    }

    /** Takes the screen the `share` intent's consent gives, which the system only hands to a service in the
     *  foreground for it, and shares it. */
    private fun share(model: AppModel, share: Intent) {
        val withMic = withMic ?: return
        val consent = IntentCompat.getParcelableExtra(share, "consent", Intent::class.java) ?: return
        projecting = true
        val projection = if (promote(notification(described(model.state.value)), withMic)) {
            getSystemService(MediaProjectionManager::class.java).getMediaProjection(share.getIntExtra("code", 0), consent)
        } else {
            null
        }
        if (projection == null) {
            stopProjecting()
            return
        }
        projection.registerCallback(
            object : MediaProjection.Callback() {
                override fun onStop() {
                    stopProjecting()
                    model.core.command(Command.StopShare)
                }
            },
            Handler(mainLooper),
        )
        val sound = if (share.getBooleanExtra("audio", false)) record(projection) else null
        val (width, height) = share.getIntExtra("width", 0) to share.getIntExtra("height", 0)
        val displays = getSystemService(DisplayManager::class.java)
        val screen = SharedScreen(projection, share.getIntExtra("dpi", 0), sound, width, height, displays)
        model.shareScreen(screen, width, height, sound != null)
    }

    private fun stopProjecting() {
        projecting = false
        withMic?.let { promote(notification(described(model.state.value)), it) }
    }

    /** A recorder of what other apps play on `projection`'s screen, never our own sound, or null where the
     *  microphone permission it needs is missing. */
    @SuppressLint("MissingPermission")
    private fun record(projection: MediaProjection): AudioRecord? {
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) return null
        val played = AudioPlaybackCaptureConfiguration.Builder(projection)
            .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
            .addMatchingUsage(AudioAttributes.USAGE_GAME)
            .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
            .excludeUid(Process.myUid())
            .build()
        val format = AudioFormat.Builder()
            .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
            .setSampleRate(48_000)
            .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
            .build()
        return try {
            AudioRecord.Builder().setAudioFormat(format).setAudioPlaybackCaptureConfig(played).build()
        } catch (_: UnsupportedOperationException) {
            null
        }
    }

    /** The service takes the phone's audio for the room, and follows the room and the audio devices. */
    private fun setUp(model: AppModel) {
        this.model = model
        interrupted = audio.requestAudioFocus(focus) != AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        audio.registerAudioDeviceCallback(devices, null)
        scope.launch {
            model.state.map(::described).distinctUntilChanged().collect { room ->
                if (room == null) stopSelf() else notifications.notify(NOTIFICATION, notification(room))
            }
        }
    }

    /** Asks for the audio again, as the app comes to the front after another app took it for good. */
    private fun regainFocus() {
        if (audio.requestAudioFocus(focus) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
            interrupted = false
            open()
        }
    }

    /** The room's sound opens anew as media, whose volume goes down to silence, on the device the system picks,
     *  once set up and unless a phone call holds the audio. */
    private fun open() {
        val withMic = withMic ?: return
        if (!interrupted) model.openAudio(withMic)
    }

    private fun notification(room: Pair<String, String>?): Notification {
        val app = packageManager.getLaunchIntentForPackage(packageName)
        val leave = Intent(this, VoiceService::class.java).setAction(LEAVE)
        return NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.room)
            .setContentTitle(room?.first)
            .setContentText(room?.second)
            .setOngoing(true)
            .setContentIntent(app?.let { PendingIntent.getActivity(this, 0, it, PendingIntent.FLAG_IMMUTABLE) })
            .addAction(0, "Leave", PendingIntent.getService(this, 0, leave, PendingIntent.FLAG_IMMUTABLE))
            .build()
    }
}
