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

/** The kinds of device the room's sound goes to, the most wanted first. */
private val WANTED_DEVICES = listOf(
    AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
    AudioDeviceInfo.TYPE_BLE_HEADSET,
    AudioDeviceInfo.TYPE_WIRED_HEADSET,
    AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
    AudioDeviceInfo.TYPE_USB_HEADSET,
    AudioDeviceInfo.TYPE_BUILTIN_SPEAKER,
)

/** The name of the room `state` has us in and of its server, or null outside a room. */
private fun described(state: CoreState): Pair<String, String>? = state.voiceRoom()?.let { room ->
    state.rooms.find { it.id == room }?.name.orEmpty() to state.server?.let { it.name ?: it.address }.orEmpty()
}

/** The service that runs the sound of the room we are in, also while the app is off screen. */
class VoiceService : Service() {
    private val scope = MainScope()
    private val audio by lazy { getSystemService(AudioManager::class.java) }
    private val notifications by lazy { getSystemService(NotificationManager::class.java) }
    private lateinit var model: AppModel

    /** Whether the sound is open with the mic, or null until the service has set up the room's audio. */
    private var withMic: Boolean? = null

    /** Whether something else holds the audio for a while, as a phone call does. */
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
                AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> {
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
    private val routed = AudioManager.OnCommunicationDeviceChangedListener { open() }

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
        val mic = checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
        if (!promote(notification(described(model.state.value)), mic)) {
            if (withMic == null) stopSelf()
            return START_NOT_STICKY
        }
        if (withMic == null) setUp(model)
        if (withMic != mic) {
            withMic = mic
            model.micDenied = !mic
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
        audio.mode = AudioManager.MODE_IN_COMMUNICATION
        audio.requestAudioFocus(focus)
        audio.addOnCommunicationDeviceChangedListener(mainExecutor, routed)
        audio.registerAudioDeviceCallback(devices, null)
        route()
        scope.launch {
            model.state.map(::described).distinctUntilChanged().collect { room ->
                if (room == null) stopSelf() else notifications.notify(NOTIFICATION, notification(room))
            }
        }
    }

    /** The room's sound goes to a Bluetooth headset, else to a wired or USB one, else to the speaker. */
    private fun route() {
        val available = audio.availableCommunicationDevices
        WANTED_DEVICES.firstNotNullOfOrNull { type -> available.find { it.type == type } }
            ?.let(audio::setCommunicationDevice)
    }

    /** The room's sound opens anew on the device in use, unless a phone call holds the audio. */
    private fun open() {
        if (!interrupted) model.openAudio(withMic == true)
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
