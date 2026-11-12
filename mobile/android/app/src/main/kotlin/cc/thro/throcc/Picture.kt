package cc.thro.throcc

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.graphics.Rect
import android.net.Uri
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import cc.thro.throcc.core.limits
import java.io.ByteArrayOutputStream
import java.io.IOException
import kotlin.math.roundToInt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

private const val PICTURE_SIDE = 256
private const val MAX_SHOWN_SIDE = 512

/** A picture file with the image it decodes to, which is null for a file that can't be shown. */
class Picture(val file: ByteArray) {
    val image = file.toImage()
}

/** The pictures in `files` by the hex of their key, taken from `kept` where it holds the same file. */
fun decoded(files: List<Pair<ByteArray, ByteArray?>>, kept: Map<String, Picture>): Map<String, Picture> =
    files.mapNotNull { (key, file) ->
        val name = key.toHexString()
        file?.let { name to (kept[name]?.takeIf { old -> old.file.contentEquals(it) } ?: Picture(it)) }
    }.toMap()

/** The image in this file at no more than 256 px a side, or null where it is over 512 px or unreadable. */
fun ByteArray.toImage(): ImageBitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(this, 0, size, bounds)
    val side = maxOf(bounds.outWidth, bounds.outHeight)
    if (minOf(bounds.outWidth, bounds.outHeight) < 1 || side > MAX_SHOWN_SIDE) return null
    val shrunk = BitmapFactory.Options().apply {
        inSampleSize = if (side > PICTURE_SIDE) MAX_SHOWN_SIDE / PICTURE_SIDE else 1
    }
    return BitmapFactory.decodeByteArray(this, 0, size, shrunk)?.asImageBitmap()
}

/** The middle square of the image at `uri` as a 256 px JPEG of at most 64 KiB, or null if it can't be read. */
suspend fun squarePicture(context: Context, uri: Uri): ByteArray? = withContext(Dispatchers.IO) {
    val square = try {
        ImageDecoder.decodeBitmap(ImageDecoder.createSource(context.contentResolver, uri)) { decoder, info, _ ->
            val scale = PICTURE_SIDE.toFloat() / minOf(info.size.width, info.size.height)
            val width = maxOf(PICTURE_SIDE, (info.size.width * scale).roundToInt())
            val height = maxOf(PICTURE_SIDE, (info.size.height * scale).roundToInt())
            val left = (width - PICTURE_SIDE) / 2
            val top = (height - PICTURE_SIDE) / 2
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            decoder.setTargetSize(width, height)
            decoder.crop = Rect(left, top, left + PICTURE_SIDE, top + PICTURE_SIDE)
        }
    } catch (_: IOException) {
        return@withContext null
    }
    (90 downTo 30 step 10).firstNotNullOfOrNull { quality ->
        val jpeg = ByteArrayOutputStream()
        square.compress(Bitmap.CompressFormat.JPEG, quality, jpeg)
        jpeg.toByteArray().takeIf { it.size <= limits().pictureBytes.toInt() }
    }
}
