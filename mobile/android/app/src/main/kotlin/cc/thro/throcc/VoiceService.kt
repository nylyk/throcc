package cc.thro.throcc

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.AudioAttributes
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
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

/** The Bluetooth headsets whose mic only a call reaches, the most wanted first. */
private val CALL_HEADSETS = listOf(AudioDeviceInfo.TYPE_BLUETOOTH_SCO, AudioDeviceInfo.TYPE_BLE_HEADSET)

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

    /** Whether the sound plays as a call, to a Bluetooth headset, rather than as media. */
    private var asCall = false

    /** Whether something else holds the audio, for a while as a phone call does, or for good. */
    private var interrupted = false

    private val focus = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
        .setAudioAttributes(
            AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
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

    private val devices = object : AudioDeviceCallback() {
        override fun onAudioDevicesAdded(added: Array<AudioDeviceInfo>) = route()

        override fun onAudioDevicesRemoved(removed: Array<AudioDeviceInfo>) = route()
    }

    // AAudio streams stay on the device they were opened on, and the system applies a picked device later.
    private val routed = AudioManager.OnCommunicationDeviceChangedListener { if (asCall) open() }

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
        audio.removeOnCommunicationDeviceChangedListener(routed)
        model.closeAudio()
        audio.clearCommunicationDevice()
        audio.abandonAudioFocusRequest(focus)
        audio.mode = AudioManager.MODE_NORMAL
    }

    /** Whether the system let the service into the foreground, for the mic too where `mic` is granted. */
    private fun promote(notification: Notification, mic: Boolean): Boolean {
        val types = ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK or
            if (mic) ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE else 0
        return try {
            ServiceCompat.startForeground(this, NOTIFICATION, notification, types)
            true
        } catch (_: IllegalStateException) {
            false
        } catch (_: SecurityException) {
            false
        }
    }

    /** The service takes the phone's audio for the room, and follows the room and the audio devices. */
    private fun setUp(model: AppModel) {
        this.model = model
        interrupted = audio.requestAudioFocus(focus) != AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        audio.addOnCommunicationDeviceChangedListener(mainExecutor, routed)
        audio.registerAudioDeviceCallback(devices, null)
        route()
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

    /** The room's sound plays as a call to a Bluetooth headset, for its mic. Without one it plays as media, whose
     *  volume goes down to silence, on the device the system picks. */
    private fun route() {
        val available = audio.availableCommunicationDevices
        val headset = CALL_HEADSETS.firstNotNullOfOrNull { type -> available.find { it.type == type } }
        asCall = headset != null
        if (headset != null) {
            audio.mode = AudioManager.MODE_IN_COMMUNICATION
            audio.setCommunicationDevice(headset)
        } else {
            audio.clearCommunicationDevice()
            audio.mode = AudioManager.MODE_NORMAL
            open()
        }
    }

    /** The room's sound opens anew on the device in use, once set up and unless a phone call holds the audio. */
    private fun open() {
        val withMic = withMic ?: return
        if (!interrupted) model.openAudio(withMic, asCall)
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
