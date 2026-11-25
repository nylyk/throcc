package cc.thro.throcc

import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.projection.MediaProjection
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

    /** 20 ms of the screen's sound, interleaved stereo, whose first frame was recorded at `at` on [System.nanoTime]'s clock. */
    external fun pushSound(samples: FloatArray, at: Long)
}

private const val SAMPLE_RATE = 48_000
private const val CHUNK_FRAMES = SAMPLE_RATE / 50

/** The screen the person agreed to share, drawn into the share's encoder, with its sound when `sound` records it. */
class SharedScreen(
    private val projection: MediaProjection,
    private val dpi: Int,
    private val sound: AudioRecord?,
) : ScreenSource {
    private var display: VirtualDisplay? = null
    private var recording: Thread? = null

    @Volatile
    private var stopped = false

    @Synchronized
    override fun show(window: ULong, width: UInt, height: UInt): Boolean = try {
        display = projection.createVirtualDisplay(
            "throcc",
            width.toInt(),
            height.toInt(),
            dpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            ScreenShare.surface(window.toLong()),
            null,
            null,
        )
        sound?.let { recording = record(it) }
        true
    } catch (_: RuntimeException) {
        false
    }

    @Synchronized
    override fun stop() {
        if (stopped) return
        stopped = true
        sound?.stop()
        recording?.join()
        sound?.release()
        display?.release()
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
