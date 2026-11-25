package cc.thro.throcc

import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.projection.MediaProjection
import android.os.Handler
import android.os.Looper
import android.view.Display
import android.view.Surface
import cc.thro.throcc.core.ScreenSource
import kotlin.concurrent.thread

/** The native side of a screen share. */
object ScreenShare {
    init {
        System.loadLibrary("throcc_mobile")
    }

    /** A `Surface` for the `ANativeWindow` at `window`. */
    external fun surface(window: Long): Surface

    /** The shared screen turned to `width`×`height` pixels. */
    external fun turned(width: Int, height: Int)

    /** 20 ms of the screen's sound, interleaved stereo, whose first frame was recorded at `at` on [System.nanoTime]'s clock. */
    external fun pushSound(samples: FloatArray, at: Long)
}

private const val SAMPLE_RATE = 48_000
private const val CHUNK_FRAMES = SAMPLE_RATE / 50

/** The screen the person agreed to share, `width`×`height` pixels as `displays` shows it now, drawn into the
 *  share's encoder and followed as it turns, with its sound when `sound` records it. */
class SharedScreen(
    private val projection: MediaProjection,
    private val dpi: Int,
    private val sound: AudioRecord?,
    width: Int,
    height: Int,
    private val displays: DisplayManager,
) : ScreenSource {
    private var display: VirtualDisplay? = null
    private var surface: Surface? = null
    private var recording: Thread? = null

    @Volatile
    private var stopped = false

    /** The screen's size and rotation when the share started. */
    private val upright = width to height
    private val uprightRotation = rotation()

    private val turns = object : DisplayManager.DisplayListener {
        override fun onDisplayChanged(id: Int) {
            if (id != Display.DEFAULT_DISPLAY) return
            val sideways = (rotation() - uprightRotation) % 2 != 0
            val (w, h) = if (sideways) upright.second to upright.first else upright
            ScreenShare.turned(w, h)
        }

        override fun onDisplayAdded(id: Int) = Unit

        override fun onDisplayRemoved(id: Int) = Unit
    }

    init {
        displays.registerDisplayListener(turns, Handler(Looper.getMainLooper()))
    }

    private fun rotation() = displays.getDisplay(Display.DEFAULT_DISPLAY)?.rotation ?: 0

    @Synchronized
    override fun show(window: ULong, width: UInt, height: UInt): Boolean = try {
        val drawn = ScreenShare.surface(window.toLong())
        val shown = display
        if (shown == null) {
            display = projection.createVirtualDisplay(
                "throcc",
                width.toInt(),
                height.toInt(),
                dpi,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                drawn,
                null,
                null,
            )
            sound?.let { recording = record(it) }
        } else {
            shown.surface = drawn
            shown.resize(width.toInt(), height.toInt(), dpi)
        }
        surface?.release()
        surface = drawn
        true
    } catch (_: RuntimeException) {
        false
    }

    @Synchronized
    override fun stop() {
        if (stopped) return
        stopped = true
        displays.unregisterDisplayListener(turns)
        sound?.stop()
        recording?.join()
        sound?.release()
        display?.release()
        surface?.release()
        projection.stop()
    }

    /** Reads the sound in 20 ms chunks until stopped, each stamped with when its first frame was recorded. */
    private fun record(sound: AudioRecord) = thread(name = "throcc-share-sound") {
        val chunk = FloatArray(CHUNK_FRAMES * 2)
        val stamp = AudioTimestamp()
        var frames = 0L
        sound.startRecording()
        while (!stopped) {
            val read = sound.read(chunk, 0, chunk.size, AudioRecord.READ_BLOCKING)
            if (read < 0) break
            if (read != chunk.size) continue
            val at = if (sound.getTimestamp(stamp, AudioTimestamp.TIMEBASE_MONOTONIC) == AudioRecord.SUCCESS) {
                stamp.nanoTime + (frames - stamp.framePosition) * 1_000_000_000L / SAMPLE_RATE
            } else {
                System.nanoTime() - 20_000_000L
            }
            ScreenShare.pushSound(chunk, at)
            frames += CHUNK_FRAMES
        }
    }
}
